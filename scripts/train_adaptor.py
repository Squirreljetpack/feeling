#!/usr/bin/env python3
"""
Train a small adaptor head that maps a frozen sentence embedding of the
RAW input text (not a shift vector) to a numeric emotional-saliency score.

Reuses the same fine-tuned bge-small-en-v1.5 model as embed_regression.py,
so the adaptor's input space matches exactly what that script feeds it at
inference time (see embed_regression.py's `embed(text, model)` -- no
"Emotional state: " prefix, no base-vector subtraction).

Dataset format: CSV with a text column and a numeric label column, e.g.
    "add two tablespoons of sugar to the bowl",0
    "throat feels constricted",1
    "so happy I could weep",2

Usage:
    # plain linear probe (default -- best starting point for small datasets)
    python3 scripts/train_adaptor.py

    # try a deeper head and compare the CV metrics it prints against the
    # linear probe's, to see whether the extra layer actually helps
    python3 scripts/train_adaptor.py --hidden-layers "64" --output scripts/saliency_adaptor_h64.pt
    python3 scripts/train_adaptor.py --hidden-layers "128,64"

    # also export the trained adaptor to ONNX for the Rust runtime
    # (assets/model/saliency_adaptor.onnx, loaded by build.rs / src/embed.rs)
    python3 scripts/train_adaptor.py --export-onnx
"""

import argparse
import random
from pathlib import Path

import numpy as np
import torch
from adaptor import SaliencyAdaptor, save_adaptor
from dataset_loader import load_dataset
from onnxruntime import InferenceSession
from tokenizers import Tokenizer
from torch import nn

MAX_SEQ_LEN = 256
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MODEL_PATH = REPO_ROOT / "assets/model/bge_small.onnx"
DEFAULT_TOKENIZER_PATH = REPO_ROOT / "target/cache/tokenizer.json"
DEFAULT_TRAIN_DATASET = Path(__file__).resolve().parent / "train.csv"
DEFAULT_ONNX_OUTPUT = REPO_ROOT / "assets/model/saliency_adaptor.onnx"


def read_rows(dataset_path: Path) -> list[tuple[str, float]]:
    """Return (text, label) pairs from the dataset_loader (columns: input_text,label)."""
    rows = load_dataset(dataset_path)
    return [(r["input_text"], float(r["label"])) for r in rows]


def set_seed(seed: int) -> None:
    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)


def load_onnx_embedder(model_path: Path, tokenizer_path: Path) -> tuple[InferenceSession, Tokenizer]:
    session = InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
    tokenizer = Tokenizer.from_file(str(tokenizer_path))
    tokenizer.enable_truncation(max_length=MAX_SEQ_LEN)
    tokenizer.enable_padding(length=None)
    return session, tokenizer


def embed_raw(texts: list[str], session: InferenceSession, tokenizer: Tokenizer, batch_size: int = 32) -> np.ndarray:
    """
    Raw text embedding via the base bge_small.onnx asset: mean-pool the
    last hidden state over real (non-pad) tokens, then L2-normalize. No
    prefix, no shift subtraction -- exactly what embed_regression.py will
    feed the adaptor at inference time.
    """
    input_names = {i.name for i in session.get_inputs()}
    all_embeds = []

    for start in range(0, len(texts), batch_size):
        batch = texts[start : start + batch_size]
        encodings = tokenizer.encode_batch(batch)
        max_len = max(len(e.ids) for e in encodings)

        input_ids = np.zeros((len(batch), max_len), dtype=np.int64)
        attention_mask = np.zeros((len(batch), max_len), dtype=np.int64)
        for i, enc in enumerate(encodings):
            n = len(enc.ids)
            input_ids[i, :n] = enc.ids
            attention_mask[i, :n] = enc.attention_mask

        feed = {"input_ids": input_ids, "attention_mask": attention_mask}
        if "token_type_ids" in input_names:
            feed["token_type_ids"] = np.zeros_like(input_ids)

        outputs = session.run(None, feed)
        last_hidden = outputs[0]  # (batch, seq_len, hidden)

        mask = attention_mask[:, :, None].astype(np.float32)
        summed = (last_hidden * mask).sum(axis=1)
        counts = np.clip(mask.sum(axis=1), 1e-9, None)
        pooled = summed / counts

        norms = np.linalg.norm(pooled, axis=1, keepdims=True)
        pooled = pooled / np.clip(norms, 1e-9, None)
        all_embeds.append(pooled)

    return np.concatenate(all_embeds, axis=0)


def regression_metrics(y_true: np.ndarray, y_pred: np.ndarray) -> dict:
    y_true = np.asarray(y_true, dtype=float)
    y_pred = np.asarray(y_pred, dtype=float)
    mae = float(np.mean(np.abs(y_true - y_pred)))
    rmse = float(np.sqrt(np.mean((y_true - y_pred) ** 2)))
    ss_res = float(np.sum((y_true - y_pred) ** 2))
    ss_tot = float(np.sum((y_true - y_true.mean()) ** 2))
    r2 = 1 - ss_res / ss_tot if ss_tot > 0 else float("nan")
    if np.std(y_true) > 0 and np.std(y_pred) > 0:
        pearson = float(np.corrcoef(y_true, y_pred)[0, 1])
    else:
        pearson = float("nan")
    return {"mae": mae, "rmse": rmse, "r2": r2, "pearson_r": pearson}


def train_one_fold(
    X_train,
    y_train,
    X_val,
    y_val,
    hidden_layers,
    dropout,
    epochs,
    lr,
    weight_decay,
    patience,
    use_sigmoid: bool = True,
) -> tuple[SaliencyAdaptor, dict]:
    model = SaliencyAdaptor(
        input_dim=X_train.shape[1], hidden_layers=hidden_layers, dropout=dropout, use_sigmoid=use_sigmoid
    )
    opt = torch.optim.AdamW(model.parameters(), lr=lr, weight_decay=weight_decay)
    loss_fn = nn.MSELoss()

    Xtr = torch.as_tensor(X_train, dtype=torch.float32)
    ytr = torch.as_tensor(y_train, dtype=torch.float32)
    Xva = torch.as_tensor(X_val, dtype=torch.float32) if X_val is not None and len(X_val) > 0 else None
    yva = torch.as_tensor(y_val, dtype=torch.float32) if Xva is not None else None

    best_val = float("inf")
    best_state = None
    bad_epochs = 0

    for _ in range(epochs):
        model.train()
        opt.zero_grad()
        loss = loss_fn(model(Xtr), ytr)
        loss.backward()
        opt.step()

        if Xva is not None and yva is not None:
            model.eval()
            with torch.no_grad():
                val_loss = loss_fn(model(Xva), yva).item()
            if val_loss < best_val - 1e-6:
                best_val = val_loss
                best_state = {k: v.clone() for k, v in model.state_dict().items()}
                bad_epochs = 0
            else:
                bad_epochs += 1
                if bad_epochs >= patience:
                    break

    if best_state is not None:
        model.load_state_dict(best_state)

    metrics = {}
    if Xva is not None and yva is not None:
        model.eval()
        with torch.no_grad():
            pred_val = model(Xva).numpy()
        metrics = regression_metrics(y_val, pred_val)
    return model, metrics


def cross_validate(X, y, hidden_layers, dropout, epochs, lr, weight_decay, patience, k, seed, use_sigmoid=True) -> dict:
    n = len(y)
    k = max(2, min(k, n))
    rng = np.random.RandomState(seed)
    indices = rng.permutation(n)
    folds = np.array_split(indices, k)

    all_metrics = []
    for i in range(k):
        val_idx = folds[i]
        train_idx = np.concatenate([folds[j] for j in range(k) if j != i])
        if len(val_idx) == 0 or len(train_idx) == 0:
            continue
        _, metrics = train_one_fold(
            X[train_idx],
            y[train_idx],
            X[val_idx],
            y[val_idx],
            hidden_layers,
            dropout,
            epochs,
            lr,
            weight_decay,
            patience,
            use_sigmoid=use_sigmoid,
        )
        if metrics:
            all_metrics.append(metrics)

    if not all_metrics:
        return {}
    return {
        key: (float(np.mean([m[key] for m in all_metrics])), float(np.std([m[key] for m in all_metrics])))
        for key in all_metrics[0]
    }


def parse_hidden_layers(spec: str) -> tuple[int, ...]:
    spec = spec.strip()
    if not spec:
        return ()
    return tuple(int(x) for x in spec.split(",") if x.strip())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--model-path", help=f"path to bge_small.onnx (default: {DEFAULT_MODEL_PATH})")
    parser.add_argument("--tokenizer-path", help=f"path to tokenizer.json (default: {DEFAULT_TOKENIZER_PATH})")
    parser.add_argument("--dataset", help=f"path to labeled dataset CSV (default: {DEFAULT_TRAIN_DATASET})")
    parser.add_argument(
        "--val-dataset",
        default=None,
        help="optional held-out labeled CSV (e.g. validation.csv) to evaluate the final adaptor on",
    )
    parser.add_argument(
        "--hidden-layers",
        default="",
        help='comma-separated hidden layer sizes, e.g. "128,64". Empty = plain linear probe.',
    )
    parser.add_argument("--dropout", type=float, default=0.1)
    parser.add_argument("--epochs", type=int, default=300)
    parser.add_argument("--lr", type=float, default=1e-2)
    parser.add_argument("--weight-decay", type=float, default=1e-3)
    parser.add_argument(
        "--patience", type=int, default=30, help="early-stopping patience, in epochs without val improvement"
    )
    parser.add_argument("--cv-folds", type=int, default=5, help="folds used only to report generalization metrics")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--output", default=None, help="output .pt path (default: scripts/saliency_adaptor.pt)")
    parser.add_argument("--no-sigmoid", action="store_true", help="Disable sigmoid output bounding")
    parser.add_argument(
        "--export-onnx",
        action="store_true",
        help="also export the trained adaptor to ONNX for the Rust runtime",
    )
    parser.add_argument(
        "--onnx-output",
        default=str(DEFAULT_ONNX_OUTPUT),
        help=f"ONNX output path (default: {DEFAULT_ONNX_OUTPUT})",
    )
    args = parser.parse_args()

    set_seed(args.seed)

    model_path = Path(args.model_path) if args.model_path else DEFAULT_MODEL_PATH
    tokenizer_path = Path(args.tokenizer_path) if args.tokenizer_path else DEFAULT_TOKENIZER_PATH
    dataset_path = Path(args.dataset) if args.dataset else DEFAULT_TRAIN_DATASET
    output_path = Path(args.output) if args.output else Path(__file__).resolve().parent / "saliency_adaptor.pt"
    hidden_layers = parse_hidden_layers(args.hidden_layers)
    use_sigmoid = not args.no_sigmoid

    print(f"Loading base embedding model from '{model_path}'...")
    session, tokenizer = load_onnx_embedder(model_path, tokenizer_path)

    print(f"Reading labeled dataset from '{dataset_path}'...")
    rows = read_rows(dataset_path)
    texts = [t for t, _ in rows]
    raw_labels = np.array([l for _, l in rows], dtype=float)
    max_label = float(raw_labels.max()) if raw_labels.max() > 0 else 1.0
    labels = raw_labels / max_label
    print(
        f"Loaded {len(texts)} rows. Raw label range: [{raw_labels.min():.2f}, {raw_labels.max():.2f}] -> Normalized: [{labels.min():.2f}, {labels.max():.2f}]"
    )

    print("Embedding raw text (no prefix, no shift subtraction)...")
    X = embed_raw(texts, session, tokenizer)

    # Strategy 1 Baseline: Neutrality Centroid Distance
    neutral_mask = raw_labels == 0
    if np.any(neutral_mask):
        neutral_centroid = X[neutral_mask].mean(axis=0)
        norm = np.linalg.norm(neutral_centroid)
        if norm > 0:
            neutral_centroid /= norm
        distances = 1.0 - np.dot(X, neutral_centroid)
        baseline_metrics = regression_metrics(labels, distances / (distances.max() if distances.max() > 0 else 1.0))
        print("\nUnsupervised Neutrality Centroid Baseline (Strategy 1):")
        print(f"  Pearson r : {baseline_metrics['pearson_r']:+.4f}")
        print(f"  MAE       : {baseline_metrics['mae']:.4f}")

    arch_str = " -> ".join(str(h) for h in hidden_layers)
    arch_str = f"input({X.shape[1]}) -> {arch_str + ' -> ' if arch_str else ''}1 (Sigmoid={use_sigmoid})"
    print(f"\nAdaptor Architecture: {arch_str}")
    print(f"{args.cv_folds}-fold CV (to gauge whether this architecture generalizes)...")
    cv_metrics = cross_validate(
        X,
        labels,
        hidden_layers,
        args.dropout,
        args.epochs,
        args.lr,
        args.weight_decay,
        args.patience,
        args.cv_folds,
        args.seed,
        use_sigmoid=use_sigmoid,
    )
    if cv_metrics:
        print("CV results (mean +/- std over folds):")
        for key, (mean, std) in cv_metrics.items():
            print(f"  {key:>10}: {mean:+.4f} +/- {std:.4f}")
    else:
        print("  (too few rows to compute held-out metrics; skipping CV report)")

    print("\nFitting final adaptor on the full dataset...")
    final_model, _ = train_one_fold(
        X,
        labels,
        None,
        None,
        hidden_layers,
        args.dropout,
        args.epochs,
        args.lr,
        args.weight_decay,
        args.patience,
        use_sigmoid=use_sigmoid,
    )

    held_out_metrics: dict | None = None
    if args.val_dataset:
        val_dataset_path = Path(args.val_dataset)
        val_rows = read_rows(val_dataset_path)
        val_texts = [t for t, _ in val_rows]
        val_labels = np.array([lab for _, lab in val_rows], dtype=float) / max_label
        print(f"\nEvaluating final adaptor on held-out '{val_dataset_path.name}' ({len(val_texts)} rows)...")
        X_val = embed_raw(val_texts, session, tokenizer)
        final_model.eval()
        with torch.no_grad():
            pred_val = final_model(torch.as_tensor(X_val, dtype=torch.float32)).numpy()
        held_out_metrics = regression_metrics(val_labels, pred_val)
        print("Held-out metrics:")
        for key, value in held_out_metrics.items():
            print(f"  {key:>10}: {value:+.4f}")

    save_adaptor(
        final_model,
        output_path,
        extra={
            "cv_metrics": cv_metrics,
            "held_out_metrics": held_out_metrics,
            "hidden_layers": hidden_layers,
            "n_train": len(texts),
            "max_label": max_label,
            "use_sigmoid": use_sigmoid,
        },
    )
    print(f"Saved adaptor to '{output_path}'")

    if args.export_onnx:
        onnx_path = Path(args.onnx_output)
        onnx_path.parent.mkdir(parents=True, exist_ok=True)
        dummy_input = torch.randn(1, final_model.input_dim)
        print(f"Exporting adaptor to ONNX at '{onnx_path}'...")
        torch.onnx.export(
            final_model,
            (dummy_input,),
            str(onnx_path),
            input_names=["input"],
            output_names=["output"],
            opset_version=14,
            dynamo=False,
        )
        print(f"Exported ONNX model to '{onnx_path}' ({onnx_path.stat().st_size} bytes)")
    print(
        '\nTip: rerun with a different --hidden-layers (e.g. "64" or "128,64") and '
        "compare the CV r2/mae above to the linear probe's -- with a small dataset, "
        "more layers often just overfits, so let the CV numbers decide, not intuition."
    )


if __name__ == "__main__":
    main()
