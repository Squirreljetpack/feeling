#!/bin/bash
# Run all feeling views and capture output to a temp file.
# Seeds a fresh DB with dev config in a temp config dir, captures view
# output, then restores the original DB and cleans up on exit.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DEV_CONFIG_PATH="$PROJECT_ROOT/assets/dev.toml"

DB_DIR="$HOME/.local/state/feeling"
DB_PATH="$DB_DIR/feeling.db"
BACKUP_PATH="$DB_DIR/feeling.db.bak"
OUTFILE=$(mktemp /tmp/feeling_views_XXXXXX.txt)

TMP_CONFIG_DIR=$(mktemp -d /tmp/feeling_config_XXXXXX)
cp "$DEV_CONFIG_PATH" "$TMP_CONFIG_DIR/config.toml"
export FEELING_CONFIG_DIR="$TMP_CONFIG_DIR"

# ---------------------------------------------------------------------------
# Backup existing DB and clean up temporary config/DB on exit
# ---------------------------------------------------------------------------
if [[ -f "$DB_PATH" ]]; then
	cp "$DB_PATH" "$BACKUP_PATH"
fi

cleanup() {
	rm -rf "$TMP_CONFIG_DIR"
	if [[ -f "$BACKUP_PATH" ]]; then
		mv "$BACKUP_PATH" "$DB_PATH"
	else
		rm -f "$DB_PATH"
	fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1. Remove any existing DB so we start with a clean slate
# ---------------------------------------------------------------------------
rm -f "$DB_PATH"
mkdir -p "$DB_DIR"

# ---------------------------------------------------------------------------
# 2. Seed the DB with all variations using the release build
# ---------------------------------------------------------------------------
cargo run --release --example seed-db -- "$DB_PATH"

# ---------------------------------------------------------------------------
# 3. Run all view commands, capturing output to a temp file
# ---------------------------------------------------------------------------
# Save original stdout (fd 3) then redirect all subsequent stdout to the file
exec 3>&1
exec >"$OUTFILE"

echo "=== Tracker view (default period = Week, mood grid) ==="
cargo run --release --bin feeling -- :

echo ""
echo "=== Tracker view (specific trackers: @recurring_1, sleep, run_times, water, notes, steps, mood_notes) ==="
cargo run --release --bin feeling -- : @recurring_1 sleep run_times water notes steps mood_notes

echo ""
echo "=== Tracker view (mood grid between trackers: sleep : run_times) ==="
cargo run --release --bin feeling -- : sleep : run_times

echo ""
echo "=== Tracker view (Week) ==="
cargo run --release --bin feeling -- :week

echo ""
echo "=== Tracker view (Month, tracker combo: @recurring_1) ==="
cargo run --release --bin feeling -- :month @recurring_1

echo ""
echo "=== Tracker view (Year, tracker combo: steps mood_notes) ==="
cargo run --release --bin feeling -- :year steps mood_notes

echo ""
echo "=== Scheduled task creation (immediate: ! '@10pm; meeting; @2 hours') ==="
cargo run --release --bin feeling -- ! '@10pm; meeting; @2 hours'

echo ""
echo "=== Today view (bare feeling; includes scheduled tasks via window overlap) ==="
cargo run --release --bin feeling --

echo ""
echo "=== Recurring tasks view (@) ==="
cargo run --release --bin feeling -- @

echo ""
echo "=== Done tasks view (@done) ==="
cargo run --release --bin feeling -- @done

echo ""
echo "=== Due tasks view (@due) — today view, tasks only ==="
cargo run --release --bin feeling -- @due

echo ""
echo "=== @scheduled view was removed (unknown view error) ==="
cargo run --release --bin feeling -- @scheduled || true

echo ""
echo "=== Oneshot tasks view (@:o) ==="
cargo run --release --bin feeling -- @:o

echo ""
echo "=== Recurring+scheduled pending view (@:O) ==="
cargo run --release --bin feeling -- @:O

echo ""
echo "=== Recurring history / completed scheduled view (@done:O) ==="
cargo run --release --bin feeling -- @done:O

echo ""
echo "=== Due view, tomorrow horizon (@due:t) ==="
cargo run --release --bin feeling -- @due:t

# ---------------------------------------------------------------------------
# 4. Print the output file path to the original terminal
# ---------------------------------------------------------------------------
echo "$OUTFILE" >&3

# ---------------------------------------------------------------------------
# 5. Delete the tmp DB since it was only needed for the view run
# ---------------------------------------------------------------------------
rm -f "$DB_PATH"
