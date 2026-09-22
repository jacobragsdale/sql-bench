#!/usr/bin/env bash
# Launch connects the first tab, `--connect-all` every tab, and `q` leaves no
# session behind on either server. Counted from the servers' side: SQL Server
# by the application name the driver sends, Oracle as SYSTEM, because the
# bench user cannot read v$session.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN=${1:-target/debug/sql-bench}
STATE=$(mktemp -d)
DURING=$(mktemp)
trap 'rm -rf "$STATE" "$DURING"' EXIT
export SQL_BENCH_CONFIG=config.local.toml

mssql() {
    "$BIN" query --conn local-mssql --format csv "select count(*) from sys.dm_exec_sessions
        where program_name = 'sql-bench' and session_id <> @@SPID" 2>/dev/null | tail -n 1
}
oracle() {
    printf "set heading off feedback off\nselect count(*) from v\$session where username = 'BENCH';\n" |
        docker exec -i sql-bench-oracle sqlplus -s 'system/"Bench_Pass1!"@localhost/FREEPDB1' | tr -d ' \t\n'
}

# check <flags> <mssql while up> <oracle while up>
check() {
    {
        sleep 5
        echo "$(mssql) $(oracle)" >"$DURING"
        printf q
        sleep 0.5
    } | timeout 20 script -qec \
        "stty rows 40 cols 120; SQL_BENCH_STATE_DIR=$STATE $BIN $1" /dev/null >/dev/null
    local during
    during=$(cat "$DURING")
    [ "$during" = "$2 $3" ] || { echo "quit-closes: '$1' had sessions $during, not $2 $3" >&2; exit 1; }
    sleep 0.5
    local after="$(mssql) $(oracle)"
    [ "$after" = "0 0" ] || { echo "quit-closes: '$1' left sessions $after after q" >&2; exit 1; }
}

check "" 1 0
check --connect-all 1 1
echo "launch connected the first tab, --connect-all both, and q closed them all"
