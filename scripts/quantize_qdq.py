#!/usr/bin/env python3
"""
Convert FP32 ONNX model to static INT8 QDQ format for Burn.

Inserts DequantizeLinear nodes for each 8-bit weight matrix initializer,
matching float types so burn-onnx and burn-flex run with zero dtype mismatches.

Downloads the base bge-small-en-v1.5 export from HuggingFace and writes the
quantized asset to assets/model/bge_small.onnx -- the model vendored into the
Rust binary by build.rs. The base model is intentionally not fine-tuned in the
current flow (we train a small saliency adaptor on its frozen embeddings), so
this step just re-derives the upstream asset; the tokenizer.json used by
embed_regression.py / train_adaptor.py is cached alongside it.

Usage: python3 quantize_qdq.py [MODEL_URL] [RAW_PATH] [OUT_PATH]
"""

import os
import sys
import urllib.request
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MODEL_URL = "https://huggingface.co/xenova/bge-small-en-v1.5/resolve/main/onnx/model.onnx"
DEFAULT_TOKENIZER_URL = "https://huggingface.co/xenova/bge-small-en-v1.5/resolve/main/tokenizer.json"


def quantize_to_qdq(input_path: str, output_path: str) -> None:
    print(f"Loading ONNX model from {input_path}...")
    model = onnx.load(input_path)

    new_initializers = []
    new_nodes = []
    converted_count = 0

    for init in list(model.graph.initializer):
        if init.data_type == TensorProto.FLOAT:
            arr = numpy_helper.to_array(init).astype(np.float32)
            max_val = np.max(np.abs(arr))
            scale_val = max_val / 127.0 if max_val > 0 else 1.0
            q_arr = np.clip(np.round(arr / scale_val), -128, 127).astype(np.int8)

            q_name = init.name + "_quantized"
            scale_name = init.name + "_scale"
            deq_out_name = init.name + "_dequantized"

            q_init = numpy_helper.from_array(q_arr, name=q_name)
            scale_init = numpy_helper.from_array(np.array(scale_val, dtype=np.float32), name=scale_name)

            new_initializers.extend([q_init, scale_init])

            deq_node = helper.make_node(
                "DequantizeLinear",
                inputs=[q_name, scale_name],
                outputs=[deq_out_name],
                name="deq_" + init.name,
            )
            new_nodes.append(deq_node)

            # Update references in all graph nodes
            for node in model.graph.node:
                for i, inp in enumerate(node.input):
                    if inp == init.name:
                        node.input[i] = deq_out_name
            converted_count += 1
        else:
            new_initializers.append(init)

    model.graph.ClearField("initializer")
    model.graph.initializer.extend(new_initializers)
    for n in new_nodes:
        model.graph.node.insert(0, n)

    try:
        os.makedirs(os.path.dirname(output_path), exist_ok=True)
    except OSError as exc:
        raise RuntimeError(f"cannot create output directory for {output_path}") from exc
    onnx.save(model, output_path)
    print(f"Quantized {converted_count} weight initializers to INT8 QDQ.")
    print(f"Saved QDQ model to {output_path} ({os.path.getsize(output_path)} bytes).")


def download_if_missing(url: str, path: str) -> None:
    if os.path.exists(path):
        return
    print(f"Downloading {url}...")
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
    except OSError as exc:
        raise RuntimeError(f"cannot create cache directory for {path}") from exc
    urllib.request.urlretrieve(url, path)


if __name__ == "__main__":
    if len(sys.argv) >= 4:
        model_url = sys.argv[1]
        raw_path = sys.argv[2]
        out_path = sys.argv[3]
    else:
        model_url = DEFAULT_MODEL_URL
        raw_path = os.path.join(REPO_ROOT, "target", "cache", "bge_fp32.onnx")
        out_path = os.path.join(REPO_ROOT, "assets", "model", "bge_small.onnx")

    download_if_missing(model_url, raw_path)
    # Tokenizer used by embed_regression.py / train_adaptor.py (their default
    # paths point at target/cache/tokenizer.json).
    download_if_missing(
        DEFAULT_TOKENIZER_URL,
        os.path.join(REPO_ROOT, "target", "cache", "tokenizer.json"),
    )

    quantize_to_qdq(raw_path, out_path)
    print(f"\nFinal INT8 QDQ model: {out_path}")
    print("A subsequent `cargo build` embeds it into the binary (build.rs reads assets/model/bge_small.onnx).")
