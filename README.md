# sql-bench

[![CI](https://github.com/jacobragsdale/sql-bench/actions/workflows/ci.yml/badge.svg)](https://github.com/jacobragsdale/sql-bench/actions/workflows/ci.yml)

A fast, lightweight terminal workbench for SQL Server and Oracle: connect to
both, run SQL from a scratch pad, browse schemas and read the source of a
procedure, without leaving the terminal. It is one Rust binary that never
blocks on a database — every driver call runs on a worker thread, and the grid
formats the window it shows and not the scan behind it, so a million rows cost
what a screenful costs. It is the sibling of
[ticket-tui](https://github.com/jacobragsdale/ticket-tui) and az-tui: same
stack, same layout, same keys.

<!-- frame:start -->
```
 1 local-mssql ●  2 local-oracle ○                                                           ? Help
╭ Objects ──────── / Filter ─╮╭ Scratch ─────────────────────────────── ✎ Editor ─ ▶▶ All ─ ▶ Run ─╮
│ ▾ dbo                      ││ 1 select id, name, country, credit_limit                           │
│   ▸ Tables                 ││ 2 from bench.customers where country in ('DE', 'NO', 'IE')         │
│   ▸ Views                  ││                                                                    │
│   ▸ Procedures             ││                                                                    │
│   ▸ Functions              ││                                                                    │
│   ▸ Sequences              ││                                                                    │
│ ▾ bench                    ││                                                                    │
│   ▾ Tables                 ││                                                                    │
│     ▸ all_types            │╰────────────────────────────────────────────────────────────────────╯
│     ▸ big_text             │╭ Results · 15 rows · 1 ms ───────────────────────────────── Export ─╮
│     ▸ binary_blobs         ││ id   name          country  credit_limit                           │
│     ▸ customers            ││ int  nvarchar      char     decimal                                │
│     ▸ events               ││   1  Zoë Bauer     DE             100.50                           ┃
│     ▸ order_items          ││   2  Ægir Nilsen   NO             201.00                           ┃
│     ▸ orders               ││   8  Mary O'Neill  IE             804.00                           ┃
│   ▸ Views                  ││  11  Zoë Bauer     DE            1105.50                           ┃
│   ▸ Procedures             ││  12  Ægir Nilsen   NO            1206.00                           ┃
│   ▸ Functions              ││  18  Mary O'Neill  IE            1809.00                           ┃
│   ▸ Sequences              ││  21  Zoë Bauer     DE               NULL                           ┃
│                            ││  22  Ægir Nilsen   NO            2211.00                           ┃
│                            ││  28  Mary O'Neill  IE               NULL                           ┃
│                            ││  31  Zoë Bauer     DE            3115.50                           │
│                            ││  32  Ægir Nilsen   NO            3216.00                           │
│                            ││  38  Mary O'Neill  IE            3819.00                           │
╰────────────────────────────╯╰────────────────────────────────────────────────────────────────────╯
 Shift-Tab previous pane  Ctrl-T next tab  Ctrl-R run the statement  F5 run all    ● connected 20ms
```
<!-- frame:end -->

The frame above is not a drawing. `scripts/readme-frame.sh` replays
`scripts/replay/readme.keys` against the local SQL Server and writes whatever
the app drew into this file, so the picture goes stale the moment the layout
does and the check says so.

## Install

```sh
cargo install --path .
```

SQL Server needs nothing else: tiberius is pure Rust, so there is no ODBC
manager, no vendor driver and no OpenSSL to find.

Oracle connections load the Oracle Instant Client at run time. On Linux,
`scripts/oracle-client.sh` downloads the free basiclite client to
`~/.local/opt/oracle`; on macOS take the basiclite dmg from
[Oracle's download page](https://www.oracle.com/database/technologies/instant-client/downloads.html)
and unpack it yourself. Point `[oracle] client_lib_dir` in the config at the
directory, or set `SQL_BENCH_ORACLE_CLIENT_DIR`. Nothing is needed at build
time — a machine that never opens an Oracle connection never loads it.

A zip or a dmg install has no run path, so `libclntsh.so` cannot find its own
libraries sitting next to it and every Oracle connection fails with *Oracle
client library not found*. `scripts/oracle-client.sh` patches the run path in
(`patchelf`, through `uvx` if it is not installed), which is why
`client_lib_dir` is then enough on its own. Without patchelf, make the
directory findable the system way instead: name it in
`/etc/ld.so.conf.d/oracle.conf` and run `ldconfig`, or export
`LD_LIBRARY_PATH` — for `cargo test` too. Arch also needs `libaio`.

## Configuration

`~/.config/sql-bench/config.toml`, or whatever `$SQL_BENCH_CONFIG` names, or
whatever `--config` names. Copy [config.example.toml](config.example.toml) and
edit it; a missing file is an empty configuration, not an error.

```toml
[oracle]
client_lib_dir = "~/.local/opt/oracle/instantclient_23_26"

[[connection]]
name = "reporting"
kind = "mssql"
host = "sql01.example.com"
database = "Reporting"
user = "svc_reports"
password_env = "REPORTING_PASSWORD"
```

`[oracle] client_lib_dir` is where the Instant Client is; left out, the driver
looks wherever the system linker does. A leading `~` is the home directory.

One `[[connection]]` per database, in the order the tabs appear.

| key | means |
|---|---|
| `name` | unique, and what `--conn` and the tab bar say |
| `kind` | `mssql` or `oracle` |
| `host` | the server |
| `port` | left out: 1433 for `mssql`, 1521 for `oracle` |
| `database` | SQL Server only, and required for it |
| `service` | Oracle only, and required for it: the service name, not the SID |
| `user` | the login |
| `password` | the password, in the file |
| `password_env` | the variable holding it, read when the connection is opened |
| `password_cmd` | a command printing it, run through `sh -c`, trailing newline dropped |
| `trust_cert` | SQL Server only; `true` accepts a self-signed certificate |
| `encrypt` | SQL Server only; `true` is the default |

Exactly one of `password`, `password_env` and `password_cmd` per connection. A
password never reaches a trace file or the screen.

## Keys

`?` opens the same list in the app, for the focused pane only.

Anywhere:

| Key | Does |
|---|---|
| `Shift-Tab` | previous pane |
| `Ctrl-T` | next tab |
| `?` | help |
| `Esc` | cancel or close help |
| `Ctrl-Q` | quit |

Anywhere but the scratch pad, which types them instead:

| Key | Does |
|---|---|
| `Tab` | next pane |
| `1-9` | select tab |
| `c` | connect |
| `C` | disconnect |
| `q` | quit |

Scratch:

| Key | Does |
|---|---|
| `Ctrl-R` | run the statement |
| `F5` | run all |
| `Ctrl-E` | edit in $EDITOR |
| `Ctrl-Z` | undo the last edits |
| `Ctrl-C` | copy the selection |
| `Shift-Arrows` | select |
| `Tab` | two spaces |
| `Home` | line start |
| `End` | line end |
| `PageDown` | page down |
| `PageUp` | page up |
| `Ctrl-A` | line start |
| `Ctrl-U` | delete to line start |
| `Ctrl-K` | delete to line end |
| `Ctrl-W` | delete the word before |
| `Ctrl-Left` | word left |
| `Ctrl-Right` | word right |

Objects:

| Key | Does |
|---|---|
| `j` | down |
| `k` | up |
| `l` | expand or open |
| `h` | collapse or go up |
| `Space` | expand or collapse |
| `Enter` | select from it |
| `s` | source |
| `i` | columns |
| `r` | reload |
| `/` | filter |
| `y` | copy the name |
| `Arrows` | move about the tree |
| `g` | first row |
| `G` | last row |
| `PageDown` | page down |
| `PageUp` | page up |

Results:

| Key | Does |
|---|---|
| `j` | row down |
| `k` | row up |
| `h` | column left |
| `l` | column right |
| `Arrows` | move the cell cursor |
| `PageDown` | page down |
| `PageUp` | page up |
| `Ctrl-D` | half a page down |
| `Ctrl-U` | half a page up |
| `g` | first row |
| `G` | last row |
| `0` | first column |
| `$` | last column |
| `[` | previous result set |
| `]` | next result set |
| `m` | 10,000 more rows |
| `Enter` | inspect the cell |
| `y` | copy the cell |
| `Y` | copy the row |
| `e` | export the result set |
| `o` | sort by the column |

The scratch pad is one pad per connection, kept in
`~/.local/state/sql-bench/scratch/<connection>.sql` and written half a second
after the last edit; `$SQL_BENCH_STATE_DIR` moves the directory. `NO_COLOR`
turns colour off.

## Headless

A bare `sql-bench` opens the TUI. Every feature is also reachable without it,
which is how the project verifies itself:

```sh
sql-bench query   --conn local-mssql 'select top 10 * from bench.customers'
sql-bench objects --conn local-mssql --schema bench --kind procedure
sql-bench source  --conn local-mssql bench.sp_customer_orders
sql-bench bench   --conn local-mssql --runs 20 'select 1'
```

`query` prints an aligned table, or `--format csv`, or `--format json`; its
`N rows in X ms` footer goes to stderr, so a pipeline reads only the rows.
`--max-rows N` (10,000 by default) stops the fetch and not just the printing,
`--timeout SECONDS` (30 by default) cancels a query that has not finished, and
`--full` prints whole cells instead of cutting them at 60 terminal columns. A
`-` in place of the statement reads it from stdin.

```sh
sql-bench query --conn local-oracle --format json --max-rows 100 --timeout 5 -
```

`objects` lists tables, views, procedures, functions, packages and sequences,
narrowed by `--schema`, by `--kind` and by a pattern the name has to contain.
`source` prints the text of one schema-qualified object, or the columns of one
if it is a table. `bench` runs a statement `--runs N` times through one
ordinary connection and prints min/p50/p95/max in milliseconds for connect,
first row and total, and the rows per second.

An exit code of 1 is the database or the configuration saying no, with the
server's own message on stderr and nothing at all on stdout.
`SQL_BENCH_TRACE=<file>` appends one tab-separated line per connect, query and
frame; unset, no clock is read.

## Replay

`sql-bench --replay FILE --size 100x28 --frames-dir DIR` drives the real loop,
the real app and the real rendering against a `ratatui::backend::TestBackend`,
taking its keys from a small script instead of a keyboard and writing frames as
plain text. That is how every feature here is checked, the picture at the top
of this file included — the grammar, the key names, the exit codes and the
frame format are in [docs/DESIGN.md](docs/DESIGN.md).

## Local databases for development

Both run in Docker, from `compose.yaml`; the first `db-up.sh` pulls two large
images, and a reset from empty takes about a minute.

```sh
scripts/db-up.sh     # SQL Server 2022 and Oracle 23ai Free, started and seeded
scripts/db-down.sh   # stop them; -v drops the volumes too
scripts/db-reset.sh  # throw both away and seed again
```

The seed in `scripts/seed/` writes the same shape into each: `customers`,
`orders` and `order_items`, a 1,000,000-row `events` table for the scans, an
`all_types` table with a column of every type the server has, a 100 kB text
body, a binary blob, and the views, procedures, functions and sequences the
object tree browses. Both files are re-runnable and skip whatever is already
there.

| | host | port | database or service | user | password |
|---|---|---|---|---|---|
| SQL Server 2022 Developer | localhost | 1433 | bench | sa | Bench_Pass1! |
| Oracle 23ai Free | localhost | 1521 | FREEPDB1 | bench | bench |

`config.local.toml` at the repo root names both, as `local-mssql` and
`local-oracle`. These passwords open nothing but a throwaway database on
localhost, which is why the file is committed.

```sh
SQL_BENCH_CONFIG=config.local.toml cargo run
```

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
scripts/qa.sh
```

`scripts/qa.sh` is everything that needs no database: the replay scripts at
five terminal sizes, a resize mid-run, the footer hints following the focus,
the quit keys from every pane, `NO_COLOR`, the scratch pad coming back after a
restart, the terminal given back after a panic, and the draw latency. CI runs
all four.

With the containers up, `SQL_BENCH_TEST_DBS=1` turns on the rest — the
integration tests in `tests/`, which return early without it, and the half of
`scripts/qa.sh` that needs a server, the README frame included:

```sh
SQL_BENCH_TEST_DBS=1 cargo test --all-targets
SQL_BENCH_TEST_DBS=1 scripts/qa.sh
scripts/readme-frame.sh          # rewrite the frame at the top of this file
scripts/readme-frame.sh --check  # or just say whether it is current
```

[docs/DESIGN.md](docs/DESIGN.md) is how all of it works and why, in full;
[docs/TYPES.md](docs/TYPES.md) says what every database type becomes,
[docs/PERF.md](docs/PERF.md) holds the measured numbers, and
[docs/backlog.yaml](docs/backlog.yaml) is the work breakdown.

## License

[MIT](LICENSE).
