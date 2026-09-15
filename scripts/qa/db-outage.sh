#!/usr/bin/env bash
# T2.5: what a query does when the database is stopped under it, and what the
# next one does once it is back.
#
# One container at a time: a query is put in flight, `docker stop` takes the
# server away, `docker start` brings it back at once, and then the checks run.
# What has to hold is that the interrupted query ended as an error — not a
# hang, and not half an answer printed as a whole one — and that the first
# query after the container is healthy connects again without being asked.
#
# That a *handle* opens a new connection after its own died is the other half
# of this, and needs no outage: `a_connection_the_server_closes_is_opened_again_for_the_next_query`
# in tests/mssql.rs and `a_session_the_server_ends_is_opened_again_for_the_next_query`
# in tests/oracle.rs end a live session from the server side and run on.
#
# The containers are shared, so each is down for exactly as long as
# `docker stop` takes: the restart is issued before anything is asserted, and
# the dead socket is dead whether or not the server is listening again.
# Deliberately does not use compose or scripts/db-*.sh.
#
# Knobs: SQL_BENCH_CONFIG, SQL_BENCH_BIN (default: the release build).
set -euo pipefail
cd "$(dirname "$0")/../.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -x "$bin" ] || cargo build --release --quiet

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0

note() { printf '   %s\n' "$*"; }
bad() { printf '   FAIL: %s\n' "$*" >&2; fail=1; }

# Blocks until docker calls the container healthy again.
healthy() { # container [seconds]
    local name=$1 deadline=$(( SECONDS + ${2:-420} ))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ "$(docker inspect --format '{{.State.Health.Status}}' "$name")" = healthy ]; then
            return 0
        fi
        sleep 2
    done
    return 1
}

outage() { # container conn sql-that-keeps-the-server-busy sql-that-proves-it-is-back
    local name=$1 conn=$2 busy=$3 alive=$4
    echo "== $name"
    "$bin" query --conn "$conn" --timeout 120 --max-rows 100000 "$busy" \
        >"$work/$conn.out" 2>"$work/$conn.err" &
    local query=$! down status=0
    sleep 2

    down=$SECONDS
    docker stop "$name" >/dev/null
    docker start "$name" >/dev/null
    note "down for $(( SECONDS - down ))s"

    wait "$query" || status=$?
    note "the interrupted query exited $status: $(head -1 "$work/$conn.err")"
    if [ "$status" -eq 0 ]; then
        bad "the query outlived the server it was running on"
    fi
    if [ ! -s "$work/$conn.err" ]; then
        bad "the query failed without saying why"
    fi
    if [ -s "$work/$conn.out" ]; then
        bad "rows were printed for a query that never finished"
    fi

    if healthy "$name"; then
        note "healthy again after $(( SECONDS - down ))s"
    else
        bad "$name never came back healthy"
        return
    fi

    # Healthy is not the same as open for business: the SQL Server
    # healthcheck asks `master` for `select 1`, and the first run of this
    # script caught the server answering that while `bench` was still
    # recovering, which it refuses a login for (error 4060). So the query that
    # proves the connection comes back is retried until the server stops
    # saying no.
    local back=$SECONDS deadline=$(( SECONDS + 120 ))
    while :; do
        if "$bin" query --conn "$conn" --format csv "$alive" >"$work/$conn.after" 2>&1; then
            note "the next query reconnected $(( SECONDS - back ))s later: $(tail -1 "$work/$conn.after")"
            break
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            bad "the next query could not reconnect: $(cat "$work/$conn.after")"
            break
        fi
        sleep 2
    done
}

# SQL Server streams a thousand rows and then holds the batch open; Oracle has
# no batch, so a join it cannot finish is what keeps the session busy.
outage sql-bench-mssql local-mssql \
    "select top 1000 * from bench.events; waitfor delay '00:01:00'" \
    'select 1 as one'
outage sql-bench-oracle local-oracle \
    'select count(*) from bench.events e1, bench.events e2' \
    'select 1 as one from dual'

echo "== both containers"
for name in sql-bench-mssql sql-bench-oracle; do
    state=$(docker inspect --format '{{.State.Status}}/{{.State.Health.Status}}' "$name")
    note "$name: $state"
    [ "$state" = running/healthy ] || bad "$name is $state"
done
exit "$fail"
