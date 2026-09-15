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

## Results grid (`src/app/results.rs`, `src/ui/results.rs`)

Ctrl-R runs the statement under the cursor, F5 every statement in the pad.
Rows arrive in batches of 500 over the tab's channel; the run loop drains
every event that has arrived each turn and paints once, so a scan is one
frame per turn and not one per batch. `Results` keeps the rows exactly as
they came and recomputes column widths over the new batch only — capped at
40 characters, never narrower than the header and its type — and the
renderer formats `rows[top .. top + visible]` and the columns that fit
across. A draw therefore costs the window and not the scan, whatever
`--max-rows` allowed.

The fetch cap is `--max-rows` (default 10,000). When it stopped the scan the
title says `(truncated)` and `m` runs the same statement again with another
10,000 allowed, keeping the cell cursor where it was. Esc while a query is
running cancels it: the receiver ends in `Cancelled`, the rows that did
arrive stay on screen and the title reads `cancelled after 1.2 s, 4,500
rows`.

A run of several statements runs them one after another, each started when
the one before it is done. The pane shows the last result set and a summary
line, `3 statements, 2 result sets, 1 rows affected`. **A statement the
server says no to stops the run**: the statements after it were written to
follow it, so they are dropped rather than run against whatever state the
failure left. The footer says which one it was (`statement 2 of 3 failed`),
the pane shows the driver's own message with the line number when it gave
one, and the scratch pad paints that statement's lines in the error
background until the next edit.

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
| Tab / Shift-Tab | not Scratch / anywhere | cycle focus Objects → Scratch → Results |
| 1-9, Ctrl-T | not Scratch / anywhere | select tab, next tab |
| c / C | not Scratch | connect, disconnect |
| Ctrl-R / F5 | Scratch | run statement under cursor / run all |
| Esc | anywhere | cancel query, close overlay, clear filter |
| Ctrl-E | Scratch | edit in $EDITOR |
| Tab, Ctrl-Z, Ctrl-C | Scratch | two spaces, undo the last edits, copy the selection |
| Home End Ctrl-A, Ctrl-U Ctrl-K Ctrl-W, Ctrl-arrows, Shift-arrows | Scratch | move, cut, jump a word, select |
| j k h l, Enter, /, r, s, i, y | Objects | move, expand, filter, reload, source, columns, copy name |
| j k h l, arrows, PageUp/Down, Ctrl-D Ctrl-U, g G, 0 $ | Results | move the cell cursor |
| Enter, y Y, e, m, [ ] | Results | inspect *(T5.3)*, copy, export, 10,000 more rows, switch result set |
| ? | anywhere | help |
| q / Ctrl-Q | not Scratch / anywhere | quit |

`?` opens the help over the layout: `<key>  <what it does>` for the keys of
the focused pane only — `Help · Scratch` — with the key column as wide as the
widest key it lists. It is never taller than the screen: a list that does not
fit scrolls with j k, the arrows and PageUp/PageDown, which the pane under it
does not see while it is open, and the title says which rows are showing
(`Help · Scratch (1-11 of 20)`). `?` and Esc close it and put it back to the
top.

The scratch pad is one pad per connection, kept in
`~/.local/state/sql-bench/scratch/<connection>.sql` — `$SQL_BENCH_STATE_DIR`
moves the directory — written 500 ms after the last edit and on the way out,
and loaded before the first frame. The same pause ends an undo burst, so
Ctrl-Z takes back everything typed without a pause in it and no more.

## Verifying

```
SQL_BENCH_CONFIG=config.local.toml sql-bench --replay scripts/replay/smoke.keys --size 120x40 --frames-dir /tmp/f
```

`--replay FILE` runs the real loop — the real app, the real rendering, the
real key handling — against a `ratatui::backend::TestBackend` of `--size`
(default 120x40), with the keys in FILE instead of a keyboard. No raw mode,
no alternate screen, nothing written to the terminal; `NO_COLOR` is honoured
exactly as it is in a real run. `frame` writes into `--frames-dir` (default
`./frames`, created if missing).

### Commands

One per line. Blank lines and lines whose first non-space character is `#`
are ignored. Every argument is trimmed except `type`'s and `paste`'s, which
are the rest of the line exactly as written.

| command | does |
|---|---|
| `key <name>` | one key press, as crossterm would deliver it |
| `type <text>` | one key press per character, spaces included |
| `paste <text>` | one `Event::Paste` with the whole text |
| `resize <cols>x<rows>` | resize the backend and send `Event::Resize` |
| `wait busy` | until nothing is connecting or running; 60 s, then exit 3 |
| `wait <ms>` | sleep that many milliseconds |
| `wait text <substring>` | until the substring is on the frame; 30 s, then exit 3 |
| `frame <name>` | write `<frames-dir>/<name>.txt` now |
| `expect <substring>` | the substring is on the frame, or exit 4 |
| `expect-not <substring>` | the substring is not on the frame, or exit 4 |

### Key names

`Enter`, `Esc`, `Tab`, `BackTab` (also spelled `Shift-Tab`), `Up`, `Down`,
`Left`, `Right`, `PageUp`, `PageDown`, `Home`, `End`, `Backspace`, `Delete`,
`Insert`, `Space`, `F1`-`F12`, and any single character (`q`, `?`, `1`).
`Ctrl-<key>` and `Alt-<key>` add that modifier to any of them, so `Ctrl-Q`,
`Alt-x` and `Ctrl-F5` are all keys. Anything else is an error naming the
line.

### Frames

```
# help 120x40
 1 local-mssql ●  2 local-oracle ○
╭ Objects ────────────╮╭ Scratch ──────────────────────╮
```

A header of `# <name> <cols>x<rows>`, then one line per row with the padding
trimmed off the right and every box-drawing character kept. With
`--frame-styles` a second file, `<name>.styles.txt`, lists the runs of cells
that share a style — `<row> <from>..<to> fg=<colour> bg=<colour>
mod=<modifiers>` — so a colour can be asserted without a screenshot.

### Exit codes

| code | means |
|---|---|
| 0 | the script ran to its end |
| 1 | something else went wrong (no such file, a bad line, unwritable frames) |
| 3 | a `wait` gave up; the frame is written as `<frames-dir>/timeout.txt` |
| 4 | an `expect` or `expect-not` was wrong; the frame goes to stderr |

Every failure names the line of the script it happened on.

### An example

```
# scripts/replay/smoke.keys
expect 1 local-mssql ○  2 local-oracle ○
frame shell
key ?
expect Ctrl-T     next tab
frame help
key Esc
expect-not ╭ Help
key q
```

### QA scripts

`scripts/qa.sh` runs every check that needs no database, one line each: the
QA replay scripts in `scripts/replay/qa/` at 60x15, 80x24, 120x40, 200x60 and
40x10, a resize mid-run, the quit keys from every pane, `NO_COLOR` against a
`--frame-styles` dump, the terminal restore below and the draw latency. CI
runs it.

### Terminal restore

Three ways out of a run, and all three give the terminal back. A quit (`q`,
`Ctrl-Q`) returns from the loop and an error returns `Err` up to `main`: both
drop the `Restore` guard in `src/run/mod.rs`. A panic runs the hook
`ratatui::try_init` installed. Guard and hook do the same two things — raw
mode off, then the alternate screen left — and the terminal's own `Drop`
shows the cursor after them. The hook restores *before* it prints, so a panic
message lands on the normal screen and not on the one about to be thrown
away.

A replay never takes the terminal at all, so QA checks this on a real pty:
`scripts/qa/panic-restore.sh` runs a debug build under `script` with
`--panic-after-ms` — a hidden flag that exists only under
`cfg(debug_assertions)`, and that panics where the loop waits for a key, so
no key has to arrive for it to fire — and asserts that the capture has the
shell drawn on it, that the run exits 101, and that `\e[?1049l\e[?25h` comes
after the panic message and is the last thing written.

## Trace format

`SQL_BENCH_TRACE=<file>` appends one line per event: `unix_ms\tkind\tk=v...`.
Unset, no clock is read at all.

| kind | fields |
|---|---|
| connect | conn, ms |
| query | conn, rows, truncated, connect_ms, first_row_ms, total_ms |
| results | rows, batches, first_batch_ms |
| frame | draw_ms |
| turn | total_ms, draw_ms, input_ms |

## Budgets *(T7.1 records the numbers in PERF.md)*

Startup < 50 ms, key-to-frame p95 < 16 ms, draw cost independent of rows
fetched, `select 1` < 5 ms after connect, 1M-row scan at 100k rows < 8 s.

## Decisions log *(T7.2)*

- Re-run with a higher cap for "more rows" instead of a server cursor.
- Lines in a Vec, not a rope; one-level undo.
- Enum backend, not a trait.
