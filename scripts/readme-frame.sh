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

if [ "$check" = 1 ]; then
    # `<n> ms` and `<n>ms` are a clock, not a layout.
    clock() { sed -E 's/[0-9]+ ?ms/N ms/g' "$1"; }
    if ! diff -u <(clock README.md) <(clock "$frames/README.md"); then
        echo "README.md is not the frame a run writes now: scripts/readme-frame.sh" >&2
        exit 1
    fi
    echo "README frame is current"
else
    cp "$frames/README.md" README.md
    echo "README frame written"
fi
