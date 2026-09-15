#!/usr/bin/env bash
# T2.5: the row cap has to stop the network, not just the printing — so the
# whole run of `query --max-rows N` over the 1,000,000 row `events` table is
# timed, connect and process start included.
#
# Budgets (docs/DESIGN.md, recorded in docs/PERF.md): 10,000 rows < 2 s,
# 100,000 rows < 8 s, on both backends. Exits non-zero on the first miss.
#
# Knobs: SQL_BENCH_CONFIG, SQL_BENCH_BIN (default: the release build).
set -euo pipefail
cd "$(dirname "$0")/../.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -x "$bin" ] || cargo build --release --quiet

fail=0
printf '%-14s %8s %10s %8s\n' backend rows elapsed budget
for conn in local-mssql local-oracle; do
    sql='select * from bench.events'
    for cap in 10000:2000 100000:8000; do
        rows=${cap%:*} budget=${cap#*:}
        start=$(date +%s%3N)
        got=$("$bin" query --conn "$conn" --max-rows "$rows" --format csv "$sql" 2>&1 >/dev/null)
        elapsed=$(( $(date +%s%3N) - start ))
        printf '%-14s %8s %9sms %7sms' "$conn" "$rows" "$elapsed" "$budget"
        case "$got" in
            *"$rows rows (truncated at $rows)"*) ;;
            *) printf '  BAD: %s\n' "$got"; fail=1; continue ;;
        esac
        if [ "$elapsed" -lt "$budget" ]; then printf '  ok\n'; else printf '  OVER BUDGET\n'; fail=1; fi
    done
done
exit "$fail"
