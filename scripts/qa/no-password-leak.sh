#!/usr/bin/env bash
# T2.5: a password must never reach a trace file or a terminal.
#
# Connects to both containers with a wrong password that is easy to grep for,
# with SQL_BENCH_TRACE on, and greps the trace and the captured stderr for it.
# Exits non-zero if either holds it — or if the servers took the password,
# which would mean the check proved nothing.
#
# Knobs: SQL_BENCH_BIN (default: the release build).
set -euo pipefail
cd "$(dirname "$0")/../.."

bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -n "${SQL_BENCH_BIN:-}" ] || cargo build --release --quiet

secret='Sup3rSecret_Wrong!'
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The committed local connections with the password swapped for the bait.
sed -e "s/^password = .*/password = \"$secret\"/" config.local.toml >"$work/config.toml"

fail=0
for pair in 'local-mssql:select 1' 'local-oracle:select 1 from dual'; do
    conn=${pair%%:*} sql=${pair#*:}
    set +e
    SQL_BENCH_CONFIG="$work/config.toml" SQL_BENCH_TRACE="$work/trace.tsv" \
        "$bin" query --conn "$conn" "$sql" \
        >"$work/$conn.out" 2>"$work/$conn.err"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "$conn: the wrong password was accepted; this check proves nothing" >&2
        fail=1
    fi
    echo "$conn exit $status: $(head -1 "$work/$conn.err")"
done

for file in "$work"/trace.tsv "$work"/*.err "$work"/*.out; do
    [ -e "$file" ] || continue
    if grep -Fq "$secret" "$file"; then
        echo "FAIL: the password is in $(basename "$file"):" >&2
        grep -Fn "$secret" "$file" >&2
        fail=1
    fi
done
[ -s "$work/trace.tsv" ] || { echo "FAIL: nothing was traced, so nothing was checked" >&2; fail=1; }

if [ "$fail" -eq 0 ]; then
    echo "ok: no password in $(wc -l <"$work/trace.tsv") trace lines or either stderr"
fi
exit "$fail"
