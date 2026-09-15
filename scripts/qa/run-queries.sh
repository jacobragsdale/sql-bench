#!/usr/bin/env bash
# T5.2 and T6.1: running statements, cancelling them and failing them, and
# browsing the objects of both containers. One replay per script per backend,
# each with a state directory of its own so the scratch pad starts empty.
# Exits non-zero on the first script that does not end at 0.
#
# Knobs: SQL_BENCH_CONFIG, SQL_BENCH_BIN (default: the debug build).
set -euo pipefail
cd "$(dirname "$0")/../.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

bin=${SQL_BENCH_BIN:-target/debug/sql-bench}
[ -x "$bin" ] || cargo build --quiet

fail=0
for script in run cancel error multi objects; do
    for backend in mssql oracle; do
        name=$script-$backend
        start=$(date +%s%3N)
        if SQL_BENCH_STATE_DIR="$work/$name" "$bin" --replay "scripts/replay/$name.keys" \
            --size 120x40 --frames-dir "$work/frames" >"$work/$name.log" 2>&1; then
            printf 'ok   %-14s %6sms\n' "$name" "$(($(date +%s%3N) - start))"
        else
            printf 'FAIL %-14s exit %s\n' "$name" "$?" >&2
            cat "$work/$name.log" >&2
            fail=1
        fi
    done
done
exit "$fail"
