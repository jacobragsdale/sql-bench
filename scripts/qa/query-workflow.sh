#!/usr/bin/env bash
# T5.4: the write-run-read-export loop end to end, on both databases, through
# the replay scripts in scripts/replay/qa/.
#
# Twelve cases: unicode in the SQL and in the rows, three hundred columns, a
# hundred kilobyte cell that may not slow scrolling, NULL-only rows, F5 over a
# pad whose second statement fails, Ctrl-R on an empty pad, Esc the instant
# after Ctrl-R, Ctrl-R on a disconnected tab, a container stopped under a
# connected one, a million row scan capped at a hundred thousand and exported,
# the grid against docs/TYPES.md, and CJK alignment.
#
# Every script runs against a config holding one connection, so it is tab 1
# whichever backend it is pointed at, and with a state directory of its own so
# the pad starts empty. Budgets: no frame over 16 ms while the grid is being
# read, and the million row case under 15 s all in.
#
# The outage case is last and one backend at a time: the container is stopped
# for the ten seconds its script waits and the failed connect after it, and is
# started again the moment that script is done — the trap starts both whatever
# goes wrong.
#
# Knobs: SQL_BENCH_BIN (default: the release build), SQL_BENCH_QA_SKIP_OUTAGE.
set -euo pipefail
cd "$(dirname "$0")/../.."

bin=${SQL_BENCH_BIN:-target/release/sql-bench}
[ -x "$bin" ] || cargo build --release --quiet

work=$(mktemp -d)
# Both containers are left running however this ends, including a `set -e`.
trap 'docker start sql-bench-mssql sql-bench-oracle >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
fail=0

note() { printf '   %s\n' "$*"; }
bad() { printf '   FAIL: %s\n' "$*" >&2; fail=1; }

# One `[[connection]]` and the Oracle client directory, so every script is
# tab 1 whichever backend it is run against (scripts/qa/connections.sh).
one_connection() { # name
    awk -v want="name = \"$1\"" 'BEGIN { RS = ""; ORS = "\n\n" } /^\[oracle\]/ || index($0, want)' \
        config.local.toml
}

# Blocks until docker calls the container healthy again (scripts/qa/db-outage.sh).
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
# recovering when the healthcheck's `select 1` against master answers
# (scripts/qa/db-outage.sh says the same).
answering() { # conn-config sql [seconds]
    local config=$1 sql=$2 deadline=$(( SECONDS + ${3:-180} ))
    while :; do
        if SQL_BENCH_CONFIG=$config "$bin" query --conn "$conn" "$sql" >/dev/null 2>&1; then
            return 0
        fi
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 2
    done
}

# One replay run, traced, against the backend `conn` names. Sets `frames`,
# `trace`, `elapsed` and `status`.
replay() { # label script size [extra args...]
    local label=$1-$conn script=$2 size=$3
    shift 3
    frames=$work/$label
    trace=$work/$label.trace
    rm -rf "$frames" "$trace"
    mkdir -p "$frames"
    local start
    status=0
    start=$(date +%s%3N)
    SQL_BENCH_CONFIG=$work/$conn.toml SQL_BENCH_STATE_DIR=$work/$label.state \
        SQL_BENCH_TRACE=$trace "$bin" --replay "$script" --size "$size" \
        --frames-dir "$frames" "$@" >"$work/$label.out" 2>"$work/$label.err" || status=$?
    elapsed=$(( $(date +%s%3N) - start ))
    [ "$status" -eq 0 ] ||
        bad "$label: the replay exited $status: $(tail -3 "$work/$label.err")"
}

# The biggest draw_ms the trace holds, counting only the frames drawn after
# the query was done — which are the ones a person's keys paid for.
after_query_draw_max() { # trace
    awk -F'\t' '$2 == "query" { seen = 1 }
                seen && $2 == "frame" { sub(/^draw_ms=/, "", $3); if ($3 > max) max = $3 }
                END { printf "%.3f", max }' "$1"
}

over_budget() { # label max budget
    awk -v m="$2" -v b="$3" 'BEGIN { exit !(m < b) }' ||
        bad "$1: a frame took ${2}ms, over the ${3}ms budget"
}

echo "== cases 1, 2, 4, 11, 12: unicode, 300 columns, NULLs, types, alignment"
for conn in local-mssql local-oracle; do
    one_connection "$conn" >"$work/$conn.toml"
    replay query-cases "scripts/replay/qa/query-cases-${conn#local-}.keys" 400x24
    [ "$status" -eq 0 ] || continue
    note "$conn: unicode, 300 columns, NULL-only rows and every type in ${elapsed}ms"
    note "$conn: like '%ë%' returned$(grep -o ' Zoë Bauer *DE' "$frames/unicode.txt" | head -1)," \
        "the 300th column reads $(sed -n 's/.*│ \(C300\|c300\) .*/\1/p' "$frames/wide.txt" | head -1)," \
        "NULLs read $(sed -n 's/.*│ \(NULL *NULL\).*/\1/p' "$frames/nulls.txt" | head -1)"

    # Case 12: the column after 李雷 has to start where it starts on every
    # other row. ratatui blanks the cell a wide glyph covers, so one character
    # of a frame is one terminal column.
    python3 - "$frames/cjk.txt" <<'PY' || bad "$conn: the grid leans on a CJK name"
import sys

frame = open(sys.argv[1], encoding="utf-8").read().split("\n")
# The grid's rows are the lines holding an email cell: the pad above it holds
# the statement, which has no @ in it.
rows = [line for line in frame if "@example.com" in line]
header = [line for line in frame
          if "email" in line.lower() and "@" not in line and "select" not in line]
if len(rows) != 5 or not header:
    sys.exit(f"{len(rows)} rows and {len(header)} headers of the grid on the frame")
# ratatui blanks the cell a wide glyph covers, so one character of a frame is
# one terminal column: the email cell starts where the header says it does, or
# the name two columns per glyph before it has pushed the rest along.
offsets = {row.index("customer") for row in rows} | {header[0].lower().index("email")}
print(f"   the email column starts at {sorted(offsets)} on the header and {len(rows)} rows")
if len(offsets) != 1:
    sys.exit("the columns lean")
PY

    # Case 11: the displays docs/TYPES.md documents, looked for on the two
    # frames the grid was walked across.
    python3 - "$conn" docs/TYPES.md "$frames/types-left.txt" "$frames/types-right.txt" \
        <<'PY' || bad "$conn: the grid does not read the way docs/TYPES.md says"
import sys

conn, doc, *frames = sys.argv[1:]
section = "SQL Server" if conn.endswith("mssql") else "Oracle"
examples, inside = [], False
for line in open(doc, encoding="utf-8"):
    if line.startswith("## "):
        inside = line[3:].strip() == section
        continue
    if inside and line.startswith("|") and line.count("|") == 4:
        type_name, _, example = (part.strip(" `") for part in line.split("|")[1:4])
        if example not in ("example display", "") and set(example) != {"-"}:
            examples.append((type_name, example))
screen = "\n".join(open(frame, encoding="utf-8").read() for frame in frames)
found = [name for name, example in examples if f" {example} " in screen]
missing = [f"{name}={example}" for name, example in examples if f" {example} " not in screen]
print(f"   {len(found)} of {len(examples)} documented displays on the frames"
      + (f"; not shown: {', '.join(missing)}" if missing else ""))
if len(found) < 6:
    sys.exit(f"only {len(found)} documented displays: {found}")
PY
done

echo "== case 3: a 100 kB cell in the grid"
for conn in local-mssql local-oracle; do
    replay big-cell scripts/replay/qa/big-cell.keys 120x40
    [ "$status" -eq 0 ] || continue
    grep -q '…' "$frames/big-cell.txt" || bad "$conn: the big cell is not cut with an ellipsis"
    draw=$(after_query_draw_max "$trace")
    frames_drawn=$(awk -F'\t' '$2 == "query" { seen = 1 } seen && $2 == "frame"' "$trace" | wc -l)
    note "$conn: $frames_drawn frames scrolling the 102,400 character cell, worst draw_ms $draw"
    over_budget "$conn big-cell" "$draw" 16
done

echo "== case 5: F5 over a pad whose second statement fails"
for conn in local-mssql local-oracle; do
    replay fail-second "scripts/replay/qa/fail-second-${conn#local-}.keys" 120x40 --frame-styles
    [ "$status" -eq 0 ] || continue
    note "$conn: $(grep -o 'statement 2 of 3 failed' "$frames/fail-second.txt")," \
        "$(sed -n 's/.*│ \(line 1: .*\)/\1/p;s/.*│ \(ORA-.*\)/\1/p' "$frames/fail-second.txt" | head -1 | cut -c1-56)"
    grep -q 'bg=Red' "$frames/fail-second.styles.txt" ||
        bad "$conn: the failed statement's lines are not painted in the error background"
done

echo "== case 6: Ctrl-R and F5 on an empty pad"
for conn in local-mssql local-oracle; do
    replay empty-pad scripts/replay/qa/empty-pad.keys 120x40
    [ "$status" -eq 0 ] || continue
    note "$conn: Ctrl-R:$(tail -1 "$frames/empty-ctrl-r.txt" | cut -c1-40)," \
        "F5:$(tail -1 "$frames/empty-f5.txt" | cut -c1-24)"
done

echo "== case 7: Esc the instant after Ctrl-R"
for conn in local-mssql local-oracle; do
    replay cancel-race "scripts/replay/qa/cancel-race-${conn#local-}.keys" 120x40
    [ "$status" -eq 0 ] || continue
    note "$conn: $(grep -o 'cancelled after [0-9.]* s, [0-9,]* rows' "$frames/cancelled.txt")," \
        "the next statement ran, ${elapsed}ms all in"
done

echo "== case 8: Ctrl-R on a disconnected tab"
for conn in local-mssql local-oracle; do
    replay run-disconnected scripts/replay/qa/run-disconnected.keys 120x40
    [ "$status" -eq 0 ] || continue
    note "$conn: $(tail -1 "$frames/connecting.txt" | cut -c1-30), then rows in ${elapsed}ms"
done

echo "== case 10: a million rows at a cap of 100,000, paged and exported"
out=/tmp/sql-bench-qa-million.csv
for conn in local-mssql local-oracle; do
    rm -f "$out"
    replay million-export scripts/replay/qa/million-export.keys 120x40 --max-rows 100000
    [ "$status" -eq 0 ] || continue
    rows=$(wc -l <"$out")
    draw=$(after_query_draw_max "$trace")
    note "$conn: ${elapsed}ms all in, worst draw_ms after the scan $draw, $rows CSV lines"
    [ "$rows" = 100001 ] || bad "$conn: $rows lines in $out, wanted 100001 (a header and the rows)"
    [ "$elapsed" -lt 15000 ] || bad "$conn: the run took ${elapsed}ms, over the 15000ms budget"
    over_budget "$conn million-export" "$draw" 16
done
rm -f "$out"

# Last, and one container at a time: the case T4.2 deferred.
if [ "${SQL_BENCH_QA_SKIP_OUTAGE:-}" = 1 ]; then
    echo "== case 9 skipped (SQL_BENCH_QA_SKIP_OUTAGE=1)"
else
    echo "== case 9: the container stopped under a connected tab, and back"
    outage() { # container conn alive-sql
        local name=$1 down stopped replay_pid deadline outage_status=0
        conn=$2
        local alive=$3 dir=$work/outage-$conn
        rm -rf "$dir"
        mkdir -p "$dir"
        SQL_BENCH_CONFIG=$work/$conn.toml SQL_BENCH_STATE_DIR=$work/outage-$conn.state \
            "$bin" --replay scripts/replay/qa/outage-run.keys --size 120x40 --frames-dir "$dir" \
            >"$work/outage-$conn.out" 2>"$work/outage-$conn.err" &
        replay_pid=$!
        # The container is stopped only once the tab has run something, which
        # is what `connected.txt` says; the script then has ten seconds.
        deadline=$(( SECONDS + 90 ))
        while [ ! -f "$dir/connected.txt" ]; do
            if ! kill -0 "$replay_pid" 2>/dev/null || [ "$SECONDS" -ge "$deadline" ]; then
                bad "$conn: nothing ran before the outage: $(tail -3 "$work/outage-$conn.err")"
                wait "$replay_pid" || true
                return
            fi
            sleep 0.2
        done
        down=$SECONDS
        # `-t 5`: SQL Server does not go down inside docker's default ten
        # seconds (scripts/qa/connections.sh).
        docker stop -t 5 "$name" >/dev/null
        stopped=$(( SECONDS - down ))
        wait "$replay_pid" || outage_status=$?
        docker start "$name" >/dev/null
        note "$name was down for $(( SECONDS - down ))s (${stopped}s of it stopping)"
        if [ "$outage_status" -ne 0 ]; then
            bad "$conn: the query against the stopped $name exited $outage_status:" \
                "$(tail -3 "$work/outage-$conn.err")"
        else
            note "$conn: the pane said$(sed -n 's/.*│ \(cannot connect[^│]*\).*/ \1/p;s/.*│ \(line [0-9]*: [^│]*\).*/ \1/p' "$dir/outage-error.txt" | head -1 | cut -c1-72)"
        fi
        healthy "$name" || { bad "$name never came back healthy"; return; }
        note "$name healthy again $(( SECONDS - down ))s after it went down"
        answering "$work/$conn.toml" "$alive" || { bad "$conn never answered again"; return; }
        # The other half: Ctrl-R on a tab nobody has connected, against the
        # container that has just come back.
        replay after-outage scripts/replay/qa/run-disconnected.keys 120x40
        [ "$status" -eq 0 ] &&
            note "$conn: Ctrl-R after the restart connected and returned rows in ${elapsed}ms"
    }
    outage sql-bench-mssql local-mssql 'select 1 as one'
    outage sql-bench-oracle local-oracle 'select 1 as one from dual'
fi

echo "== both containers"
for name in sql-bench-mssql sql-bench-oracle; do
    state=$(docker inspect --format '{{.State.Status}}/{{.State.Health.Status}}' "$name")
    note "$name: $state"
    [ "$state" = running/healthy ] || bad "$name is $state"
done
exit "$fail"
