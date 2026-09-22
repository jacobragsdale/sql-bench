#!/usr/bin/env bash
# T4.2: every way a connection can fail, driven through the TUI.
#
# Six cases, each a replay script under `scripts/replay/qa/` run against the
# real loop: a wrong password, a port nothing listens on, a database stopped
# under a connected tab and started again, a `password_cmd` that sleeps for
# thirty seconds, `--connect-all` with one database down, and — after every
# one of those — a second in which nothing may be drawn, because a settled
# transition animates nothing.
#
# Only the two cases that need an outage stop a container, one at a time, and
# only for the rest of the ten seconds `stopped-reconnect.keys` waits; both are
# started again as soon as that script is done, and the trap starts them
# whatever goes wrong. Deliberately does not use compose or scripts/db-*.sh.
#
# Knobs: SQL_BENCH_BIN (default: the release build).
set -euo pipefail
cd "$(dirname "$0")/../.."

bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -n "${SQL_BENCH_BIN:-}" ] || cargo build --release --quiet

work=$(mktemp -d)
# Both containers are left running however this ends, including a `set -e`.
trap 'docker start sql-bench-mssql sql-bench-oracle >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
fail=0

note() { printf '   %s\n' "$*"; }
bad() { printf '   FAIL: %s\n' "$*" >&2; fail=1; }

# Blocks until docker calls the container healthy again (db-outage.sh).
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

# Healthy is not the same as open for business: `bench` can still be
# recovering when the healthcheck's `select 1` against master answers, and a
# login to it is refused until it is not (db-outage.sh says the same).
answering() { # conn sql [seconds]
    local conn=$1 sql=$2 deadline=$(( SECONDS + ${3:-180} ))
    while :; do
        if SQL_BENCH_CONFIG=config.local.toml "$bin" query --conn "$conn" "$sql" >/dev/null 2>&1; then
            return 0
        fi
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 2
    done
}

# One `[[connection]]` and the Oracle client directory, so the single-tab
# scripts are tab 1 whichever backend they are pointed at.
one_connection() { # name
    awk -v want="name = \"$1\"" 'BEGIN { RS = ""; ORS = "\n\n" } /^\[oracle\]/ || index($0, want)' \
        config.local.toml
}

# One replay run, traced. Sets `status`, `elapsed`, `frames` and `idle` — the
# frames the trace recorded in the last second of the run, which every script
# that ends in an idle window spends turning the loop over a settled app.
replay() { # label config script [extra args...]
    local label=$1 config=$2 script=$3
    local trace=$work/$label.trace start end
    shift 3
    frames=$work/$label
    mkdir -p "$frames"
    status=0
    start=$(date +%s%3N)
    SQL_BENCH_CONFIG=$config SQL_BENCH_TRACE=$trace "$bin" --replay "$script" \
        --size 120x40 --frames-dir "$frames" "$@" \
        >"$work/$label.out" 2>"$work/$label.err" || status=$?
    end=$(date +%s%3N)
    elapsed=$(( end - start ))
    idle=$(awk -F'\t' -v from=$(( end - 1000 )) '$1 >= from && $2 == "frame"' "$trace" | wc -l)
    [ "$status" -eq 0 ] || bad "$label: the replay exited $status: $(tail -3 "$work/$label.err")"
}

# Case 6: after a transition settles nothing animates, so the trace must hold
# no `frame` line from the second the script spent idle at the end.
still() { # label
    [ "$idle" -le 1 ] || bad "$1: $idle frames were drawn in the second after the transition settled"
    note "$1: $idle frames drawn in the idle second"
}

# The driver's complaint as the Results pane shows it, for the log.
complaint() { # frame-file
    grep -o 'localhost:[0-9]*: cannot connect:.*' "$1" | sed 's/[[:space:]]*│.*$//;q' | cut -c1-96
}

echo "== 1 wrong password"
bait='Wr0ng_QA_Bait!'
for conn in local-mssql local-oracle; do
    one_connection "$conn" | sed "s/^password = .*/password = \"$bait\"/" >"$work/$conn-wrong.toml"
    replay "wrong-password-$conn" "$work/$conn-wrong.toml" scripts/replay/qa/wrong-password.keys
    note "$conn: $(complaint "$frames/wrong-password.txt")"
    if grep -rFq "$bait" "$frames" "$work/wrong-password-$conn.trace" \
        "$work/wrong-password-$conn.out" "$work/wrong-password-$conn.err"; then
        bad "$conn: the password is on a frame, in the trace or on a stream"
        grep -rFln "$bait" "$frames" "$work/wrong-password-$conn".* >&2
    fi
    still "wrong-password-$conn"
done

echo "== 2 wrong port"
for conn in local-mssql local-oracle; do
    one_connection "$conn" | sed 's/^port = .*/port = 1/' >"$work/$conn-port1.toml"
    replay "wrong-port-$conn" "$work/$conn-port1.toml" scripts/replay/qa/wrong-port.keys
    note "$conn: refused in ${elapsed}ms: $(complaint "$frames/wrong-port.txt")"
    [ "$elapsed" -lt 5000 ] || bad "$conn: a refusal on port 1 took ${elapsed}ms, which is not immediate"
done

echo "== 4 a password_cmd that hangs"
one_connection local-mssql | sed 's/^password = .*/password_cmd = "sleep 30"/' >"$work/hang.toml"
replay hang-abandon "$work/hang.toml" scripts/replay/qa/hang-abandon.keys
note "help opened, C abandoned the attempt, and the run took ${elapsed}ms of a 30 s sleep"
[ "$elapsed" -lt 10000 ] || bad "C did not abandon the attempt: the run took ${elapsed}ms"
still hang-abandon

# Case 5 runs inside case 3's outage, while SQL Server is still down.
connect_all_one_down() {
    echo "== 5 --connect-all with sql-bench-mssql down"
    replay connect-all config.local.toml scripts/replay/qa/connect-all-one-down.keys --connect-all
    note "tab bar:$(sed -n 2p "$frames/connect-all.txt")"
    note "the down tab: $(complaint "$frames/connect-all-failed-tab.txt")"
    [ "$elapsed" -lt 30000 ] || bad "--connect-all with one database down took ${elapsed}ms"
}

echo "== 3 stopped while connected, then reconnected"
outage() { # container conn sql-that-proves-it-is-back [hook run while it is down]
    local name=$1 conn=$2 alive=$3 hook=${4:-} dir=$work/stopped-$conn
    local replay_pid down stopped status=0 deadline
    one_connection "$conn" >"$work/$conn.toml"
    rm -rf "$dir"
    mkdir -p "$dir"
    SQL_BENCH_CONFIG=$work/$conn.toml SQL_BENCH_TRACE=$work/stopped-$conn.trace "$bin" \
        --replay scripts/replay/qa/stopped-reconnect.keys --size 120x40 --frames-dir "$dir" \
        >"$work/stopped-$conn.out" 2>"$work/stopped-$conn.err" &
    replay_pid=$!
    # The container is stopped only once the tab is connected, which is what
    # `connected.txt` says; the script then has ten seconds to spend.
    deadline=$(( SECONDS + 90 ))
    while [ ! -f "$dir/connected.txt" ]; do
        if ! kill -0 "$replay_pid" 2>/dev/null || [ "$SECONDS" -ge "$deadline" ]; then
            bad "$conn never connected: $(tail -3 "$work/stopped-$conn.err")"
            wait "$replay_pid" || true
            return
        fi
        sleep 0.2
    done
    down=$SECONDS
    # `-t 5`: SQL Server does not go down inside docker's default ten seconds,
    # so the default spends the script's whole window stopping. Five is long
    # enough for Oracle to shut down on its own and short enough to leave the
    # reconnect below a margin it cannot lose.
    docker stop -t 5 "$name" >/dev/null
    stopped=$(( SECONDS - down ))
    # The stop always returns inside the script's window; if one ever did not,
    # its `expect ✗` is the confusing failure and this is what explains it.
    [ "$stopped" -lt 8 ] || bad "$name took ${stopped}s to stop, which races the script's 10 s window"
    wait "$replay_pid" || status=$?
    if [ -n "$hook" ]; then "$hook"; fi
    docker start "$name" >/dev/null
    note "$name was down for $(( SECONDS - down ))s (${stopped}s of it stopping)"
    if [ "$status" -ne 0 ]; then
        bad "$conn: the reconnect against a stopped $name exited $status: $(tail -3 "$work/stopped-$conn.err")"
    else
        note "$conn: the reconnect failed with $(complaint "$dir/reconnect-failed.txt")"
    fi

    if healthy "$name"; then note "$name healthy again after $(( SECONDS - down ))s"; else
        bad "$name never came back healthy"
        return
    fi
    if answering "$conn" "$alive"; then note "$conn answers queries again"; else
        bad "$conn never answered again"
        return
    fi
    replay "restarted-$conn" "$work/$conn.toml" scripts/replay/qa/restarted-reconnect.keys
    note "$conn: c after the restart:$(sed -n '$p' "$frames/reconnected.txt" | cut -c1-60)"
    still "restarted-$conn"
}

outage sql-bench-mssql local-mssql 'select 1 as one' connect_all_one_down
outage sql-bench-oracle local-oracle 'select 1 as one from dual'

echo "== the E4 replay, both containers up"
replay connect config.local.toml scripts/replay/connect.keys
note "scripts/replay/connect.keys ran in ${elapsed}ms"

echo "== both containers"
for name in sql-bench-mssql sql-bench-oracle; do
    state=$(docker inspect --format '{{.State.Status}}/{{.State.Health.Status}}' "$name")
    note "$name: $state"
    [ "$state" = running/healthy ] || bad "$name is $state"
done
exit "$fail"
