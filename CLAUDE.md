# sql-bench — conventions for every contributor (human or agent)

sql-bench is a fast, lightweight terminal workbench for SQL Server and Oracle:
manage connections, run queries from a scratch pad, browse database objects
and stored procedure source. Rust, ratatui 0.30, crossterm 0.29, edition 2024.
It is the sibling of ticket-tui and az-tui (same stack, same layout, same keys).

## The four rules

1. **The app never blocks on a database.** Every driver call runs on a worker
   thread and reports back over `std::sync::mpsc`. The event loop only polls.
2. **The app is pure.** `src/app/` turns `crossterm` events into state changes
   and never touches IO. `src/ui/` renders state into a `ratatui::Frame`.
   `src/run/` owns the terminal, threads and timers.
3. **Everything is verifiable headlessly.** Every feature is reachable through
   a subcommand (`query`, `objects`, `source`, `bench`) or through replay mode
   (`--replay keys.txt`), which drives the real loop against a `TestBackend`
   and writes frames as plain text. Screenshots are never the verification.
4. **Measure, don't guess.** `SQL_BENCH_TRACE=<file>` appends one line per
   frame and per query phase. Budgets: startup < 50 ms, key-to-frame < 16 ms,
   draw cost independent of rows fetched.

## Layout

```
src/main.rs        thin: parse CLI, run subcommand or TUI
src/lib.rs         module list
src/cli.rs         clap definitions and headless subcommands
src/config.rs      ~/.config/sql-bench/config.toml: connections, oracle client dir
src/db/            model.rs (Cell, Column, QueryEvent), mod.rs (Connection handle
                   + worker thread), mssql.rs, oracle.rs, catalog.rs (object SQL)
src/app/           pure state: shell (tabs, focus, status), scratch, results, objects
src/ui/            rendering + theme; tests in src/ui/tests/
src/run/           terminal loop, replay mode, trace
tests/             integration tests, skipped unless SQL_BENCH_TEST_DBS=1
scripts/           db-up.sh, db-down.sh, db-reset.sh, seed SQL, perf.sh
docs/              DESIGN.md, backlog.yaml (the work breakdown), PERF.md
```

## Local databases

`scripts/db-up.sh` starts both containers via `compose.yaml` and seeds them.

| | host | port | database/service | user | password |
|---|---|---|---|---|---|
| SQL Server 2022 Developer | localhost | 1433 | bench | sa | Bench_Pass1! |
| Oracle 23ai Free | localhost | 1521 | FREEPDB1 | bench | bench |

`config.local.toml` at the repo root names both as `local-mssql` and
`local-oracle`. Oracle Instant Client lives at
`~/.local/opt/oracle/instantclient_23_26` (env `SQL_BENCH_ORACLE_CLIENT_DIR`
or `[oracle] client_lib_dir` in config override it). `scripts/oracle-client.sh` installs it and patches its run path (patchelf via
`uvx`), so nothing else is needed on this machine.

## Checks before any commit

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
SQL_BENCH_TEST_DBS=1 cargo test --all-targets   # when the containers are up
```

## Style

- Dependencies are frozen to the list in `docs/DESIGN.md`. Do not add one
  without the ticket saying so.
- No trait with one implementation. `db::Backend` is an enum.
- Tests render through `ratatui::backend::TestBackend` and assert on buffer
  text. Prefer asserting a whole line over a substring.
- Errors: `anyhow` at the edges, a small `DbError` enum inside `db/`.
- Doc comments explain why, not what. No comment restates the code.
- Commit messages: imperative, one line, optionally a body. Reference the
  ticket as `[T2.1]`.
- Mark deliberate shortcuts with a `// ponytail:` comment naming the ceiling.
