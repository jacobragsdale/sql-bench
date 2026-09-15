#!/usr/bin/env bash
# T5.1: the scratch pad, headlessly. Replays scripts/replay/scratch.keys and
# diffs the frame it writes against the one checked in, then proves a pad
# typed in one run is there in the next. No database has to be up.
set -euo pipefail
cd "$(dirname "$0")/../.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
frames=$work/frames
expected=scripts/replay/expected/scratch.txt

replay() { # state-dir script
    SQL_BENCH_STATE_DIR=$1 cargo run --quiet -- \
        --replay "$2" --size 120x40 --frames-dir "$frames"
}

replay "$work/frame-state" scripts/replay/scratch.keys
diff -u "$expected" "$frames/scratch.txt"
echo "scratch: the frame is the one in $expected"

# The pad is written on the way out and loaded again on the way in.
cat >"$work/save.keys" <<'KEYS'
key Tab
type select 42 from the pad
key Ctrl-Q
KEYS
cat >"$work/restore.keys" <<'KEYS'
expect select 42 from the pad
key Ctrl-Q
KEYS
replay "$work/state" "$work/save.keys"
test -s "$work/state/scratch/local-mssql.sql"
replay "$work/state" "$work/restore.keys"
echo "scratch: the pad came back after a restart"
