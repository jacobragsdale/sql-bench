#!/usr/bin/env bash
# Every check that needs no database: the QA replay scripts at the sizes they
# are meant to hold at, the NO_COLOR frames, the terminal restored after a
# panic and the draw latency. One line per check; the first failure is the
# exit code. Later tickets add lines to CASES and calls to `check`.
set -euo pipefail
cd "$(dirname "$0")/.."

export SQL_BENCH_CONFIG=config.local.toml
# Every case starts from an empty scratch pad, and none of them touch the
# pads of whoever is running this.
export SQL_BENCH_STATE_DIR=$(mktemp -d)
BIN=target/debug/sql-bench
FRAMES=$(mktemp -d)
LOG=$(mktemp)
trap 'rm -rf "$FRAMES" "$LOG" "$SQL_BENCH_STATE_DIR"' EXIT

cargo build --quiet

# <size> <script>: one replay run each, at the size that script asserts at.
CASES=(
    "60x15  shell.keys"
    "80x24  shell.keys"
    "120x40 shell.keys"
    "200x60 shell.keys"
    "40x10  too-small.keys"
    "120x40 resize.keys"
    "120x40 focus.keys"
    "120x40 quit-from-objects.keys"
    "120x40 quit-from-scratch.keys"
    "120x40 quit-from-results.keys"
)

# check <label> <command...>: one line, and the whole output if it failed.
check() {
    local label=$1
    shift
    if "$@" >"$LOG" 2>&1; then
        printf 'ok   %-34s %s\n' "$label" "$(tail -n 1 "$LOG")"
    else
        printf 'FAIL %s\n' "$label" >&2
        cat "$LOG" >&2
        exit 1
    fi
}

# NO_COLOR has to reach the frames: with it set every run of cells in a styles
# file is the terminal's own colours and no modifier, and without it the same
# frame is painted — which is what keeps this from passing on an empty file.
no_color() {
    NO_COLOR=1 "$BIN" --replay scripts/replay/qa/styles.keys --size 120x40 \
        --frames-dir "$FRAMES/plain" --frame-styles
    NO_COLOR= "$BIN" --replay scripts/replay/qa/styles.keys --size 120x40 \
        --frames-dir "$FRAMES/colour" --frame-styles
    if grep -v '^#' "$FRAMES/plain/styles.styles.txt" |
        grep -v 'fg=Reset bg=Reset mod=NONE'; then
        echo "NO_COLOR=1 painted the cells above" >&2
        return 1
    fi
    grep -q 'fg=Cyan bg=Reset mod=BOLD' "$FRAMES/colour/styles.styles.txt" ||
        { echo "the colour run painted nothing, so the check proves nothing" >&2; return 1; }
}

for case in "${CASES[@]}"; do
    read -r size script <<<"$case"
    check "$script at $size" \
        "$BIN" --replay "scripts/replay/qa/$script" --size "$size" --frames-dir "$FRAMES"
done
check "NO_COLOR=1 paints nothing" no_color
check "the terminal after a panic" scripts/qa/panic-restore.sh "$BIN"
check "draw latency" scripts/qa/draw-latency.sh "$BIN" 16
check "the scratch pad frame and its persistence" scripts/qa/scratch-frame.sh

# Everything below needs the local databases (scripts/db-up.sh) and stops
# containers along the way, so it runs only when asked for.
if [ "${SQL_BENCH_TEST_DBS:-}" = 1 ]; then
    check "no password leaks" scripts/qa/no-password-leak.sh
    check "max-rows timing" scripts/qa/max-rows-timing.sh
    check "connection lifecycle" scripts/qa/connections.sh
    check "query replays" scripts/qa/run-queries.sh
    check "the query workflow" scripts/qa/query-workflow.sh
    check "the object browser" scripts/qa/objects.sh
    check "database outage" scripts/qa/db-outage.sh
    check "the README frame" scripts/readme-frame.sh --check
fi
