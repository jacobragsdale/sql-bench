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
`[oracle] client_lib_dir` in the config at it. Nothing is needed at build
time.

## Local databases

```sh
scripts/db-up.sh     # SQL Server 2022 and Oracle 23ai Free, seeded
scripts/db-down.sh
```

`config.local.toml` in this repo names both as `local-mssql` and
`local-oracle`:

```sh
SQL_BENCH_CONFIG=config.local.toml cargo run
```

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

The subcommands are stubs until their tickets land; each says so and exits
2. `docs/DESIGN.md` is the contract they are built against and
`docs/backlog.yaml` is the work breakdown.
