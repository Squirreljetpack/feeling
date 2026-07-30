#!/usr/bin/env python3
"""
Saliency adaptor: a small feed-forward head trained on top of frozen
sentence-embedding vectors to
predict a numeric emotional-saliency score.

Used by:
  - train_saliency_adaptor.py   (fits + saves the adaptor)
  - embed_regression.py         (loads it to replace the Shift ||Δ|| column)
"""
from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

import numpy as np
import torch
from torch import nn


class SaliencyAdaptor(nn.Module):
    """
    MLP head: embedding_dim -> ... -> 1 (scalar saliency score in [0, 1] when use_sigmoid=True).

    hidden_layers=() gives a plain linear probe (safest default for small
    datasets -- fewer parameters to overfit with). Pass e.g.
    hidden_layers=(128,) or (128, 64) to add depth once you have enough
    rows to justify it.
    """

    def __init__(
        self,
        input_dim: int,
        hidden_layers: Sequence[int] = (),
        dropout: float = 0.1,
        use_sigmoid: bool = True,
    ):
        super().__init__()
        dims = [input_dim, *hidden_layers]
        layers: list[nn.Module] = []
        for in_dim, out_dim in zip(dims[:-1], dims[1:]):
            layers.append(nn.Linear(in_dim, out_dim))
            layers.append(nn.ReLU())
            if dropout > 0:
                layers.append(nn.Dropout(dropout))
        layers.append(nn.Linear(dims[-1], 1))
        self.net = nn.Sequential(*layers)
        self.input_dim = input_dim
        self.hidden_layers = tuple(hidden_layers)
        self.dropout = dropout
        self.use_sigmoid = use_sigmoid

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        out = self.net(x).squeeze(-1)
        if self.use_sigmoid:
            out = torch.sigmoid(out)
        return out

    def predict_numpy(self, x: np.ndarray) -> np.ndarray:
        """x: (N, input_dim) or (input_dim,) raw embedding(s) -> (N,) scores."""
        x = np.atleast_2d(x)
        self.eval()
        with torch.no_grad():
            out = self(torch.as_tensor(x, dtype=torch.float32))
        return out.numpy()


def save_adaptor(model: SaliencyAdaptor, path: Path, extra: dict | None = None) -> None:
    payload = {
        "state_dict": model.state_dict(),
        "input_dim": model.input_dim,
        "hidden_layers": model.hidden_layers,
        "dropout": model.dropout,
        "use_sigmoid": model.use_sigmoid,
    }
    if extra:
        payload["extra"] = extra
    torch.save(payload, path)


def load_adaptor(path: Path) -> SaliencyAdaptor:
    payload = torch.load(path, map_location="cpu")
    model = SaliencyAdaptor(
        input_dim=payload["input_dim"],
        hidden_layers=payload["hidden_layers"],
        dropout=payload.get("dropout", 0.0),
        use_sigmoid=payload.get("use_sigmoid", False),
    )
    model.load_state_dict(payload["state_dict"])
    model.eval()
    return model

