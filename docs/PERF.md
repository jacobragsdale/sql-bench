# sql-bench performance

Measured numbers and the machine they came off. The budgets they are
measured against are in [DESIGN.md](DESIGN.md); *(T7.1)* fills in the rest
of this file once there is an app to time.

## Environment

Measured 2026-09-14 on the development machine:

| | |
|---|---|
| CPU | AMD Ryzen 7 8745HS, 16 threads (`nproc`) |
| memory | 27 GiB total (`free -h`) |
| OS | Arch Linux, kernel 7.2.3 |
| docker | 29.8.0, compose 5.5.1 |
| rust | 1.98.0 |

`scripts/db-reset.sh` — both containers destroyed with their volumes,
recreated, waited for and seeded — took **46 s** wall clock, images already
pulled. Most of it is the two servers starting: the seeding itself is the
tail.

`db-up.sh` prints its own seed time at the end of each line. Into empty
databases that was 1 s for SQL Server and 9 s for Oracle — the same
1,000,000-row `events` table either way, and one set-based insert either
way; Oracle is simply slower at it. Run again against databases that are
already seeded, both drop to 0-1 s, since both seed files skip what already
exists.

## Queries

Against the containers `scripts/db-up.sh` seeds. Milliseconds, nearest-rank
percentiles; `connect` is the one connect each run of `bench` makes, `rows/s`
is over every run. T7.1 moved these tables under **Budgets** below, where
they are the evidence for the two query budgets; this block is what the
script recorded before that.

### 2026-09-14 (a7d0bf8) — 20 runs, 5 for the 100k scans

**local-mssql**

| query | rows | connect | first_row p50 | first_row p95 | total p50 | total p95 | rows/s |
|---|---|---|---|---|---|---|---|
| select 1 | 1 | 5 | 0 | 0 | 0 | 0 | 20000 |
| customers, top 100 | 50 | 4 | 0 | 0 | 0 | 0 | 1000000 |
| events, 10k cap | 10000 | 4 | 3 | 5 | 13 | 14 | 787401 |
| events, 100k cap | 100000 | 5 | 4 | 5 | 95 | 106 | 1026694 |

**local-oracle**

| query | rows | connect | first_row p50 | first_row p95 | total p50 | total p95 | rows/s |
|---|---|---|---|---|---|---|---|
| select 1 | 1 | 39 | 0 | 0 | 0 | 0 | 20000 |
| customers, top 100 | 50 | 38 | 0 | 0 | 0 | 0 | 333333 |
| events, 10k cap | 10000 | 38 | 0 | 0 | 8 | 10 | 1242236 |
| events, 100k cap | 100000 | 38 | 0 | 1 | 77 | 102 | 1216545 |

## Driver conformance (T2.5)

`scripts/qa/max-rows-timing.sh`, release build, 2026-09-14. Wall clock of a
whole `sql-bench query --max-rows N --format csv 'select * from bench.events'`
— process start, connect, fetch and formatting — against the 1,000,000 row
table, best of three runs each:

| backend | 10,000 rows (budget 2 s) | 100,000 rows (budget 8 s) | the whole table, for contrast |
|---|---|---|---|
| local-mssql | 21 ms | 128 ms | 1256 ms |
| local-oracle | 57 ms | 161 ms | 1428 ms |

Ten times the cap costs ten times the wall clock once the fixed cost of
starting a process and connecting is out of the way — 128 ms to 1256 ms on
SQL Server, 161 ms to 1428 ms on Oracle — which is what it looks like when
the cap stops the fetch rather than the printing. Rows past `max_rows` are
never pulled off the socket: SQL Server drops the connection outright and
Oracle closes the cursor.

Unreachable host, `10.255.255.1:1433` and `:1521` (measured the same way):
SQL Server gives up after **10.0 s** and Oracle after **10.05 s**, both
inside the ten second budget — the assertion lives in
`an_unreachable_host_gives_up_inside_the_connect_timeout` in `tests/mssql.rs`
and `tests/oracle.rs`.

## Shell

`scripts/qa/draw-latency.sh` replays 100 keys at 200x60 — every key its own
frame, 101 of them — and reads `draw_ms` off the trace's `frame` lines. The
shell it draws is the placeholder one: a tab bar, three bordered panes and a
footer. Median of five runs on the machine above, in milliseconds:

| build | p50 | p95 | max |
|---|---|---|---|
| release | 0.15 | 0.17 | 0.43 |
| debug | 1.97 | 2.11 | 4.5 |

The budget is 5 ms, and the release build is what it is held to. A debug
build does the same work through an unoptimised ratatui and is an order of
magnitude slower, so `scripts/qa.sh` measures it against 16 ms instead:
enough to catch a draw that got expensive, not enough to fail on a busy
runner.

`draw_ms` carries three decimals since T3.3. A draw of this shell is a
fraction of a millisecond, and a number rounded to whole ones cannot be held
to a 5 ms budget — every frame above would have read 0, 1 or 2.

## Query workflow (T5.4)

`scripts/qa/query-workflow.sh`, release build, 2026-09-15, on the machine
above. Twelve cases through the replay scripts in `scripts/replay/qa/`,
against both containers: unicode in the SQL and in the rows, a 300-column
result, a 100 kB cell, NULL-only rows, F5 over a failing second statement,
Ctrl-R on an empty pad, Esc the instant after Ctrl-R, Ctrl-R on a
disconnected tab, a container stopped under a connected one, a million-row
scan capped at 100,000 and exported, the grid against
[TYPES.md](TYPES.md), and CJK alignment. Wall clock of the whole replay
process — start, connect, run, keys, frames and exit — median of three runs.

| case | local-mssql | local-oracle | budget |
|---|---|---|---|
| unicode, 300 columns, NULL-only rows, `all_types`, CJK (one script) | 263 ms | 311 ms | — |
| 100 kB cell: 16 frames of j/k, worst `draw_ms` | 0.861 | 0.644 | 16 ms |
| Esc straight after Ctrl-R, then another statement | 168 ms | 193 ms | — |
| Ctrl-R on a disconnected tab: connect and rows | 68 ms | 112 ms | — |
| 1M rows at `--max-rows 100000`, `G`, `PageUp` ×5, CSV export | 209 ms | 249 ms | 15 s |
| the same run's worst `draw_ms` after the scan was done | 0.490 | 0.308 | 16 ms |

The million-row case is the one with a budget worth stating: 100,000 rows
fetched, the cell cursor walked to the bottom and five pages back up, and
100,000 rows written to a CSV, all inside a quarter of a second — two orders
of magnitude under the 15 s the ticket allowed. Both draw budgets are met by
a factor of twenty: the grid formats the window it shows and never the scan,
so a 102,400 character cell costs the 40 columns it is cut to.

The outage case (T4.2's deferred one) runs last and one container at a time:
each was stopped for **10 s** — the wait its replay script spends — and
docker called it healthy again **16 s** after it went down. The query that
crossed the outage failed with the driver's own words (`cannot connect: An
error occured during the attempt of performing I/O` on SQL Server,
`DPI-1080: connection was closed by ORA-03113` on Oracle) rather than
hanging, and a Ctrl-R on a disconnected tab afterwards connected and returned
rows in 65 ms and 111 ms. The whole script, both backends and both outages,
is 35 s.

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

