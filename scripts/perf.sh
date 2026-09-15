#!/usr/bin/env bash
# Measure every budget docs/DESIGN.md sets, print the table and append it to
# docs/PERF.md with the commit it was measured at, so a regression is a diff
# rather than a feeling. Exits non-zero if a budget was missed.
# Assumes scripts/db-up.sh has already run; builds release itself.
#
# Knobs: RUNS (cheap queries), SCAN_RUNS (the 100k row scans), STARTUP_RUNS,
# GRID_RUNS (replays of the grid per row cap), SQL_BENCH_CONFIG.
set -euo pipefail
cd "$(dirname "$0")/.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
# The grid replay types into a scratch pad. It keeps its own, not yours.
export SQL_BENCH_STATE_DIR=$(mktemp -d)
RUNS=${RUNS:-20}
SCAN_RUNS=${SCAN_RUNS:-5}
STARTUP_RUNS=${STARTUP_RUNS:-10}
GRID_RUNS=${GRID_RUNS:-3}
PERF=docs/PERF.md
FRAMES=$(mktemp -d)
trap 'rm -rf "$FRAMES" "$SQL_BENCH_STATE_DIR"' EXIT

cargo build --release --quiet
bench=target/release/sql-bench

# --- the measurements ------------------------------------------------------

# The nearest-rank percentile of the numbers on stdin: the sample at
# ceil(p/100 * n) counting from one, which is what `bench` reports too.
at_percentile() { # p
    sort -g | awk -v p="$1" '
        { sample[NR] = $0 }
        END {
            if (NR == 0) { print "n/a"; exit }
            rank = int((NR * p + 99) / 100)
            print sample[rank < 1 ? 1 : rank]
        }'
}

# Process start to the first frame, in milliseconds: the `start` line main
# writes before it has even parsed its arguments, and the `frame` line the
# first redraw writes. Both come off one clock in one process, which is the
# only way to measure this without hyperfine.
startup_ms() {
    local run trace
    for run in $(seq "$STARTUP_RUNS"); do
        trace=$(mktemp)
        SQL_BENCH_TRACE=$trace "$bench" --replay scripts/replay/quit.keys \
            --frames-dir "$FRAMES" >/dev/null
        awk -F'\t' '$2 == "start" { start = $1 }
                    $2 == "frame" && !seen { print $1 - start; seen = 1 }' "$trace"
        rm -f "$trace"
    done
}

# Every `draw_ms` of the hundred keys walked over a grid of `--max-rows`
# rows, over GRID_RUNS replays. The last hundred frames of each: the ones
# before them are the connect and the scan, and neither is a keystroke.
grid_draws() { # max-rows
    local run trace
    for run in $(seq "$GRID_RUNS"); do
        trace=$(mktemp)
        SQL_BENCH_TRACE=$trace "$bench" --replay scripts/replay/perf-grid.keys \
            --size 120x40 --max-rows "$1" --frames-dir "$FRAMES" >/dev/null
        awk -F'\t' '$2 == "frame" { sub(/^draw_ms=/, "", $3); print $3 }' "$trace" |
            tail -n 100
        rm -f "$trace"
    done
}

# The p50 of the `total` phase of a `bench` run: the round trip the TUI
# makes, with the one connect already paid for.
bench_total_p50() { # conn runs sql [--max-rows N]
    local conn=$1 runs=$2 sql=$3
    shift 3
    "$bench" bench --conn "$conn" --runs "$runs" "$@" "$sql" |
        awk '$1 == "total" { print $3 }'
}

# --- the verdicts ----------------------------------------------------------

FAILED=0
ROWS=()

# One row of the table, and the exit code if the budget did not hold. The
# measured value is a bare number so that awk compares it as one; `unit` is
# only what the table says after it, and `test` is an awk condition on `m`.
verdict() { # budget measured unit test
    local pass=yes
    awk -v m="$2" "BEGIN { exit !($4) }" || { pass=no; FAILED=1; }
    ROWS+=("| $1 | $2$3 | $pass |")
}

startup=$(startup_ms | at_percentile 50)
verdict 'startup to the first frame, no connections < 50 ms' "$startup" ' ms' 'm < 50'

ten_k=$(grid_draws 10000)
hundred_k=$(grid_draws 100000)
key_p95=$(printf '%s\n' "$ten_k" | at_percentile 95)
ten_p50=$(printf '%s\n' "$ten_k" | at_percentile 50)
hundred_p50=$(printf '%s\n' "$hundred_k" | at_percentile 50)
verdict 'key to frame p95, 10,000 rows on screen < 16 ms' "$key_p95" ' ms' 'm < 16'

ratio=$(awk -v ten="$ten_p50" -v hundred="$hundred_p50" \
    'BEGIN { printf "%.2f", (ten > 0 ? hundred / ten : 0) }')
verdict "draw cost at 100,000 rows over 10,000 ($hundred_p50 ms / $ten_p50 ms) < 1.20" \
    "$ratio" '' 'm < 1.20'

for conn in local-mssql local-oracle; do
    if [ "$conn" = local-mssql ]; then one='select 1'; else one='select 1 from dual'; fi
    verdict "\`select 1\` round trip on $conn < 5 ms" \
        "$(bench_total_p50 "$conn" "$RUNS" "$one")" ' ms' 'm < 5'
    verdict "1,000,000 row scan at \`--max-rows 100000\` on $conn < 8 s" \
        "$(bench_total_p50 "$conn" "$SCAN_RUNS" 'select * from bench.events' \
            --max-rows 100000)" ' ms' 'm < 8000'
done

# --- the report ------------------------------------------------------------

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

budgets() {
    printf '\n### %s (%s)\n\n' "$(date +%F)" "$(git rev-parse --short HEAD)"
    printf '| budget | measured | pass |\n|---|---|---|\n'
    printf '%s\n' "${ROWS[@]}"
}

# The heading goes in once and at the end, because every later block is
# appended to the end of the file and has to land under it.
grep -q '^## Budgets (T7.1)' "$PERF" || cat >>"$PERF" <<'HEADING'

## Budgets (T7.1)

Every budget `docs/DESIGN.md` sets, measured by `scripts/perf.sh` in a
release build at 120x40, one block per run. Startup is the median of ten
runs of `--replay scripts/replay/quit.keys`: the first `frame` line of the
trace minus the `start` line `main` writes before it parses its arguments.
Key to frame is the `draw_ms` of the hundred keys
`scripts/replay/perf-grid.keys` walks over a grid of 10,000 rows, three
replays pooled, and the row independence is the same measurement at ten
times the rows. The two query budgets are the `total` p50 of `sql-bench
bench`, the one connect already paid for. Nearest-rank percentiles
throughout, and the phase tables under each block are where those two
numbers come from. `cargo test --release -- --ignored` asserts the same
budgets with a 2x margin against synthetic rows and no database at all.
HEADING

{
    budgets
    printf '\n%s runs, %s for the 100k scans.\n' "$RUNS" "$SCAN_RUNS"
    table local-mssql 'select 1' 'select top 100 * from bench.customers'
    table local-oracle 'select 1 from dual' 'select * from bench.customers fetch first 100 rows only'
} >>"$PERF"

budgets
echo
echo "perf: appended to $PERF"
[ "$FAILED" = 0 ] || { echo "perf: a budget above was missed" >&2; exit 1; }
