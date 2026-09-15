# sql-bench design

A fast, lightweight terminal workbench for SQL Server and Oracle. This
document is the contract every ticket in [backlog.yaml](backlog.yaml) builds
against. Sections marked *(T7.2)* are completed by that ticket.

## The four rules

1. **The app never blocks on a database.** Every driver call runs on a worker
   thread and reports back over `std::sync::mpsc`. The event loop only polls.
2. **The app is pure.** `src/app/` turns events into state changes and never
   touches IO. `src/ui/` renders state. `src/run/` owns the terminal, threads
   and timers.
3. **Everything is verifiable headlessly.** Subcommands (`query`, `objects`,
   `source`, `bench`) and replay mode (`--replay`) cover every feature.
4. **Measure, don't guess.** `SQL_BENCH_TRACE=<file>` records every phase.

## Dependencies (frozen)

| crate | why |
|---|---|
| anyhow | errors at the edges |
| clap (derive) | CLI |
| crossterm 0.29, ratatui 0.30 | terminal |
| serde (derive), toml 0.9 | config |
| tiberius (no default features; `tds73`, `rustls`) | SQL Server, pure Rust |
| tokio (`rt`, `net`, `time`), tokio-util (`compat`), futures-util | tiberius is async; confined to `db/mssql.rs` |
| oracle 0.6 | Oracle over ODPI-C; needs Instant Client at runtime |
| time 0.3 | timestamps in traces and exports |
| tempfile (dev) | tests |

Why tiberius: the only maintained pure-Rust TDS client; no OpenSSL, no
system driver. Why the `oracle` crate and not ODBC: there is no pure-Rust
Oracle driver, so a native client is unavoidable; ODPI-C is the vendor's own
thin layer, and ODBC would add unixODBC plus two vendor drivers for nothing.

## Data model (`src/db/model.rs`)

```rust
pub enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Decimal(String),   // exact text, scale preserved
    Text(String),
    Bytes(Vec<u8>),
    DateTime(String),  // as the driver formats it; no timezone invented
}

pub struct Column { pub name: String, pub type_name: String }

pub enum QueryEvent {
    Columns(Vec<Column>),          // once per result set
    Rows(Vec<Vec<Cell>>),          // one batch
    RowsAffected(u64),
    Done { rows: usize, truncated: bool, connect_ms: u32, first_row_ms: u32, total_ms: u32 },
    Error(DbError),
}

pub enum DbError { Connect(String), Query { message: String, line: Option<u32> }, Cancelled, Timeout, Unsupported(String) }

pub struct QueryOptions { pub batch_size: usize /* 500 */, pub max_rows: Option<usize> /* Some(10_000) */ }
```

Values with no lossless Rust equivalent are carried as the text the driver
produced. `Decimal` keeps every digit of an Oracle `NUMBER`; `DateTime` is
whatever the driver formatted, and no timezone is invented.

A `CLOB`, `NCLOB` or `BLOB` is read through a locator and stops at **1 MiB**.
A truncated `CLOB` ends in `…` (cut on a character boundary); a truncated
`BLOB` is simply the first mebibyte. A LOB column can hold four gigabytes and
a workbench that copied one into memory because somebody typed `select *`
would be a workbench that fell over. Raise `LOB_LIMIT` in `db/oracle.rs` if a
real value is ever cut, but raise the memory budget with it.

## Connection handle (`src/db/mod.rs`)

`Connection::open` blocks until connected (10 s timeout) and returns a handle
whose worker thread owns the driver connection. `Connection::query` returns
a `Receiver<QueryEvent>` immediately. `Connection::cancel` sets a flag: SQL
Server has no way to say stop, so tiberius drops the socket; Oracle has
`break_execution`, so a watcher thread interrupts the call OCI is inside.
Either way the connection is spent and the next query opens a new one.
`Backend` is an enum with `Mssql` and `Oracle` variants; no trait.

## Catalog (`src/db/catalog.rs`)

Plain SQL per backend through the same query path: schemas, objects by kind,
columns, source. Never touches table data.

## Layout and keys

```
 1 local-mssql ●  2 local-oracle ○                                            ?
╭ Objects ────────────╮╭ Scratch ─────────────────────────────────────────╮
│ ▾ bench             ││ select top 10 * from bench.customers             │
│   ▾ Tables          ││ where country = 'US'                             │
│     customers       ││                                                  │
│   ▸ Views           │╰──────────────────────────────────────────────────╯
│   ▸ Procedures      │╭ Results · 10 rows · 12 ms ───────────────────────╮
│                     ││ id  name        country  created_at              │
╰─────────────────────╯╰──────────────────────────────────────────────────╯
 Tab focus  Ctrl-R run  F5 run all  Ctrl-E editor  ? help          ● connected
```

| key | where | does |
|---|---|---|
| Tab / Shift-Tab | anywhere | cycle focus Objects → Scratch → Results |
| 1-9, Ctrl-T | not Scratch / anywhere | select tab, next tab |
| c / C | not Scratch | connect, disconnect |
| Ctrl-R / F5 | Scratch | run statement under cursor / run all |
| Esc | anywhere | cancel query, close overlay, clear filter |
| Ctrl-E | Scratch | edit in $EDITOR |
| j k h l, Enter, /, r, s, i, y | Objects | move, expand, filter, reload, source, columns, copy name |
| j k h l g G, Enter, y Y, e, m, [ ] | Results | move, inspect, copy, export, more rows, switch set |
| ? | anywhere | help |
| q / Ctrl-Q | not Scratch / anywhere | quit |

## Verifying *(T3.2 fills in the replay format)*

```
SQL_BENCH_CONFIG=config.local.toml sql-bench --replay scripts/replay/run-mssql.keys --size 120x40 --frames-dir /tmp/f
```

## Trace format

`SQL_BENCH_TRACE=<file>` appends one line per event: `unix_ms\tkind\tk=v...`.
Unset, no clock is read at all.

| kind | fields |
|---|---|
| connect | conn, ms |
| query | conn, rows, truncated, connect_ms, first_row_ms, total_ms |
| frame | draw_ms |
| turn | total_ms, draw_ms, input_ms |

## Budgets *(T7.1 records the numbers in PERF.md)*

Startup < 50 ms, key-to-frame p95 < 16 ms, draw cost independent of rows
fetched, `select 1` < 5 ms after connect, 1M-row scan at 100k rows < 8 s.

## Decisions log *(T7.2)*

- Re-run with a higher cap for "more rows" instead of a server cursor.
- Lines in a Vec, not a rope; one-level undo.
- Enum backend, not a trait.
