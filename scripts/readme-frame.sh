#!/usr/bin/env bash
# Put a real frame in README.md: replay scripts/replay/readme.keys at 100x28
# against the local SQL Server and write what it drew between the
# `<!-- frame:start -->` and `<!-- frame:end -->` markers. `--check` writes
# nothing and exits 1 if the README is not what a run would produce now.
#
# The picture in the README is therefore output and never a drawing. It needs
# `scripts/db-up.sh` to have run; the check is in the database-gated half of
# scripts/qa.sh for that reason, and CI — which has no databases — never runs
# it. Regenerate it locally when the layout changes.
#
# Milliseconds are masked out of the comparison: the footer says how long the
# connect took and the results title how long the query took, and neither is
# the same twice. The committed frame keeps whatever the run that wrote it
# measured.
#
# Masking the number is not enough on its own. A title's border fill and the
# footer's gap are what is left of a fixed width after the text, so each loses
# a character when the clock gains a digit; a mask that rewrote only the
# number would make this check pass or fail on how busy the machine was. The
# fill runs are squeezed too, on the lines that carry a clock and nowhere
# else, so those two lines are compared for their text and not their padding
# and every other line still pins the layout to the column.
set -euo pipefail
cd "$(dirname "$0")/.."

check=0
case "${1:-}" in
    --check) check=1 ;;
    "") ;;
    *) echo "usage: $0 [--check]" >&2; exit 1 ;;
esac

frames=$(mktemp -d)
state=$(mktemp -d)
trap 'rm -rf "$frames" "$state"' EXIT

cargo build --quiet
SQL_BENCH_CONFIG=config.local.toml SQL_BENCH_STATE_DIR="$state" \
    target/debug/sql-bench --replay scripts/replay/readme.keys \
    --size 100x28 --frames-dir "$frames"

# The `# readme 100x28` header is the frame file's own; the README wants the
# screen and not the label.
tail -n +2 "$frames/readme.txt" >"$frames/frame"

awk -v frame="$frames/frame" '
    /<!-- frame:start -->/ {
        print
        print "```"
        while ((getline line < frame) > 0) print line
        print "```"
        inside = 1
        next
    }
    /<!-- frame:end -->/ { inside = 0 }
    !inside
' README.md >"$frames/README.md"

grep -q '<!-- frame:start -->' README.md ||
    { echo "README.md has no <!-- frame:start --> marker" >&2; exit 1; }

# `<n> ms` and `<n>ms` are a clock, not a layout, and neither is the fill that
# shrank to make room for it.
clock() {
    sed -E 's/[0-9][0-9,]* ?ms/N ms/g
            /N ms/ { s/─+/─/g; s/ {2,}/ /g }' "$1"
}

# The mask is blind to the clock's width or this check is a coin toss: the
# same two lines, a one-digit clock against a three-digit one, compare equal.
clock_mask_ignores_the_clock() {
    local narrow wide
    narrow=$(printf '%s\n' '│╭ Results · 15 rows · 1 ms ────╮' ' hints    ● connected 5ms')
    wide=$(printf '%s\n' '│╭ Results · 15 rows · 123 ms ──╮' ' hints  ● connected 100ms')
    [ "$(clock <(printf '%s\n' "$narrow"))" = "$(clock <(printf '%s\n' "$wide"))" ] ||
        { echo "readme-frame: the clock mask still sees the clock's width" >&2; return 1; }
}

if [ "$check" = 1 ]; then
    clock_mask_ignores_the_clock
    if ! diff -u <(clock README.md) <(clock "$frames/README.md"); then
        echo "README.md is not the frame a run writes now: scripts/readme-frame.sh" >&2
        exit 1
    fi
    echo "README frame is current"
else
    cp "$frames/README.md" README.md
    echo "README frame written"
fi
