#!/usr/bin/env bash
# `sql-bench </dev/null` gives the terminal back instead of holding it.
#
# A run whose own standard input is at end of file has no keyboard: crossterm
# would read /dev/tty instead and wait there for bytes that never come, which
# used to leave raw mode and the alternate screen held by a loop no key could
# quit. `script` lends the pty — there is no tty in CI or in an agent's
# session — and what it captured is the evidence: the shell drawn, and after
# it the alternate screen left and the cursor shown again.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN=${1:-target/debug/sql-bench}
LIMIT=${2:-2}
OUT=$(mktemp)
STATE=$(mktemp -d)
trap 'rm -rf "$OUT" "$STATE"' EXIT

leave=$(printf '\033[?1049l')
mouse_off=$(printf '\033[?1000l')
show=$(printf '\033[?25h')

# The byte offset of the last time the capture has `$1` in it, or nothing.
last() { grep -aobF "$1" "$OUT" | tail -n 1 | cut -d: -f1; }

fail() {
    echo "stdin-eof: $1" >&2
    cat -v "$OUT" >&2
    exit 1
}

started=$(date +%s%3N)
status=0
timeout "$LIMIT" script -qec \
    "stty rows 24 cols 80; SQL_BENCH_CONFIG=config.local.toml SQL_BENCH_STATE_DIR=$STATE $BIN </dev/null" \
    /dev/null >"$OUT" </dev/null || status=$?
elapsed=$(($(date +%s%3N) - started))

[ "$status" = 124 ] && fail "still running after ${LIMIT}s, so the terminal is still held"
[ "$status" = 0 ] || fail "exited $status, and an input that ran out is not a failure"
grep -qa "Objects" "$OUT" || fail "the shell was never drawn, so nothing was taken to give back"
tail -c 32 "$OUT" | grep -qaF "$leave$show" ||
    fail "the run did not end with the alternate screen left and the cursor shown"
off=$(last "$mouse_off")
[ -n "$off" ] && [ "$off" -lt "$(last "$leave")" ] ||
    fail "the mouse was not given back (\\e[?1000l) before the alternate screen was left"

echo "exit 0 after ${elapsed} ms, then \\e[?1000l before \\e[?1049l\\e[?25h: the terminal was given back"
