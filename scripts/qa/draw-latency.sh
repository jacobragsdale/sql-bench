#!/usr/bin/env bash
# Key-to-frame draw cost at 200x60, the biggest size the shell is checked at:
# the p95 of the draw_ms field of the trace's frame lines, against the budget
# a shell drawing placeholders has no excuse to miss.
#
# Usage: draw-latency.sh [binary] [budget in ms]. The 5 ms of DESIGN.md is
# what the release build is held to, and what docs/PERF.md records; a debug
# build does the same work through an unoptimised ratatui and is measured
# against the 16 ms of the key-to-frame budget instead, so that scripts/qa.sh
# catches a draw that got expensive without failing on a busy machine.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN=${1:-target/release/sql-bench}
BUDGET=${2:-5}
TRACE=$(mktemp)
FRAMES=$(mktemp -d)
trap 'rm -rf "$TRACE" "$FRAMES"' EXIT

SQL_BENCH_CONFIG=config.local.toml SQL_BENCH_TRACE=$TRACE \
    "$BIN" --replay scripts/replay/qa/latency.keys --size 200x60 --frames-dir "$FRAMES" >/dev/null

# One draw_ms per frame line, sorted, so a percentile is a line number.
sorted=$(awk -F'\t' '$2 == "frame" { sub(/^draw_ms=/, "", $3); print $3 }' "$TRACE" | sort -g)
n=$(printf '%s\n' "$sorted" | grep -c .)
[ "$n" -ge 20 ] || { echo "draw-latency: $n frames is not enough to measure" >&2; exit 1; }
at() { printf '%s\n' "$sorted" | sed -n "${1}p"; }

p50=$(at $(((n + 1) / 2)))
p95=$(at $(((n * 95 + 99) / 100)))
max=$(at "$n")
printf '%d frames at 200x60: draw_ms p50 %s  p95 %s  max %s  (budget %s)\n' \
    "$n" "$p50" "$p95" "$max" "$BUDGET"
awk -v p="$p95" -v b="$BUDGET" 'BEGIN { exit !(p < b) }' ||
    { echo "draw-latency: a p95 of $p95 ms is over the $BUDGET ms budget" >&2; exit 1; }
