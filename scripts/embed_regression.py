#!/usr/bin/env python3
"""
Strict 'Emotional state: X' - 'Emotional state' shift arithmetic evaluated with
Non-Negative Least Squares (NNLS) on NORMALIZED basis rays.

Embeddings come from the base bge_small.onnx asset (int8 QDQ), no fine-tuning.

Displays a single normalized-shift NNLS table:
  * every query's shift vector is scaled to L2 unit length
  * NNLS solves for non-negative weights over the mood rays
  * rows show a learned SALIENCY score (from scripts/adaptor.py, trained by
    scripts/train_adaptor.py on the RAW text embedding -- not the shift
    vector), the top 5 weights, reconstruction cosine match, and R^2
  * the shift vector itself (embed("Emotional state: X") -
    embed("Emotional state")) is still used internally to drive the NNLS
    basis-ray decomposition -- only the displayed magnitude column has
    been swapped from ||shift|| to the adaptor's saliency prediction

Dependencies: numpy, scipy, torch, onnxruntime, tokenizers
Usage: python3 scripts/embed_regression.py [--model-path PATH] [--tokenizer-path PATH] [--dataset PATH] [--adaptor-path PATH]
"""

import argparse
from pathlib import Path

import numpy as np
from adaptor import load_adaptor
from dataset_loader import DEFAULT_DATASET, load_dataset
from onnxruntime import InferenceSession
from scipy.optimize import nnls
from tokenizers import Tokenizer

MAX_SEQ_LEN = 256
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MODEL_PATH = REPO_ROOT / "assets/model/bge_small.onnx"
DEFAULT_TOKENIZER_PATH = REPO_ROOT / "target/cache/tokenizer.json"
DEFAULT_ADAPTOR_PATH = Path(__file__).resolve().parent / "saliency_adaptor.pt"
PREFIX = "Emotional state: "
BASE = "Emotional state"
# PREFIX = ""
# BASE = None

# Mood Axes -> Unipolar Rays
MOOD_AXES = [
    ("happy", "sad"),
    ("drained", "energetic"),
    ("reflective", "purposeful"),
    ("passive", "frustrated"),
    ("pained", "comfortable"),
    ("guarded", "vulnerable"),
    ("isolated", "connected"),
    ("confused", "lucid"),
    ("proud", "let down"),
    ("engrossed", "disgusted"),
]


def resolve_path(value: str, default: Path) -> Path:
    return Path(value) if value else default


def load_embedder(model_path: Path, tokenizer_path: Path) -> tuple[InferenceSession, Tokenizer]:
    session = InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
    tokenizer = Tokenizer.from_file(str(tokenizer_path))
    tokenizer.enable_truncation(max_length=MAX_SEQ_LEN)
    return session, tokenizer


def embed(text: str, session: InferenceSession, tokenizer: Tokenizer) -> np.ndarray:
    """Encode a single text with the base onnx model (mean pooling + L2 norm)."""
    enc = tokenizer.encode(text)
    input_ids = np.array([enc.ids], dtype=np.int64)
    attention_mask = np.array([enc.attention_mask], dtype=np.int64)

    input_names = {i.name for i in session.get_inputs()}
    feed = {"input_ids": input_ids, "attention_mask": attention_mask}
    if "token_type_ids" in input_names:
        feed["token_type_ids"] = np.zeros_like(input_ids)

    last_hidden = session.run(None, feed)[0]  # (1, seq_len, hidden)

    mask = attention_mask[:, :, None].astype(np.float32)
    summed = (last_hidden * mask).sum(axis=1)
    counts = np.clip(mask.sum(axis=1), 1e-9, None)
    pooled = (summed / counts)[0]

    norm = np.linalg.norm(pooled)
    return pooled / norm if norm > 0 else pooled


def cos_sim(u: np.ndarray, v: np.ndarray) -> float:
    norm_u = np.linalg.norm(u)
    norm_v = np.linalg.norm(v)
    if norm_u == 0 or norm_v == 0:
        return 0.0
    try:
        return float(np.dot(u, v) / (norm_u * norm_v))
    except ZeroDivisionError:
        return 0.0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-path", help="path to bge_small.onnx (default: scripts/../assets/model/bge_small.onnx)")
    parser.add_argument("--tokenizer-path", help="path to tokenizer.json (default: target/cache/tokenizer.json)")
    parser.add_argument(
        "--dataset", help="path to mood_journal_dataset.csv (default: scripts/mood_journal_dataset.csv)"
    )
    parser.add_argument(
        "--adaptor-path", help="path to trained saliency adaptor (default: scripts/saliency_adaptor.pt)"
    )
    args = parser.parse_args()

    model_path = resolve_path(args.model_path, DEFAULT_MODEL_PATH)
    tokenizer_path = resolve_path(args.tokenizer_path, DEFAULT_TOKENIZER_PATH)
    dataset_path = resolve_path(args.dataset, DEFAULT_DATASET)
    adaptor_path = resolve_path(args.adaptor_path, DEFAULT_ADAPTOR_PATH)

    print(f"Loading base embedding model from '{model_path}'...")
    session, tokenizer = load_embedder(model_path, tokenizer_path)

    print(f"Loading saliency adaptor from '{adaptor_path}'...")
    try:
        adaptor = load_adaptor(adaptor_path)
    except FileNotFoundError:
        raise SystemExit(
            f"No adaptor found at '{adaptor_path}'. Train one first with:\n    python3 scripts/train_adaptor.py"
        )

    # 1. Base Embedding
    v_base = embed(BASE, session, tokenizer) if BASE else 0

    # 2. Build basis vectors (normalized rays)
    basis_names: list[str] = []
    norm_vectors: list[np.ndarray] = []

    for start_mood, end_mood in MOOD_AXES:
        basis_names.extend([start_mood, end_mood])

    for mood_name in basis_names:
        v_raw = embed(PREFIX + mood_name, session, tokenizer) - v_base
        mag = np.linalg.norm(v_raw)
        norm_vectors.append(v_raw / mag if mag > 0 else v_raw)

    # Matrix (hidden_dim x n_rays) of normalized mood rays
    A_norm = np.column_stack(norm_vectors)

    # 3. Test suite (shared mood_journal_dataset.csv, see dataset_loader.py)
    test_suite = [row["input_text"] for row in load_dataset(dataset_path)]

    print("\n" + "=" * 128)
    print(f" TABLE: NORMALIZED SHIFT VECTORS in base embedding space ({model_path.name})")
    print("=" * 128)
    header = f"{'Input X ':<38} | {'Saliency':>11} | {'Top 5 NNLS Weights (w >= 0)':<50} | {'Cos Match':<9} | {'R²':<7}"
    print(header)
    print("-" * 128)

    for text in test_suite:
        prompt = PREFIX + text
        v_x = embed(prompt, session, tokenizer) - v_base
        len_x = np.linalg.norm(v_x)  # raw shift vector norm -- still drives NNLS below

        if len_x == 0:
            continue

        # Normalized: scale query vector to unit length
        target_vec = v_x / len_x
        target_mag = 1.0

        weights, residual_norm = nnls(A_norm, target_vec)
        v_rec = A_norm @ weights
        r2 = 1.0 - (residual_norm**2 / target_mag**2)
        match = cos_sim(v_rec, target_vec)

        # Learned saliency: adaptor's prediction on the RAW text embedding
        # (no "Emotional state: " prefix, no base-vector subtraction) --
        # this is what replaces the old ||shift|| column.
        v_text_raw = embed(text, session, tokenizer)
        try:
            saliency = float(adaptor.predict_numpy(v_text_raw)[0])
        except (TypeError, ValueError):
            saliency = float("nan")

        top_indices = np.argsort(weights)[::-1][:5]
        pos_coeffs = [(basis_names[i], weights[i]) for i in top_indices if weights[i] > 0.001]

        if pos_coeffs:
            top_str = ", ".join([f"{name}: +{w:.2f}" for name, w in pos_coeffs])
        else:
            top_str = "None"

        display_text = text if len(text) <= 36 else text[:33] + "..."

        print(f"{display_text:<38} | {saliency:>11.3f} | {top_str:<50} | {match:.4f}    | {r2:+.4f}")

    print("=" * 128)


if __name__ == "__main__":
    main()
