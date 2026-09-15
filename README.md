# sql-bench

A fast, lightweight terminal workbench for SQL Server and Oracle: connect,
run queries from a scratch pad, browse database objects and read stored
procedure source, without leaving the terminal. Rust, ratatui, one binary.

It is the sibling of [ticket-tui](https://github.com/jacobragsdale/ticket-tui)
— same stack, same layout, same keys.

## Build

```sh
cargo build --release   # target/release/sql-bench
cargo test --all-targets
```

Oracle connections load the Oracle Instant Client at runtime; point
`[oracle] client_lib_dir` in the config at it, or set
`SQL_BENCH_ORACLE_CLIENT_DIR`. Nothing is needed at build time.

On Linux, `scripts/oracle-client.sh` downloads the free basiclite client to
`~/.local/opt/oracle` and, with `patchelf` on the path (or `uvx`), gives it
the run path a zip install lacks, so `client_lib_dir` alone is enough.
Without patchelf the client's own libraries still have to be findable: put
the directory in `/etc/ld.so.conf.d/` and run `ldconfig`, or export
`LD_LIBRARY_PATH` (for `cargo test` too). Either way every Oracle connection
otherwise fails with *Oracle client library not found*. Arch needs `libaio`.

## Local databases

Both run in Docker, from `compose.yaml`; the first `db-up.sh` pulls two
large images.

```sh
scripts/db-up.sh     # SQL Server 2022 and Oracle 23ai Free, seeded
scripts/db-down.sh
scripts/db-reset.sh  # throw both away and seed again, about a minute
```

`config.local.toml` in this repo names both as `local-mssql` and
`local-oracle`:

```sh
SQL_BENCH_CONFIG=config.local.toml cargo run
SQL_BENCH_TEST_DBS=1 cargo test --all-targets   # the tests that need a database
```

Without `SQL_BENCH_TEST_DBS` those tests return early, so `cargo test` on a
machine with no containers is still green.

## Configuration

`~/.config/sql-bench/config.toml`, or whatever `$SQL_BENCH_CONFIG` names.
Copy [config.example.toml](config.example.toml) and edit it; a missing file
is an empty configuration, not an error.

## Command line

A bare `sql-bench` opens the TUI. Every feature is also reachable
headlessly, which is how the project verifies itself:

```sh
sql-bench query   --conn local-mssql 'select top 10 * from bench.customers'
sql-bench query   --conn local-mssql --format json --max-rows 100 -   # SQL on stdin
sql-bench objects --conn local-mssql --schema bench --kind procedure
sql-bench source  --conn local-mssql bench.sp_customer_orders
sql-bench bench   --conn local-mssql --runs 20 'select 1'
```

`query` writes rows to stdout — an aligned table, `--format csv` or
`--format json` — and its `N rows in X ms` footer to stderr, so a pipeline
reads only the rows. It exits 1 on a database complaint, printing the
server's own message and nothing at all on stdout.

`bench` runs the statement `--runs N` times through one ordinary connection
and prints min/p50/p95/max in milliseconds for each phase — connect, first
row, total — and the rows per second. `scripts/perf.sh` runs the canonical
queries on both containers and appends the numbers to `docs/PERF.md`.

`SQL_BENCH_TRACE=<file>` appends one line per connect and one per query,
tab separated; unset, no clock is read. `docs/DESIGN.md` lists the fields,
is the contract everything here is built against, and `docs/backlog.yaml`
is the work breakdown.
