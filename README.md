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

A zip Instant Client ships without a run path, so the operating system also
has to be able to find `libclntsh.so`'s own libraries — `client_lib_dir` says
where the client is, not where its dependencies are. Once per machine:

```sh
echo "$HOME/.local/opt/oracle/instantclient_23_26" | sudo tee /etc/ld.so.conf.d/oracle.conf
sudo ldconfig
```

or export `LD_LIBRARY_PATH` before running (`cargo test` included). Without
it every Oracle connection fails with *Oracle client library not found*.

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
sql-bench objects --conn local-mssql customers
sql-bench source  --conn local-mssql bench.usp_report
sql-bench bench   --conn local-mssql 'select 1'
```

The TUI and the subcommands are stubs until their tickets land; each says
so and exits 2. `docs/DESIGN.md` is the contract they are built against and
`docs/backlog.yaml` is the work breakdown.
