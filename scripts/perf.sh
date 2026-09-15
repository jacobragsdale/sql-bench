#!/usr/bin/env bash
# Time the canonical queries on both containers and append the numbers to
# docs/PERF.md, so a regression is a diff rather than a feeling.
# Assumes scripts/db-up.sh has already run; builds release itself.
#
# Knobs: RUNS (cheap queries), SCAN_RUNS (the 100k row scans), SQL_BENCH_CONFIG.
set -euo pipefail
cd "$(dirname "$0")/.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
RUNS=${RUNS:-20}
SCAN_RUNS=${SCAN_RUNS:-5}
PERF=docs/PERF.md

cargo build --release --quiet
bench=target/release/sql-bench

# One markdown row: the phase numbers `bench` printed, folded onto one line.
row() { # conn label runs sql [--max-rows N]
    local conn=$1 label=$2 runs=$3 sql=$4
    shift 4
    "$bench" bench --conn "$conn" --runs "$runs" "$@" "$sql" | awk -v label="$label" '
        $1 == "connect"   { connect = $3 }
        $1 == "first_row" { first50 = $3; first95 = $4 }
        $1 == "total"     { total50 = $3; total95 = $4 }
        /rows\/s/         { rows = $1; per_second = $3 }
        END { printf "| %s | %s | %s | %s | %s | %s | %s | %s |\n",
                  label, rows, connect, first50, first95, total50, total95, per_second }'
}

table() { # conn one-row-query hundred-row-query
    local conn=$1
    printf '\n**%s**\n\n' "$conn"
    printf '| query | rows | connect | first_row p50 | first_row p95 | total p50 | total p95 | rows/s |\n'
    printf '|---|---|---|---|---|---|---|---|\n'
    row "$conn" 'select 1' "$RUNS" "$2"
    row "$conn" 'customers, top 100' "$RUNS" "$3"
    row "$conn" 'events, 10k cap' "$RUNS" 'select * from bench.events' --max-rows 10000
    row "$conn" 'events, 100k cap' "$SCAN_RUNS" 'select * from bench.events' --max-rows 100000
}

grep -q '^## Queries' "$PERF" || cat >>"$PERF" <<'HEADING'

## Queries

One block per run of `scripts/perf.sh`, against the containers
`scripts/db-up.sh` seeds. Milliseconds, nearest-rank percentiles; `connect`
is the one connect each run of `bench` makes, `rows/s` is over every run.
HEADING

{
    printf '\n### %s (%s) — %s runs, %s for the 100k scans\n' \
        "$(date +%F)" "$(git rev-parse --short HEAD)" "$RUNS" "$SCAN_RUNS"
    table local-mssql 'select 1' 'select top 100 * from bench.customers'
    table local-oracle 'select 1 from dual' 'select * from bench.customers fetch first 100 rows only'
} >>"$PERF"

echo "perf: appended to $PERF"
tail -n 24 "$PERF"
