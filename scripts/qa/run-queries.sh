#!/usr/bin/env bash
# T5.2, T5.3 and T6.1: running statements, cancelling, failing, inspecting and
# exporting them, and browsing the objects of both containers. One replay per script per backend,
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

# T5.3: the inspector, the export prompt and the file it wrote. SQL Server
# only — bench.big_text is where the 100 kB row is.
out=/tmp/sql-bench-inspect.csv
rm -f "$out"
start=$(date +%s%3N)
if SQL_BENCH_STATE_DIR="$work/inspect" "$bin" --replay scripts/replay/inspect.keys \
    --size 120x40 --frames-dir "$work/frames" >"$work/inspect.log" 2>&1; then
    # The header and the three rows, counted by a reader that knows a quoted
    # field can hold a line break of its own.
    rows=$(python3 -c 'import csv,sys; print(sum(1 for _ in csv.reader(open(sys.argv[1], newline=""))))' "$out")
    if [ "$rows" = 4 ]; then
        printf 'ok   %-14s %6sms  %s rows exported\n' inspect "$(($(date +%s%3N) - start))" "$rows"
    else
        printf 'FAIL %-14s %s CSV rows in %s, wanted 4\n' inspect "$rows" "$out" >&2
        fail=1
    fi
else
    printf 'FAIL %-14s exit %s\n' inspect "$?" >&2
    cat "$work/inspect.log" >&2
    fail=1
fi

exit "$fail"
