#!/usr/bin/env bash
# A real terminal's mouse reaches the app, and the terminal gets the mouse
# back every time the app lets go of it.
#
# Replay hands the loop crossterm events, so it never sees the bytes a
# terminal sends. Here `script` lends a pty at 80x24 and the mouse is typed
# into it the way xterm reports it with SGR mode on (1-based `\e[<0;x;yM` for
# the left button down, `m` for up): a click in the scratch pad focuses it,
# Ctrl-E hands the terminal to an editor that is `true` and takes it back, a
# click on `? Help` opens the help, and Ctrl-Q quits. What the pty captured is
# the evidence: capture turned on, the help drawn for the pane the first
# click focused, capture off and on again around the editor, and capture off
# before the alternate screen is left for good. No database is touched.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN=${1:-target/debug/sql-bench}
OUT=$(mktemp)
STATE=$(mktemp -d)
trap 'rm -rf "$OUT" "$STATE"' EXIT

on=$(printf '\033[?1000h')
sgr=$(printf '\033[?1006h')
off=$(printf '\033[?1000l')
leave=$(printf '\033[?1049l')

# Every byte offset at which the capture has `$1`, one per line.
at() { grep -aobF "$1" "$OUT" | cut -d: -f1 || true; }

fail() {
    echo "mouse-bytes: $1" >&2
    cat -v "$OUT" >&2
    exit 1
}

# A click is a press and a release on one cell, 1-based column then row.
click() { printf '\033[<0;%d;%dM' "$1" "$2"; sleep 0.1; printf '\033[<0;%d;%dm' "$1" "$2"; }

# The sleeps give the first frame, the editor and each click time to land:
# bytes that arrive before the app has drawn are still read, but a click is
# resolved against the frame that was on screen.
{
    sleep 1
    click 41 6 # the scratch pad, row 5 of the frame
    sleep 0.3
    printf '\005' # Ctrl-E
    sleep 1
    click 75 1 # `? Help`, at the right end of the tab bar
    sleep 0.5
    printf '\021' # Ctrl-Q
    sleep 0.5
} | timeout 10 script -qec \
    "stty rows 24 cols 80; VISUAL=true EDITOR=true SQL_BENCH_CONFIG=config.local.toml SQL_BENCH_STATE_DIR=$STATE $BIN" \
    /dev/null >"$OUT" || fail "exited $?"

ons=($(at "$on"))
offs=($(at "$off"))
leaves=($(at "$leave"))
[ "${#ons[@]}" = 2 ] || fail "mouse capture was turned on ${#ons[@]} times, not at start and after the editor"
[ -n "$(at "$sgr")" ] || fail "SGR reporting (\\e[?1006h) was never asked for"
[ "${#offs[@]}" = 2 ] || fail "mouse capture was turned off ${#offs[@]} times, not for the editor and at the end"
grep -qa "Help · Scratch" "$OUT" ||
    fail "the help for Scratch was never drawn, so the clicks did not reach the app"
# on, off for the editor, on again, off at the end, and that last off before
# the last time the alternate screen is left.
[ "${ons[0]}" -lt "${offs[0]}" ] && [ "${offs[0]}" -lt "${ons[1]}" ] &&
    [ "${ons[1]}" -lt "${offs[1]}" ] || fail "capture was not off around the editor and on again after it"
[ "${offs[1]}" -lt "${leaves[-1]}" ] ||
    fail "the mouse was not given back (\\e[?1000l) before the alternate screen was left"

echo "two clicks through the pty drew Help · Scratch; capture on, off for the editor, on, off before \\e[?1049l"
