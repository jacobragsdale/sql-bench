#!/usr/bin/env bash
# Build the hand-testing demo: database `shop` on SQL Server and schema SHOP
# on Oracle (connections shop-mssql and shop-oracle). Drops and rebuilds both
# on every run; the bench test fixture is not touched. Run db-up.sh first.
set -euo pipefail
cd "$(dirname "$0")/seed"

# Piped in rather than read from /seed: the containers may have been started
# from another checkout, and /seed is that checkout's directory.
out=$(docker exec -i sql-bench-mssql /opt/mssql-tools18/bin/sqlcmd -C -b -f 65001 \
    -S localhost -U sa -P 'Bench_Pass1!' -i /dev/stdin <demo-mssql.sql 2>&1) || { echo "$out" >&2; exit 1; }
echo "mssql   database shop rebuilt"

out=$(docker exec -i -e NLS_LANG=.AL32UTF8 sql-bench-oracle \
    sqlplus -s 'system/Bench_Pass1!@FREEPDB1' <demo-oracle.sql 2>&1) || { echo "$out" >&2; exit 1; }
# sqlplus reports its own errors (SP2-) in the output, not the exit status.
if grep -q 'SP2-' <<<"$out"; then echo "$out" >&2; exit 1; fi
echo "oracle  schema SHOP rebuilt"
