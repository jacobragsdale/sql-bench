# sql-bench design

How sql-bench works and why, in full: the rules the modules obey, the worker
protocol every query goes down, what a database type becomes, how a pad is cut
into statements, how the grid stays cheap, the replay grammar, the trace
format and the decisions behind all of it. [README.md](../README.md) is the
short way in, and [backlog.yaml](backlog.yaml) is the work breakdown this was
built from.

- [The four rules](#the-four-rules)
- [Module map](#module-map)
- [Dependencies](#dependencies-frozen)
- [Configuration and startup](#configuration-and-startup)
- [The worker protocol](#the-worker-protocol)
- [Data model](#data-model-srcdbmodelrs)
- [Statement splitting](#statement-splitting-srcappscratchrs)
- [Catalog and the object tree](#catalog-and-the-object-tree)
- [Results grid](#results-grid-srcappresultsrs-srcuiresultsrs)
- [Overlays, copy and export](#overlays-copy-and-export)
- [The scratch pad](#the-scratch-pad)
- [Keys](#keys)
- [Replay](#replay)
- [QA scripts](#qa-scripts)
- [Terminal restore](#terminal-restore)
- [Trace format](#trace-format)
- [Budgets](#budgets)
- [Decisions log](#decisions-log)

## The four rules

1. **The app never blocks on a database.** Every driver call runs on a worker
   thread and reports back over `std::sync::mpsc`. The event loop only polls.
2. **The app is pure.** `src/app/` turns events into state changes and never
   touches IO. `src/ui/` renders state. `src/run/` owns the terminal, threads
   and timers.
3. **Everything is verifiable headlessly.** Subcommands (`query`, `objects`,
   `source`, `bench`) and replay mode (`--replay`) cover every feature.
   Screenshots are never the verification — the frame in the README is
   written by a replay run.
4. **Measure, don't guess.** `SQL_BENCH_TRACE=<file>` records every phase.

## Module map

```
src/main.rs        thin: parse CLI, run a subcommand, a replay or the TUI
src/lib.rs         the module list
src/cli.rs         clap definitions and the headless subcommands
src/config.rs      config.toml: connections, the Oracle client directory
src/db/            model.rs (Cell, Column, QueryEvent), mod.rs (the handle and
                   its worker), mssql.rs, oracle.rs, catalog.rs (object SQL)
src/app/           pure state: mod.rs (tabs, focus, keys), scratch.rs,
                   results.rs, objects.rs, prompt.rs
src/ui/            rendering and theme; tests in src/ui/tests/
src/run/           mod.rs (the loop and the terminal), runtime.rs (threads and
                   channels), replay.rs, state.rs (the pads), editor.rs
src/export.rs      table, CSV and JSON, shared by the TUI and the subcommands
src/trace.rs       SQL_BENCH_TRACE
tests/             integration tests, skipped unless SQL_BENCH_TEST_DBS=1
scripts/           containers, seed SQL, QA, perf, the README frame
docs/              DESIGN.md, TYPES.md, PERF.md, backlog.yaml
```

The app is a plain state machine: `App::handle(Event)` returns a list of
`Action`s, and the run loop is the only thing that carries one out. That is
what makes the whole of `src/app/` testable without a terminal or a server,
and what keeps replay honest — a replay drives the same `handle` the keyboard
does.

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
| unicode-width 0.2 | how many terminal columns a glyph is drawn in; the grid and `export::table` align on it |
| tempfile (dev) | tests |

Adding one is a ticket's decision, never a convenience.

## Configuration and startup

`--config PATH`, else `$SQL_BENCH_CONFIG`, else
`~/.config/sql-bench/config.toml`. A missing file is an empty configuration
and not an error. [README.md](../README.md#configuration) explains every key;
[config.example.toml](../config.example.toml) is a commented copy of the whole
of it.

Everything in the file is validated when it is read, except the passwords.
Those are resolved when a connection is opened — by the caller's thread, not
the worker's, so a `password_cmd` that fails does so against something still
allowed to print — which is why a file holding ten `password_cmd` lines does
not run ten commands to answer `--help`.

One tab per connection, in file order, none of them connected. `--connect
NAME` (repeatable) and `--connect-all` connect after the first frame is on
screen and never before: a connect takes up to ten seconds and nobody should
watch a blank terminal for it.

`Ctrl-E` writes the pad to a temp file, runs `$VISUAL` or `$EDITOR` (split on
whitespace, so `code -w` works), reads the file back and resumes the TUI.
Neither variable set is a footer message. In a replay it does nothing and says
`editor unavailable in replay`, because a replay has no terminal to hand over.

## The worker protocol

`Connection::open` blocks until connected or until the driver's ten second
timeout runs out, and returns a handle whose worker thread owns the one driver
connection. The handle and the worker speak over two channels:

```rust
enum Request {
    Query { sql: String, options: QueryOptions, reply: Sender<QueryEvent> },
    Close,
}
```

`Connection::query` sends a `Request::Query` and hands back the matching
`Receiver<QueryEvent>` at once; the worker fills it with `Columns`, `Rows`,
`RowsAffected` and finally one `Done` or one `Error`. Dropping the receiver
stops the query at the next batch. Dropping the handle cancels whatever is
running, sends `Close` and joins the thread.

`Backend` is an enum with `Mssql` and `Oracle` variants. tiberius is async, so
the SQL Server worker builds a current-thread tokio runtime and blocks on it;
nothing async leaves `db/mssql.rs`. The `oracle` crate is synchronous, so that
worker is a plain loop.

### Cancel

Cancel is not a `Request`: the worker is inside the query the cancel is meant
to stop and would not read the channel until it finished. `Connection::cancel`
sets an `AtomicBool` the backend polls every `CANCEL_POLL_MS` (50 ms) while it
waits on the server, which is cheap next to a network round trip and is the
floor on how fast a cancel can land.

How each backend answers differs. TDS gives tiberius nothing to say *stop*
with, so the SQL Server worker races the query against a watcher and, when the
flag is set, drops the client — the socket goes and the server notices. Oracle
has `break_execution`, so a watcher thread interrupts the call OCI is inside.
Either way the receiver ends in `DbError::Cancelled`, the connection is spent,
and the next query opens a new one.

The Oracle ceiling is worth knowing: a session asleep inside PL/SQL —
`dbms_session.sleep` — finishes its sleep before the break lands, so the
cancel is reported late rather than lost. Everything else answers in well
under a second.

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
whatever the driver formatted, and no timezone is invented. `Cell::display` is
the one text form, so a value reads the same in the grid as in an export.

Which database type becomes which `Cell`, for both servers, with a real
example of each, is [TYPES.md](TYPES.md) — the table there is printed output
and is asserted against by `tests/mssql.rs` and `tests/oracle.rs`.

A `CLOB`, `NCLOB` or `BLOB` is read through a locator and stops at **1 MiB**
(`LOB_LIMIT` in `db/oracle.rs`). A truncated `CLOB` ends in `…`, cut on a
character boundary; a truncated `BLOB` is simply the first mebibyte. A LOB
column can hold four gigabytes, and a workbench that copied one into memory
because somebody typed `select *` would be a workbench that fell over. SQL
Server has no locator in this driver: `nvarchar(max)` and `varbinary(max)`
come down the wire whole. Raising either ceiling means raising a memory
budget, not just a constant.

## Statement splitting (`src/app/scratch.rs`)

`Ctrl-R` runs the statement the cursor is in; `F5` runs every statement in the
pad. Both go through `Scratch::statements`, which walks the lines once:

- A line ending in `;` ends the statement, and the `;` goes with it.
- A line that is exactly `GO` ends it on SQL Server, a line that is exactly
  `/` ends it on Oracle. Case is ignored; the terminator line is not part of
  the statement.
- A blank line ends it.
- The end of the pad ends it.
- `declare` or `begin` at the start of a line opens a block, and only the
  `end` that closes it ends the statement — so the semicolons inside a PL/SQL
  body do not split it, and neither does a blank line inside it. Words are
  matched whatever their case.
- A statement is trimmed, and an empty one is never emitted.

Each statement carries the range of lines it came from, which is what lets a
failure paint those lines in the pad and what `statement_at_cursor` searches.
A cursor on a blank line or on a terminator gets the statement *before* it,
because that is where it was just typed.

## Catalog and the object tree

`src/db/catalog.rs` is plain SQL per backend down the same query path:
schemas, objects by kind, a table's columns, an object's source. It never
touches table data.

Each question is a `CatalogRequest` — `Schemas`, `Objects { schema, kind }`,
`Columns { schema, table, .. }`, `Source { schema, name, kind }` — that knows
the one statement that answers it (`sql(backend)`) and how to read the rows
back (`answer(backend, rows)`). That split is what lets the same question be
asked two ways: the `objects` and `source` subcommands run it blocking, and
the object tree sends it down the tab's own query channel and is polled once a
turn, so the loop waits for neither. A tab already loading something queues the
next request rather than opening a second connection.

The tree (`src/app/objects.rs`, `src/ui/objects.rs`) is a flat `Vec<Node>`
with a depth per row: schemas, the kinds under them, the objects under those,
a table's columns under that. Nothing is loaded until it is opened — the row
says `…` while the catalog query runs and `✗ ORA-…` if it would not — and
what has loaded stays loaded for the session, `r` being the way to ask again.
The connection's own schema is listed first and opened as soon as the
connection is up: `dbo` on SQL Server, the user name on Oracle, where every
catalog name is upper case.

`/` narrows the pane to the rows whose name contains what is typed, plus the
branches above them, and says so in the title: `Objects /cust`. Esc clears it.
Enter on a table or a view writes `select top 100 * from schema.name` —
`select * from schema.name fetch first 100 rows only` on Oracle — into the pad
on a line of its own and moves the focus there. `i` puts a table's columns in
the results pane as a grid, `s` puts an object's source there as numbered
lines that the pane's own movement keys scroll, and `y` copies the qualified
name.

## Results grid (`src/app/results.rs`, `src/ui/results.rs`)

Rows arrive in batches of 500 over the tab's channel; the run loop drains
every event that has arrived each turn and paints once, so a scan is one frame
per turn and not one per batch. `Results` keeps the rows exactly as they came
and recomputes column widths over the new batch only — capped at 40
characters, never narrower than the header and its type — and the renderer
formats `rows[top .. top + visible]` and the columns that fit across. A draw
therefore costs the window and not the scan, whatever `--max-rows` allowed.

Widths, cuts and alignment are all counted in terminal columns rather than
characters, through `unicode-width`, because a CJK glyph is drawn two cells
wide and a grid that counted `char`s would lean.

The fetch cap is `--max-rows` (default 10,000). When it stopped the scan the
title says `(truncated)` — `Results · 10,000 rows (truncated) · 1,234 ms` —
and `m` runs the same statement again with another 10,000 allowed, keeping the
cell cursor where it was. Esc while a query is running cancels it: the
receiver ends in `Cancelled`, the rows that did arrive stay on screen and the
title reads `cancelled after 1.2 s, 4,500 rows`.

A run of several statements runs them one after another, each started when the
one before it is done. The pane shows the last result set and a summary line,
`3 statements, 2 result sets, 1 rows affected`; `[` and `]` switch between the
sets of one statement. **A statement the server says no to stops the run**:
the statements after it were written to follow it, so they are dropped rather
than run against whatever state the failure left. The footer says which one it
was (`statement 2 of 3 failed`), the pane shows the driver's own message with
the line number when it gave one, and the scratch pad paints that statement's
lines in the error background until the next edit.

## Overlays, copy and export

Enter opens the cell inspector over the layout: a 72-column overlay, inside
the tab bar and the footer the way the help is, titled `body · nvarchar ·
102,400 chars` — the column, its type, and the value in characters, or in
bytes for a `Bytes`. Text is wrapped at the overlay's width with its own line
breaks kept, a `Bytes` is a hex dump of sixteen bytes a line with the
printable ones beside it, and a NULL is the word. j k, the arrows and
PageUp/PageDown scroll it, which the grid under it does not see; Esc closes
it, after the help and before a running query.

`y` copies the cell and `Y` the row, tab-separated with a NULL as nothing,
into the app's clipboard and out through OSC 52; the footer says `copied 1
cell`. `e` opens a one-line prompt in the footer, `Export to: ` prefilled with
`~/sql-bench-<connection>-<YYYYmmdd-HHMMSS>.csv`, which takes every key while
it is open — insert, Backspace, Left, Right, Home, End and Ctrl-U, and Esc to
give up. Enter writes every fetched row of the set on screen: JSON for a
`.json` name and CSV for anything else, through the same `src/export.rs` the
headless subcommands use, so a file is the same bytes whichever door it left
by. The footer says `exported 1,234 rows to <path>`, or what stopped it.

`?` opens the help over the layout: `<key>  <what it does>` for the keys of
the focused pane only — `Help · Scratch` — with the key column as wide as the
widest key it lists. It is never taller than the screen: a list that does not
fit scrolls with j k, the arrows and PageUp/PageDown, which the pane under it
does not see while it is open, and the title says which rows are showing
(`Help · Scratch (1-11 of 20)`). `?` and Esc close it and put it back to the
top.

## The scratch pad

One pad per connection, kept in `<state dir>/scratch/<connection>.sql`. The
state directory is `$SQL_BENCH_STATE_DIR`, else `$XDG_STATE_HOME/sql-bench`,
else `~/.local/state/sql-bench`. A pad is written 500 ms after the last edit
and on the way out, and is loaded before the first frame. The same pause ends
an undo burst, so Ctrl-Z takes back everything typed without a pause in it and
no more.

## Keys

`src/app/mod.rs` holds one table, `KEYS`: the key, where it works
(`anywhere`, `not Scratch`, `Scratch`, `Results`, `Objects`) and what it does.
The help overlay, the footer hints, the key tests and the README's key tables
all read that table and nothing else, so a key that is not in it is a key
nobody is told about, and a key in it that `App::handle` ignores fails a test.
`the_readme_lists_every_key_and_no_others` in `src/app/tests.rs` is what keeps
the README honest.

`not Scratch` is the interesting column. The pad has to be able to type `c`,
`q` and `1`, so those keys only act as commands where nothing is being typed,
and every pane keeps a way out that the pad does not swallow: `Shift-Tab`,
`Ctrl-T`, `Ctrl-Q` and `?` work everywhere. `Tab` is in the table twice for
the same reason — next pane outside the pad, two spaces inside it.

[README.md](../README.md#keys) lists them all, grouped by pane, and has a
frame of the layout written by a real run.

## Replay

```
SQL_BENCH_CONFIG=config.local.toml sql-bench --replay scripts/replay/smoke.keys --size 120x40 --frames-dir /tmp/f
```

`--replay FILE` runs the real loop — the real app, the real rendering, the
real key handling — against a `ratatui::backend::TestBackend` of `--size`
(default 120x40), with the keys in FILE instead of a keyboard. No raw mode, no
alternate screen, nothing written to the terminal; `NO_COLOR` is honoured
exactly as it is in a real run. `frame` writes into `--frames-dir` (default
`./frames`, created if missing).

The runner takes the loop a turn at a time rather than feeding a cleverer
input source into it, because `expect`, `wait` and `frame` all have to look at
the screen *between* keys, and something called from inside the loop can see
neither.

### Commands

One per line. Blank lines and lines whose first non-space character is `#` are
ignored. Every argument is trimmed except `type`'s and `paste`'s, which are
the rest of the line exactly as written.

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
`Ctrl-`, `Alt-` and `Shift-` add that modifier to any of them, so `Ctrl-Q`,
`Alt-x`, `Shift-Left` and `Ctrl-F5` are all keys. Anything else is an error
naming the line.

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

A frame is one character per terminal cell, so a glyph drawn two cells wide
comes out as itself followed by a space. That is fine for an `expect` and
wrong for a picture, which is why `scripts/replay/readme.keys` picks rows
whose names are narrow.

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
expect ╭ Help · Objects ─
expect Ctrl-T     next tab
frame help
key Esc
expect-not ╭ Help

key Tab
expect ╭ Scratch
key Shift-Tab

key q
```

## QA scripts

`scripts/qa.sh` runs every check that needs no database, one line each: the QA
replay scripts in `scripts/replay/qa/` at 60x15, 80x24, 120x40, 200x60 and
40x10, a resize mid-run, the footer hints following the focus, the quit keys
from every pane, `NO_COLOR` against a `--frame-styles` dump, the scratch pad's
frame and its persistence, the two terminal-restore checks below, the stdin-EOF check and the draw latency. CI
runs it.

Everything that needs the two containers is behind `SQL_BENCH_TEST_DBS=1` in
the same script: the password-leak check, the `--max-rows` timing, the
connection lifecycle, the query and object replays, the query workflow, a
database outage, and `scripts/readme-frame.sh --check`. CI has no databases,
so the README's frame is regenerated locally — run `scripts/readme-frame.sh`
after anything that changes the layout, and commit what it writes. The
comparison ignores the milliseconds in the footer and the results title, which
are a clock and not a layout.

## Terminal restore

Four ways out of a run, and all four give the terminal back. A quit (`q`,
`Ctrl-Q`), an input that ran out, and an error returning `Err` up to `main`
all drop the `Restore` guard in `src/run/mod.rs`. A panic runs the hook
`ratatui::try_init` installed. Guard and hook do the same two things — raw
mode off, then the alternate screen left — and the terminal's own `Drop` shows
the cursor after them. The hook restores *before* it prints, so a panic
message lands on the normal screen and not on the one about to be thrown away.

A replay never takes the terminal at all, so QA checks this on a real pty:
`scripts/qa/panic-restore.sh` runs a debug build under `script` with
`--panic-after-ms` — a hidden flag that exists only under
`cfg(debug_assertions)`, and that panics where the loop waits for a key, so no
key has to arrive for it to fire — and asserts that the capture has the shell
drawn on it, that the run exits 101, and that `\e[?1049l\e[?25h` comes after
the panic message and is the last thing written.

Input that ran out is the fourth way: keys are read on a thread
of their own into an `mpsc` channel, so the loop waits on the channel and
never inside crossterm. A read that fails — the pty's other end closed, the
window shut — drops the sender, and `TerminalInput` then says it is no longer
live, which ends the loop rather than keeping it alive for a spinner nobody
can see. A run whose own standard input is not a terminal gets no reader at
all: crossterm would quietly read `/dev/tty` instead, which is how `sql-bench
</dev/null` used to hold raw mode and the alternate screen with no key left
that could quit it. `scripts/qa/stdin-eof.sh` runs that under `script` and
asserts the shell was drawn, the run exits 0 inside two seconds, and
`\e[?1049l\e[?25h` is the last thing written.

## Trace format

`SQL_BENCH_TRACE=<file>` appends one line per event: `unix_ms\tkind\tk=v...`.
Unset, no clock is read at all, so a build that is not being measured pays
nothing for the instrument. A write that fails is dropped: a trace file is
never worth taking the app down for. `start` is written before the command
line has been parsed, so that startup is the first `frame` minus it and not a
shell's idea of when the process began.

| kind | fields | written when |
|---|---|---|
| start | — | first thing in `main` |
| connect | conn, ms | a worker has opened, or failed to open, its connection |
| query | conn, rows, truncated, connect_ms, first_row_ms, total_ms | a worker has finished a statement |
| results | rows, batches, first_batch_ms | the app has taken the last event of a run |
| frame | draw_ms | every redraw |
| turn | total_ms, draw_ms, input_ms | a turn that drew and handled input slower than the budget |

## Budgets

Startup < 50 ms, key-to-frame p95 < 16 ms (5 ms for a release build's draw),
draw cost independent of rows fetched, `select 1` < 5 ms after connect, a
100,000-row scan < 8 s. [PERF.md](PERF.md) is what was measured against them,
on which machine, and by which script.

`scripts/perf.sh` measures every one of them and appends the table to
[PERF.md](PERF.md) with the commit it measured, and fails if one was missed.
`cargo test --release -- --ignored` asserts the same budgets with a 2x
margin against rows it makes up, so a regression is a red test on a machine
with no database on it.

## Decisions log

**tiberius, not ODBC, for SQL Server.** It is the only maintained pure-Rust
TDS client. No unixODBC, no vendor driver, no OpenSSL: `cargo install` is the
whole install on every platform. The cost is that it is async, which is why
one tokio current-thread runtime lives inside `db/mssql.rs` and nothing async
leaves it.

**The `oracle` crate, not ODBC, for Oracle.** There is no pure-Rust Oracle
driver, so a native client is unavoidable. ODPI-C is the vendor's own thin
layer over OCI and the `oracle` crate binds it statically; ODBC would add
unixODBC *and* a vendor driver on top of the same client library, for nothing.
The Instant Client is loaded at run time, so a machine that never opens an
Oracle connection never needs it.

**Re-run with a higher cap for "more rows", not a server cursor.** Holding a
cursor open means holding a transaction open on somebody's production server
for as long as a person leaves a pane on screen. `m` re-runs the statement
with the cap raised by 10,000 and puts the cell cursor back. It costs the
rows already read a second time and it is wrong for a query that is not
stable, which is the trade a workbench should make: nothing is pinned server
side, and a connection is never held hostage by an idle window.

**A `Vec<String>` of lines, not a rope.** A scratch pad is a screenful of SQL,
not a document. Indexing lines is what the renderer, the splitter and the
persistence all want, and a rope would cost more to read than it ever saved.
Undo is one snapshot per edit burst for the same reason; an undo stack is the
upgrade if anyone asks for a second Ctrl-Z.

**`Backend` is an enum, not a trait.** Two implementations, both in this
crate, neither of them swappable by a user. An enum keeps every match
exhaustive, so a third backend would be a compile error in every place that
has to care, and there is no object safety to design around.

**Replay on a `TestBackend`, not screenshots.** A screenshot proves a shape
and nothing else: it cannot be diffed usefully, it cannot be asserted on, and
it rots without saying so. A replay runs the real loop, the real app and the
real rendering and writes text, so a frame can be committed, diffed, grepped
and asserted against — and the README's frame is regenerated by the same
mechanism rather than redrawn by hand.

**`?` toggles help everywhere, including the pad.** A key that is only
sometimes help is a key nobody trusts. `?` is not a character anybody needs
often in SQL, and Ctrl-E hands the pad to a real editor for anything heavier,
so the pad gives up `?` and keeps everything else.

**Tab types two spaces in the pad.** A tab character in SQL is a merge
conflict waiting to happen and renders differently everywhere. Two spaces are
what `scripts/seed` uses. The cost is that the pad cannot use Tab to change
panes, which is why `Shift-Tab` works everywhere.

**Display width through `unicode-width`, not `char` counts.** A CJK glyph is
drawn two cells wide, so a grid that counted characters would lean the moment
a name was not ASCII. Every width, cut and pad in the grid and in
`export::table` is counted in terminal columns.

**A 1 MiB LOB ceiling on Oracle, none on SQL Server.** ODPI-C gives a locator,
so Oracle can read the first mebibyte of a `CLOB` or `BLOB` and stop; a cut
`CLOB` ends in `…`. tiberius has no locator, so `nvarchar(max)` and
`varbinary(max)` come down the wire whole and a row holding a gigabyte would
be a gigabyte in this process. Both are honest about what they are: the Oracle
side is a constant to raise alongside a memory budget, and the SQL Server side
would be a driver change, not a constant.
