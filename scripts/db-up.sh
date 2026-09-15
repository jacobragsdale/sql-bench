#!/usr/bin/env bash
# Start SQL Server and Oracle, wait until both accept logins, load the seed
# schema into each and print one summary line per database.
# Idempotent: both seed files skip what already exists.
set -euo pipefail
cd "$(dirname "$0")/.."

SA_PASSWORD='Bench_Pass1!'

mssql() {
    docker exec sql-bench-mssql /opt/mssql-tools18/bin/sqlcmd \
        -C -b -S localhost -U sa -P "$SA_PASSWORD" "$@"
}

oracle() {
    docker exec -i -e NLS_LANG=.AL32UTF8 sql-bench-oracle \
        sqlplus -s bench/bench@FREEPDB1 "$@"
}

wait_healthy() {
    local name=$1 deadline=$((SECONDS + 300)) status=starting
    while [ "$status" != healthy ]; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "db-up: $name is $status after 5 minutes" >&2
            docker logs --tail 30 "$name" >&2 || true
            exit 1
        fi
        sleep 2
        status=$(docker inspect -f '{{.State.Health.Status}}' "$name" 2>/dev/null || echo missing)
    done
}

# Seed output is noise unless something went wrong. sqlplus reports most
# failures in its output rather than its exit status, so check both.
check() {
    local out=$1
    if grep -qE 'ORA-|PLS-|SP2-|Msg [0-9]+|compilation errors' <<<"$out"; then
        echo "$out" >&2
        exit 1
    fi
}

docker compose up -d
wait_healthy sql-bench-mssql
wait_healthy sql-bench-oracle

t0=$SECONDS
out=$(mssql -i /seed/mssql.sql 2>&1) || { echo "$out" >&2; exit 1; }
check "$out"
mssql_seconds=$((SECONDS - t0))

t0=$SECONDS
out=$(oracle @/seed/oracle.sql 2>&1) || { echo "$out" >&2; exit 1; }
check "$out"
oracle_seconds=$((SECONDS - t0))

mssql_counts=$(mssql -d bench -h -1 -W -Q "set nocount on;
    select concat('customers=', (select count(*) from bench.customers),
                  ' orders=', (select count(*) from bench.orders),
                  ' order_items=', (select count(*) from bench.order_items),
                  ' events=', (select count(*) from bench.events))" | tr -d '\r' | grep customers=)

oracle_counts=$(oracle <<'SQL' | tr -d '\r' | grep customers=
set heading off feedback off pagesize 0 linesize 200
select 'customers=' || (select count(*) from customers)
    || ' orders=' || (select count(*) from orders)
    || ' order_items=' || (select count(*) from order_items)
    || ' events=' || (select count(*) from events) from dual;
exit
SQL
)

for db in mssql oracle; do
    eval "counts=\$${db}_counts"
    case " $counts " in
        *" customers=50 "*) ;;
        *) echo "db-up: $db seed is wrong: $counts" >&2; exit 1 ;;
    esac
done

printf 'mssql   localhost:1433 db=bench  %s  seed %ss\n' "$mssql_counts" "$mssql_seconds"
printf 'oracle  localhost:1521 FREEPDB1  %s  seed %ss\n' "$oracle_counts" "$oracle_seconds"
