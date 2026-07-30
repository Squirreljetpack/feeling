"""
Shared loader for `scripts/mood_journal_dataset.csv`.

All embedding scripts read their test entries from this single CSV instead of
embedding datasets in code. Columns: `type`, `category`, `input_text`.
`type` is the 3-way split used by embed_confidence.py (EMOTION / ADJACENT /
CONTROL); the other scripts use `category` + `input_text`.
"""

import csv
from pathlib import Path

DEFAULT_DATASET = Path(__file__).resolve().parent / "mood_journal_dataset.csv"


def load_dataset(path: Path = DEFAULT_DATASET) -> list[dict[str, str]]:
    """Load all rows as dicts with keys type, category, input_text."""
    with open(path, newline="", encoding="utf-8") as f:
        return [dict(row) for row in csv.DictReader(f)]
