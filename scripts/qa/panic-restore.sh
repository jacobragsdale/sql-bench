#!/usr/bin/env bash
# The terminal is given back when the loop panics.
#
# `--panic-after-ms` (a debug build's flag) panics inside the loop while it
# holds raw mode and the alternate screen. There is no tty in CI or in an
# agent's session, so `script` lends one, and what the pty captured is the
# evidence: the panic message, and after it the alternate screen left and the
# cursor shown again.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN=${1:-target/debug/sql-bench}
OUT=$(mktemp)
trap 'rm -f "$OUT"' EXIT

leave=$(printf '\033[?1049l')
mouse_off=$(printf '\033[?1000l')
show=$(printf '\033[?25h')

# The byte offset of the last time the capture has `$1` in it, or nothing.
last() { grep -aobF "$1" "$OUT" | tail -n 1 | cut -d: -f1; }

fail() {
    echo "panic-restore: $1" >&2
    cat -v "$OUT" >&2
    exit 1
}

status=0
# stty so the pty has a size and the shell is really drawn before the panic:
# a terminal nothing was drawn on proves nothing about giving one back. 0 ms
# so the panic is raised the first time the loop waits for a key, which is
# after that draw and before anything has to arrive — nothing is typed here,
# and `script` hands the pty an immediate end of input when, as in CI, its
# own stdin is /dev/null.
script -qec "stty rows 24 cols 80; SQL_BENCH_CONFIG=config.local.toml $BIN --panic-after-ms 0" \
    /dev/null >"$OUT" || status=$?

[ "$status" = 101 ] || fail "exited $status, and a panic exits 101"
grep -qa "Objects" "$OUT" || fail "the shell was never drawn, so nothing was taken to give back"
sed -n '/panicked at/,$p' "$OUT" | grep -qaF "$leave$show" ||
    fail "the alternate screen and the cursor were not given back after the panic"
tail -c 32 "$OUT" | grep -qaF "$leave$show" ||
    fail "the run did not end with the alternate screen left and the cursor shown"
off=$(last "$mouse_off")
[ -n "$off" ] && [ "$off" -lt "$(last "$leave")" ] ||
    fail "the mouse was not given back (\\e[?1000l) before the alternate screen was left"

echo "panic exit 101, then \\e[?1000l before \\e[?1049l\\e[?25h: raw mode off, alternate screen left, cursor shown"
