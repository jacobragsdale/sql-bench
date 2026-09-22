#!/usr/bin/env bash
# T6.2: the object browser against everything scripts/seed seeded.
#
# Four questions, both containers:
#   1. is every seeded object reachable through the tree, and is the tree's
#      listing the one `objects --kind` prints;
#   2. is the source `s` shows the source `source` prints, line for line;
#   3. does a procedure created WITH ENCRYPTION say so rather than nothing;
#   4. does a schema of a thousand tables still open inside 2 s and scroll
#      under the 5 ms draw budget.
# Plus the filter: `/cust` narrows, one that matches nothing says so, and Esc
# gives the tree back from either.
#
# The tree is walked by a replay and read off the frames it writes, so what
# is asserted is what was drawn. Everything this creates — bench.sp_secret,
# the qa1000 schema, BENCH's QA_T% tables — is dropped by the EXIT trap, and
# the last check is that both databases are back to the seed.
#
# Deliberately does not use compose or scripts/db-*.sh: the containers are
# shared with whoever else is running QA, and this stops neither.
#
# Knobs: SQL_BENCH_CONFIG, SQL_BENCH_BIN (default: the release build).
set -euo pipefail
cd "$(dirname "$0")/../.."

export SQL_BENCH_CONFIG=${SQL_BENCH_CONFIG:-config.local.toml}
bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -n "${SQL_BENCH_BIN:-}" ] || cargo build --release --quiet

# 200x60: the object pane holds a seeded schema fully expanded, and the
# results pane holds more lines than the longest seeded source.
SIZE=200x60
SOURCE_LINES=33

work=$(mktemp -d)
fail=0
trap 'teardown; rm -rf "$work"' EXIT

ok() { printf 'ok   %s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*" >&2; fail=1; }

sqlcmd() {
    docker exec -i sql-bench-mssql /opt/mssql-tools18/bin/sqlcmd \
        -C -b -S localhost -U sa -P 'Bench_Pass1!' -d bench "$@"
}

# Reads the script off stdin, the way scripts/db-up.sh does.
sqlplus() {
    docker exec -i -e NLS_LANG=.AL32UTF8 sql-bench-oracle \
        sqlplus -s bench/bench@FREEPDB1
}

# Another QA run may have a container down for a moment. Nothing here stops
# one, so waiting is all this does.
healthy() { # container [seconds]
    local name=$1 deadline=$((SECONDS + ${2:-420}))
    while [ "$SECONDS" -lt "$deadline" ]; do
        [ "$(docker inspect --format '{{.State.Status}}/{{.State.Health.Status}}' \
            "$name" 2>/dev/null)" = running/healthy ] && return 0
        sleep 2
    done
    return 1
}

teardown() {
    sqlcmd -Q "
        SET NOCOUNT ON;
        IF OBJECT_ID('bench.sp_secret') IS NOT NULL DROP PROCEDURE bench.sp_secret;
        DECLARE @sql nvarchar(max) = N'';
        SELECT @sql = @sql + 'DROP TABLE qa1000.' + QUOTENAME(o.name) + ';'
        FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
        WHERE s.name = 'qa1000' AND o.type = 'U';
        EXEC sp_executesql @sql;
        IF SCHEMA_ID('qa1000') IS NOT NULL EXEC('DROP SCHEMA qa1000');
    " >/dev/null 2>&1 || true
    sqlplus <<'SQL' >/dev/null 2>&1 || true
SET FEEDBACK OFF
BEGIN
  FOR t IN (SELECT table_name FROM user_tables WHERE table_name LIKE 'QA\_T%' ESCAPE '\') LOOP
    EXECUTE IMMEDIATE 'DROP TABLE "' || t.table_name || '" PURGE';
  END LOOP;
END;
/
EXIT
SQL
    # Nothing here is allowed to leave a database it changed, so a drop that
    # did not take says so even when it is the trap running.
    local left
    left=$(sqlcmd -h -1 -W -Q "
        SET NOCOUNT ON;
        SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
        WHERE s.name = 'qa1000' OR o.name = 'sp_secret';
    " 2>&1 | tr -d '[:space:]')
    [ "$left" = 0 ] || printf 'objects: sql-bench-mssql still holds %s of what this made\n' "$left" >&2
}

# replay <name> <keys file>: one run, retried once through a container that
# was restarted under it.
replay() {
    local name=$1 keys=$2 try
    for try in 1 2; do
        if SQL_BENCH_STATE_DIR="$work/state-$name" SQL_BENCH_TRACE="$work/$name.trace" \
            "$bin" --replay "$keys" --size "$SIZE" --frames-dir "$work/frames" \
            >"$work/$name.log" 2>&1; then
            return 0
        fi
        [ "$try" = 1 ] || break
        grep -qiE 'connect|closed|ORA-|link|refused' "$work/$name.log" || break
        healthy sql-bench-mssql || true
        healthy sql-bench-oracle || true
        rm -f "$work/$name.trace"
    done
    cat "$work/$name.log" >&2
    return 1
}

frame() { printf '%s/frames/%s.txt' "$work" "$1"; }

# The right-hand pane of a frame from one title line down: what is past the
# two borders that divide the panes, with the pane's own border and the
# padding taken off.
pane() { # frame title-pattern
    sed -n "/$2/,\$p" "$(frame "$1")" | sed -n 's/.*││//p' | sed -e 's/│$//' -e 's/ *$//'
}

# rows_have <frame> <name...>: every name is drawn as a row of the tree.
# Prints the ones that are not.
rows_have() {
    local file missing=() name
    file=$(frame "$1")
    shift
    for name in "$@"; do
        grep -qE "[ ▸▾] ${name}( |\$)" "$file" || missing+=("$name")
    done
    [ ${#missing[@]} -eq 0 ] || { printf '%s ' "${missing[@]}"; return 1; }
}

# The names `objects --kind` lists, one per line, sorted.
listed() { # conn schema kind
    "$bin" objects --conn "$1" --schema "$2" --kind "$3" | sed 1,2d | awk '{print $3}' | sort
}

########################################################################
# What the seed made, per backend. Every one of these has to be listed by
# the subcommand and drawn in the tree.
########################################################################

MSSQL_table='all_types big_text binary_blobs customers events order_items orders'
MSSQL_view='v_customer_totals v_recent_orders'
MSSQL_procedure='sp_customer_orders sp_mark_shipped'
MSSQL_function='fn_order_total tvf_orders_by_status'
MSSQL_sequence='order_seq'
MSSQL_sourced='sp_customer_orders sp_mark_shipped fn_order_total tvf_orders_by_status'
MSSQL_customers=customers
MSSQL_orders=orders
MSSQL_totals=v_customer_totals
# What tests/catalog.rs says `bench.customers` was declared as, which is what
# `i` has to put in the grid.
MSSQL_columns='id int no yes
name nvarchar(100) no no
email varchar(200) no no
country char(2) no no
created_at datetime2(7) no no
credit_limit decimal(12,2) yes no'

ORACLE_table='ALL_TYPES BIG_TEXT BINARY_BLOBS CUSTOMERS EVENTS ORDER_ITEMS ORDERS'
ORACLE_view='V_CUSTOMER_TOTALS V_RECENT_ORDERS'
ORACLE_procedure='CUSTOMER_ORDERS MARK_SHIPPED'
ORACLE_function='ORDER_TOTAL'
ORACLE_package='ORDER_PKG'
ORACLE_sequence='ORDER_SEQ'
ORACLE_sourced='CUSTOMER_ORDERS MARK_SHIPPED ORDER_TOTAL ORDER_PKG'
ORACLE_customers=CUSTOMERS
ORACLE_orders=ORDERS
ORACLE_totals=V_CUSTOMER_TOTALS
ORACLE_columns='ID NUMBER(10) no yes
NAME NVARCHAR2(100) no no
EMAIL VARCHAR2(200 BYTE) no no
COUNTRY CHAR(2 BYTE) no no
CREATED_AT TIMESTAMP(6) no no
CREDIT_LIMIT NUMBER(12,2) yes no'

# One line of the columns grid, as a regex: the literal text with runs of
# spaces standing for the gaps the grid pads with.
as_row() {
    printf '^ *%s' "$(sed -e 's/[][(){}.*+?^$|\\]/\\&/g' -e 's/ \+/ +/g' <<<"$1")"
}

# browse <label> <conn> <schema> <tab key> <kind...>
browse() {
    local label=$1 conn=$2 schema=$3 tab=$4
    shift 4
    local kinds=("$@") upper=${label^^} keys=$work/$label.keys
    local kind name var customers orders totals want got missing expected shown total
    var=$upper\_customers; customers=${!var}
    var=$upper\_orders; orders=${!var}
    var=$upper\_totals; totals=${!var}

    {
        [ "$tab" = 1 ] || echo "key $tab"
        echo "key c"
        echo "wait busy"
        # SQL Server lands in dbo, and the seeded schema is the one below it;
        # Oracle lands in the seeded schema itself, open already.
        if [ "$conn" = local-mssql ]; then
            echo "key G"
            echo "key l"
            echo "expect ▾ $schema"
        else
            echo "key g"
            echo "expect ▾ $schema"
        fi
        # Down to the last kind, then open them bottom up: every expansion
        # adds its rows below the cursor, so `k` lands on the kind above it
        # however many objects turned up.
        for kind in "${kinds[@]}"; do echo "key j"; done
        for kind in "${kinds[@]}"; do
            echo "key l"
            echo "wait busy"
            echo "key k"
        done
        echo "frame $label-tree"

        # The filter: what it keeps, what it says when it keeps nothing, and
        # that Esc gives the tree back from either. The first `/` lists every
        # schema's objects for it to search.
        echo "key /"
        echo "wait busy"
        echo "type cust"
        echo "expect ╭ Objects /cust ─"
        echo "expect $customers"
        echo "expect $totals"
        echo "expect-not ▸ $orders"
        echo "frame $label-narrow"
        echo "key Enter"
        echo "key Esc"
        echo "expect ▸ $orders"
        echo "key /"
        echo "type zzzznope"
        echo "expect no objects match"
        echo "frame $label-nomatch"
        echo "key Enter"
        echo "key Esc"
        echo "expect ▸ $orders"

        # i on customers: its columns in the grid. `schema.name`, since other
        # schemas have a customers too.
        echo "key /"
        echo "type $schema.$customers"
        echo "key Enter"
        echo "key G"
        echo "key i"
        echo "wait busy"
        echo "expect $schema.$customers columns"
        echo "frame $label-columns"
        echo "key Esc"

        # s on everything with source text, the filter putting the cursor on
        # it and G taking the one row it left.
        var=$upper\_sourced
        for name in ${!var}; do
            echo "key /"
            echo "type $schema.$name"
            echo "key Enter"
            echo "key G"
            echo "key s"
            echo "wait busy"
            echo "expect Source · $schema.$name ·"
            echo "frame $label-src-$name"
            echo "key Esc"
        done
        echo "key Ctrl-Q"
    } >"$keys"

    replay "$label" "$keys" || { bad "$label: the tree walk did not run"; return; }

    # Every kind: the subcommand lists what the seed made, and every name it
    # lists is a row of the tree.
    for kind in "${kinds[@]}"; do
        var=$upper\_$kind
        want=$(printf '%s\n' ${!var} | sort)
        got=$(listed "$conn" "$schema" "$kind")
        if [ "$want" != "$got" ]; then
            bad "$label objects --kind $kind listed [$(echo $got)], the seed made [$(echo $want)]"
            continue
        fi
        if [ -z "$want" ]; then
            ok "$label $kind: none seeded, none listed"
        elif missing=$(rows_have "$label-tree" $got); then
            ok "$label $kind: $(echo $got | wc -w) listed, every one of them a row of the tree"
        else
            bad "$label $kind: listed but never drawn: $missing"
        fi
    done

    # /cust kept the matches and the branches above them, and the replay's
    # own expect-not saw that it dropped orders.
    if missing=$(rows_have "$label-narrow" "$customers" "$totals"); then
        ok "$label /cust: $customers and $totals kept, $orders dropped"
    else
        bad "$label /cust dropped a row it matches: $missing"
    fi
    if grep -q 'no objects match' "$(frame "$label-nomatch")"; then
        ok "$label /zzzznope: no objects match, and Esc brought the tree back"
    else
        bad "$label /zzzznope did not say no objects match"
    fi

    # i: the columns, against what tests/catalog.rs says the seed declared.
    var=$upper\_columns
    while IFS= read -r expected; do
        if pane "$label-columns" ' columns · ' | grep -qE "$(as_row "$expected")"; then
            ok "$label i $customers: $expected"
        else
            bad "$label i on $customers has no row '$expected'"
        fi
    done <<<"${!var}"

    # s: the lines drawn, gutter stripped, against `source`. Blank lines go
    # from both sides — the pane pads with them below the last line, and
    # nothing distinguishes that padding from a blank line of the source.
    var=$upper\_sourced
    for name in ${!var}; do
        pane "$label-src-$name" '╭ Source · ' |
            sed -E 's/^ *[0-9]+ ?//' | sed '/^ *$/d' >"$work/$label-$name.shown"
        "$bin" source --conn "$conn" "$schema.$name" | tr -d '\r' >"$work/$label-$name.raw"
        sed -e 's/ *$//' -e '/^ *$/d' "$work/$label-$name.raw" >"$work/$label-$name.cli"
        shown=$(grep -c . <"$work/$label-$name.shown" || true)
        total=$(grep -c '' <"$work/$label-$name.raw" || true)
        if [ "$total" -gt "$SOURCE_LINES" ]; then
            bad "$label s $name: $total lines is more than the $SOURCE_LINES the pane shows at $SIZE"
        elif diff -u "$work/$label-$name.cli" "$work/$label-$name.shown" >"$work/$label-$name.diff"; then
            ok "$label s $name: $shown lines drawn, the same as source ($total of $total)"
        else
            bad "$label s $name differs from source:"
            cat "$work/$label-$name.diff" >&2
        fi
    done
}

healthy sql-bench-mssql || { echo "objects: sql-bench-mssql is not up" >&2; exit 1; }
healthy sql-bench-oracle || { echo "objects: sql-bench-oracle is not up" >&2; exit 1; }

browse mssql local-mssql bench 1 table view procedure function sequence
browse oracle local-oracle BENCH 2 table view procedure function package sequence

########################################################################
# A procedure created WITH ENCRYPTION says so.
########################################################################

sqlcmd -Q "
    SET NOCOUNT ON;
    IF OBJECT_ID('bench.sp_secret') IS NOT NULL DROP PROCEDURE bench.sp_secret;
    EXEC('CREATE PROCEDURE bench.sp_secret WITH ENCRYPTION AS SELECT 1 AS hidden');
" >/dev/null

cat >"$work/secret.keys" <<'KEYS'
key c
wait busy
key G
key l
expect ▾ bench
key j
key j
key j
key l
wait busy
# r asks the catalog again, which is how a procedure created since the
# branch was first opened turns up in it.
key r
wait busy
expect sp_secret
key /
type sp_secret
key Enter
key G
key s
wait busy
expect Source · bench.sp_secret ·
frame secret
key Ctrl-Q
KEYS
if replay secret "$work/secret.keys"; then
    if pane secret '╭ Source · ' | grep -q 'source not available (encrypted)'; then
        ok "mssql sp_secret: s says source not available (encrypted)"
    else
        bad "mssql sp_secret did not say the source is not available"
        pane secret '╭ Source · ' | head -3 >&2
    fi
    if "$bin" source --conn local-mssql bench.sp_secret 2>&1 |
        grep -q 'source not available (encrypted)'; then
        ok "mssql sp_secret: source prints the same"
    else
        bad "mssql source bench.sp_secret says something else"
    fi
else
    bad "mssql sp_secret: the replay did not run"
fi

########################################################################
# A thousand tables.
########################################################################

sqlcmd -Q "
    SET NOCOUNT ON;
    IF SCHEMA_ID('qa1000') IS NULL EXEC('CREATE SCHEMA qa1000');
    DECLARE @i int = 1, @sql nvarchar(400);
    WHILE @i <= 1000
    BEGIN
        SET @sql = CONCAT('CREATE TABLE qa1000.t', RIGHT(CONCAT('0000', @i), 4),
                          ' (id int NOT NULL PRIMARY KEY, name nvarchar(50) NULL)');
        EXEC(@sql);
        SET @i = @i + 1;
    END;
" >/dev/null

# Oracle gets them in BENCH under a prefix of its own: a second account would
# need grants before this login could see a single one of them in the tree.
sqlplus <<'SQL' >/dev/null
SET FEEDBACK OFF
BEGIN
  FOR i IN 1..1000 LOOP
    EXECUTE IMMEDIATE 'CREATE TABLE QA_T' || LPAD(i, 4, '0') ||
                      ' (id NUMBER(10) NOT NULL PRIMARY KEY, name NVARCHAR2(50))';
  END LOOP;
END;
/
EXIT
SQL

# The trace's own clock: the last frame before the catalog query that brought
# the rows back and the first one after it — the key that opened the branch
# to the branch on the screen.
expand_ms() { # trace rows
    awk -F'\t' -v want="rows=$2" '
        $2 == "query" { for (i = 3; i <= NF; i++) if ($i == want) seen = 1; next }
        $2 == "frame" { if (!seen) before = $1; else if (!after) after = $1 }
        END { if (seen && after) print after - before; else print "?" }
    ' "$1"
}

# Every draw after that query is a draw of the loaded branch, which is what
# scrolling through a thousand rows costs.
draws() { # trace rows -> p50 p95 max n
    awk -F'\t' -v want="rows=$2" '
        $2 == "query" { for (i = 3; i <= NF; i++) if ($i == want) seen = 1; next }
        seen && $2 == "frame" { sub(/^draw_ms=/, "", $3); print $3 }
    ' "$1" | sort -g |
        awk '{ a[NR] = $1 }
             END { if (NR == 0) { print "? ? ? 0"; exit }
                   printf "%s %s %s %d\n", a[int((NR + 1) / 2)], a[int((NR * 95 + 99) / 100)], a[NR], NR }'
}

# thousand <label> <tab key> <keys that open the branch> <rows> <last table>
thousand() {
    local label=$1 tab=$2 open=$3 rows=$4 last=$5
    local keys=$work/$label-1000.keys key ms p50 p95 max n missing

    {
        [ "$tab" = 1 ] || echo "key $tab"
        echo "key c"
        echo "wait busy"
        for key in $open; do echo "key $key"; done
        echo "wait busy"
        echo "frame $label-1000-open"
        # Down the thousand rows, ten at a time and then one at a time.
        for key in $(seq 1 90); do echo "key PageDown"; done
        for key in $(seq 1 60); do echo "key j"; done
        echo "key G"
        echo "frame $label-1000-end"
        echo "key Ctrl-Q"
    } >"$keys"

    replay "$label-1000" "$keys" || { bad "$label 1000 tables: the replay did not run"; return; }

    ms=$(expand_ms "$work/$label-1000.trace" "$rows")
    if [ "$ms" != "?" ] && [ "$ms" -lt 2000 ]; then
        ok "$label 1000 tables: the branch opened in ${ms} ms (budget 2000)"
    else
        bad "$label 1000 tables: opening took ${ms} ms"
    fi

    read -r p50 p95 max n <<<"$(draws "$work/$label-1000.trace" "$rows")"
    if [ "$n" -ge 100 ] && awk -v p="$p95" 'BEGIN { exit !(p < 5) }'; then
        ok "$label 1000 tables: $n draws scrolling, draw_ms p50 $p50 p95 $p95 max $max (budget 5)"
    else
        bad "$label 1000 tables: $n draws scrolling, draw_ms p50 $p50 p95 $p95 max $max (budget 5)"
    fi

    if missing=$(rows_have "$label-1000-end" "$last"); then
        ok "$label 1000 tables: $last is drawn at the end of the branch"
    else
        bad "$label 1000 tables: $missing is never drawn"
    fi
}

# SQL Server: qa1000 is the schema after bench and Tables the kind below it.
# Oracle: BENCH is open already, and Tables is its first kind.
thousand mssql 1 'G l j l' 1000 t1000
thousand oracle 2 'g j l' 1007 QA_T1000

########################################################################
# Back to the seed.
########################################################################

teardown
counts=$(
    "$bin" objects --conn local-mssql --schema bench | sed 1,2d | grep -c . || true
    "$bin" objects --conn local-oracle --schema BENCH | sed 1,2d | grep -c . || true
)
if [ "$(echo $counts)" = "14 14" ]; then
    ok "both databases are back to the seed: 14 objects in each"
else
    bad "the databases hold [$(echo $counts)] objects, the seed is [14 14]"
fi

exit "$fail"
