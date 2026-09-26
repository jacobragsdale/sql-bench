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
src/app/           pure state: mod.rs (tabs, focus, keys), pointer.rs (the
                   mouse), scratch.rs, results.rs, objects.rs, prompt.rs
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
| tokio (`rt`, `net`, `time`, `signal`), tokio-util (`compat`), futures-util | tiberius is async; confined to `db/mssql.rs`, plus the thread that catches SIGTERM/SIGHUP |
| oracle 0.6 | Oracle over ODPI-C; needs Instant Client at runtime |
| time 0.3 | timestamps in traces and exports |
| unicode-width 0.2 | how many terminal columns a glyph is drawn in; the grid and `export::table` align on it |
| tempfile (dev) | tests |

Adding one is a ticket's decision, never a convenience.

## Configuration and startup

`--config PATH`, else `$SQL_BENCH_CONFIG`, else
`~/.config/sql-bench/config.toml`. A missing default file is an empty
configuration and not an error; a missing file that `--config` or
`$SQL_BENCH_CONFIG` named is one, and so is a key the file does not know.
[README.md](../README.md#configuration) explains every key;
[config.example.toml](../config.example.toml) is a commented copy of the whole
of it.

Everything in the file is validated when it is read, except the passwords.
Those are resolved when a connection is opened — by the caller's thread, not
the worker's, so a `password_cmd` that fails does so against something still
allowed to print — which is why a file holding ten `password_cmd` lines does
not run ten commands to answer `--help`.

One tab per connection, in file order. The first one connects at launch;
`--connect NAME` (repeatable) and `--connect-all` name others instead. Every
one of them connects after the first frame is on screen and never before: a
connect takes up to ten seconds and nobody should watch a blank terminal for
it. A replay connects only what those flags name, so its frames never depend
on a server being up. Opening a disconnected tab (`Ctrl-T`, a digit, a click)
connects it; a failed one waits for `c`, so its error stays up. More tabs
than the bar is wide start the bar late enough to show the active one, with
`…` for those left off. Quitting
closes every connection before the terminal is given back: the driver owns
them, and dropping one cancels its query and joins its worker.

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

pub enum DbError { Connect(String), Lost(String), Config(String), Query { message: String, line: Option<u32> }, Cancelled, Timeout, Unsupported(String) }

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
because somebody typed `select *` would be a workbench that fell over. The
session prefetches each LOB's size and first 8 K (`LOB_PREFETCH`) with its
row, and the read stops at that size, so a small LOB costs no round trip of
its own: 10,000 short CLOBs fetch in about 40 ms rather than 820. SQL
Server has no locator in this driver: `nvarchar(max)` and `varbinary(max)`
come down the wire whole. Raising either ceiling means raising a memory
budget, not just a constant.

## Statement splitting (`src/app/scratch.rs`)

`Ctrl-R` runs the statement the cursor is in; `F5` runs every statement in the
pad. Both go through `Scratch::statements`, which walks the lines once:

- A line ending in `;` ends the statement, and the `;` goes with it; a
  trailing `--` comment after the `;` does not hide it.
- A line that is exactly `GO` ends it on SQL Server, a line that is exactly
  `/` ends it on Oracle. Case is ignored; the terminator line is not part of
  the statement.
- A blank line ends it.
- The end of the pad ends it.
- `begin` at the start of a line opens a block (`begin tran` does not), and
  only the `end` that closes it ends the statement — so the semicolons inside
  a PL/SQL body do not split it, and neither does a blank line inside it. On
  Oracle `declare`, and a `create` of a procedure, function, trigger, package
  or type body, open one too; a package or type body counts as its own
  `begin`, since its `end` has none. Words are matched whatever their case.
- On SQL Server a `declare`, or a `create` of a procedure, function or
  trigger, ignores its semicolons and runs on to the next blank line or `GO`:
  a variable lives as long as its batch, and so does a procedure's body.
- Oracle runs one statement per call, so outside a block a statement is also
  cut at every `;` that is not in a quote or a comment — `select 1 from dual;
  select 2 from dual` on one line is two. Both keep that line's range, so
  `Ctrl-R` there runs the first.
- A statement is trimmed, and an empty one is never emitted — nor is one
  that is only comments and semicolons: a note on lines of its own is not a
  statement, and Oracle would stop a run with ORA-00900 over it. A note in
  front of a statement is read past, so `/* why */ begin` opens a block, and
  the Oracle driver keeps the `end;` of a block or a stored program whatever
  comment comes before it.

Each statement carries the range of lines it came from, which is what lets a
failure paint those lines in the pad and what `statement_at_cursor` searches.
A cursor on a blank line or on a terminator gets the statement *before* it,
because that is where it was just typed.

## Catalog and the object tree

`src/db/catalog.rs` is plain SQL per backend down the same query path:
schemas, objects by kind, a table's columns, an object's source. It never
touches table data.

Each question is a `CatalogRequest` — `Schemas`, `Index`, `Objects { schema,
kind }`, `Columns { schema, table, .. }`, `Source { schema, name, kind }` — that knows
the one statement that answers it (`sql(backend)`) and how to read the rows
back (`answer(backend, rows)`). That split is what lets the same question be
asked two ways: the `objects` and `source` subcommands run it blocking, and
the object tree sends it down the tab's own query channel and is polled once a
turn, so the loop waits for neither. A tab already loading something queues the
next request rather than opening a second connection, and a query and a load
take turns rather than both going to the worker: a cancel is one flag per
connection, so Esc on a query with the index queued behind it would have
cancelled the index too.

The tree (`src/app/objects.rs`, `src/ui/objects.rs`) is a flat `Vec<Node>`
with a depth per row: schemas, the kinds under them, the objects under those,
a table's columns under that. A connection that comes up asks two things in
a row: the schema list, and then the *index* — every object of every schema
worth showing, in one `Index` query, kept in the tab's `Objects` for the
session. The pane's title says `Objects · indexing…` until it lands. Once it
has, opening a kind fills its branch from the index and asks the server
nothing; before it has, the branch is asked for on its own the way it always
was, and the index refills it when it arrives. A table's columns and an
object's source are still fetched when they are wanted — the row says `…`
while the catalog query runs and `✗ ORA-…` if it would not — and `r` on a
kind asks for the index again. The connection's own schema is listed first
and opened as soon as the connection is up: `dbo` on SQL Server, the user
name on Oracle, where every catalog name is upper case.

`Ctrl-P` anywhere opens the finder (`src/app/finder.rs`) over the layout: a
query line and, under it, the objects of *every* connected tab whose name
answers it, best first — the name itself, then names starting with it, then
names containing it, then names its letters appear in, in order. A word with
a dot in it is matched against `schema.name`; several words all have to
match. The index keeps a lower-cased copy of every name when it lands, so a
keystroke is one pass over the names and nothing is allocated per object;
the hits are ranked and cut to the best two hundred, which the title says
(`Find · 2311 of 48210, first 200`). Enter switches to the object's tab,
opens the tree down to it and puts what it is made of in the results pane:
the source of a procedure, a function, a package or a view, the columns of
a table. The source is fetched then, not kept — it is one catalog query on
the tab's own connection, the same one `s` runs — and the focus lands on the
results pane so the source scrolls at once. Esc closes it; Ctrl-Q still
quits.

`/` searches the whole connection, not just what is open: it fills every
branch nobody opened from the index (the title says `…` while the index is
still on its way, and a tab whose index failed asks for it again). The pane
narrows to the schemas, objects and open columns whose name contains what is
typed — `schema.name` when it has a dot in it — plus the branches above them,
and says so in the title: `Objects /cust`. The cursor jumps to the first match
and Up/Down step between matches while typing; Enter keeps the filter and
gives the keys back. Esc clears it and opens the branches above the cursor,
so the row the filter found stays under it.
Enter on a table or a view writes `select top 100 * from schema.name` —
`select * from schema.name fetch first 100 rows only` on Oracle — into the pad
on a line of its own, after the line the cursor is in, and moves the focus
there. A name is quoted where it has to be — `dbo.[order details]`,
`[user]`, `BENCH."MixedCase"` — and left alone where it reads back the same;
`y` copies it the same way. `i` puts a table's columns in
the results pane as a grid, `s` puts an object's source there as numbered
lines that the pane's own movement keys scroll — for a table, a `CREATE
TABLE` written from its columns and primary key — and `y` copies the
qualified name. Over a source, `y` (or its title's `Copy`) copies all of it.

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
title of that set says `(truncated)` — `Results · 10,000 rows (truncated) ·
1,234 ms`, and on an earlier set of an Oracle run, which goes on past one —
and `m` runs the last statement again with another 10,000 allowed, keeping the
cell cursor where it was; the next statement run is capped at `--max-rows`
again. On SQL Server a cap that stops a scan ends the session, because the
driver has no attention packet and dropping the socket is the only way to
stop the server: the statement and any open transaction roll back and
`#temp` tables go, so a run stops there rather than go on in a session its
statements were not written for, and the footer says so (`row cap: statement
2 of 3 reset the session, so the 1 after it did not run`). Esc while a query is running cancels it: the
receiver ends in `Cancelled`, the rows that did arrive stay on screen and the
title reads `cancelled after 1.2 s, 4,500 rows`.

A run of several statements runs them one after another, each started when the
one before it is done. The pane shows the last result set, `[` and `]` switch
between the sets of every statement of the run, and the title counts the rows
of the set on screen. The footer sums the run up,
`3 statements, 2 result sets, 1 row affected`, which is where an update's
count is once a later select is on screen. **A statement the server says no to stops the run**:
the statements after it were written to follow it, so they are dropped rather
than run against whatever state the failure left. The footer says which one it
was (`statement 2 of 3 failed`), the pane shows the driver's own message with
the line number when it gave one — the pad's, as its gutter numbers it — and
the scratch pad paints that statement's lines in the error background until
the next edit. An edit made while the statement ran moves its lines, so then
neither happens and the line is the statement's own. The message is a page
after the sets the run did produce: `[` goes back to them and `]` round to it.
A query that replaces the sets hands the old ones to the run loop, which
frees them on a thread of their own: a million rows are a million
allocations, and giving them back used to hold F5's first frame for 70 ms.

`/` filters the set on screen to the rows with what is typed in any cell,
case-insensitively and as the grid draws it (`null` finds a NULL), and the
title says so: `Results · 2 of 8 rows · 3 ms · /apple`. The hidden rows move
to the end of the set rather than out of it, so the grid, copy, the inspector
and export all see only the matches and clearing costs a sort, not a query.
Enter keeps the filter and gives the keys back; Esc clears it. Like `o`, it
waits for the last row, and a run, `m`, `[`, `]` or `s` drops it.

## Overlays, copy and export

Enter opens the cell inspector over the layout: a 72-column overlay, inside
the tab bar and the footer the way the help is, titled `body · nvarchar ·
102,400 chars` — the column, its type, and the value in characters, or in
bytes for a `Bytes`. Text is wrapped at the overlay's width with its own line
breaks kept, a `Bytes` is a hex dump of sixteen bytes a line with the
printable ones beside it, and a NULL is the word. j k, the arrows and
PageUp/PageDown scroll it, which the grid under it does not see; Esc closes
it, after the help and before a running query, and so does anything that
takes the grid from under it — another pane or tab focused, `/`, or a run
that leaves no cell.

The grid selects a range of cells: the rectangle between an anchor and the
cursor, painted in the selection colour and counted in the title, `Results ·
50 rows · 42 ms · 3×2 selected`. Shift-arrows set the anchor at the cursor
and grow the range, and an unshifted move drops it; `v` sets it and makes
every movement key grow the range until `v` again — `v G` is the column from
the cursor down, `v $` the rest of the row. Dragging over cells selects from
the cell the button went down on, Shift-click from the anchor or the cursor,
and a right-click inside the range keeps it for the menu. Esc drops it, after
cancelling a running query; a sort, `[` or `]`, a new result set, a run or
`m` drop it too.

`y` or Ctrl-C copies the range as tab-separated lines, or the one cell as it
is when there is no range; `Y` copies every column of the rows the range
spans — the cursor's row without one — under a line of column names. A NULL
is nothing, and a value with a tab, a line break or a quote in it is quoted
the way `src/export.rs` quotes CSV, so a spreadsheet pastes one value per
cell. Either goes into the app's clipboard and out to the system's, ends the
range as `y` ends a visual selection in vim, and the footer says `copied 1
cell`, `copied 6 cells` or `copied 3 rows`. In the pad Ctrl-C copies the
selection, or the statement under the cursor when there is none, and Ctrl-X
cuts it.

Out to the system's is two routes at once, both best effort. OSC 52 is
written on every copy, because it is the only one that reaches the clipboard
of the machine a person is sitting at over SSH; and `src/run/clipboard.rs`
pipes the text into a tool on a short-lived thread, so a big copy never holds
up the loop. The tool is looked for once, when a real terminal is claimed, on
`PATH` with no crate: `wl-copy`/`wl-paste --no-newline` when
`WAYLAND_DISPLAY` is set, `xclip -selection clipboard [-o]` or `xsel -b
[-i|-o]` when `DISPLAY` is, `pbcopy`/`pbpaste` on macOS, and `clip.exe` with
PowerShell's `Get-Clipboard` under WSL. `Clipboard` is an enum — no tool, a
tool's two argvs, or a replay's fake — not a trait.

OSC 52 cannot be read back, so Ctrl-V is `Action::ReadClipboard`: a worker
runs the paste tool, kills it after 500 ms, and answers over an `mpsc`
channel the loop polls every 10 ms while one is out. `App::pasted` puts the
answer in that tab's pad (`pasted 3 lines`); no tool, a failure, a timeout
or an empty clipboard fall back to `shell.clipboard`, the app's own (`pasted
1 line from sql-bench's clipboard`), and with that empty too the footer says
`the clipboard is empty`. Ctrl-V and a bracketed paste work from any pane and
move the focus to the pad, except that a paste goes into the finder's query
or the export prompt when one is open, and nowhere while the help, the
inspector or a menu is. Inside tmux, OSC 52 needs `set -g set-clipboard on`;
the tool route does not care. A replay never runs a tool: `clipboard <text>`
sets its fake, and a copy writes the fake, so copy and paste round-trip
headlessly.

`e` opens a one-line prompt in the footer — once the last row is here, and
not over a pane with nothing to write — `Export to: ` prefilled with
`~/sql-bench-<connection>-<YYYYmmdd-HHMMSS>.csv` (the time in UTC, with any
`/` in the name a `_`), which takes every key while it is open — insert,
Backspace, Left, Right, Home, End and Ctrl-U, and Esc to give up; a path
wider than the footer scrolls to keep the cursor on it. Enter writes every fetched row of the set on screen: JSON for a
`.json` name and CSV for anything else, through the same `src/export.rs` the
headless subcommands use, so a file is the same bytes whichever door it left
by. The footer says `exported 1,234 rows to <path>`, or what stopped it.

`?` (F1 in the pad, or anywhere) opens the help over the layout: `<key>  <what it does>` for the keys of
the focused pane only — `Help · Scratch` — with the key column as wide as the
widest key it lists. It is never taller than the screen: a list that does not
fit scrolls with j k, the arrows and PageUp/PageDown, and the title says
which rows are showing (`Help · Scratch (1-11 of 20)`). It takes every key
while it is open, the way a menu does, so nothing typed reaches the pane
hidden under it. `?`, F1 and Esc close it and put it back to the top; Ctrl-Q
still quits.

## Mouse

Every frame says what can be clicked: `ui::render` returns a `Hits`, the
regions it drew in paint order with a `Target` each (`src/app/pointer.rs`),
and `Hits::at` is the last region pushed that holds the pointer. There are no
layers. An overlay pushes a whole-frame `Outside` and then its own body, so
nothing under it can be reached. A `Target` holds indexes and the window it
was drawn from, never a reference, so the hits are plain data.

The four rules hold because the hits flow back as data and nothing else does.
The renderer still only reads the app; it returns the hits, and the run loop
keeps the last frame's and hands them to `App::pointer` with each mouse event
and `Instant::now()`, so the app sees no terminal and reads no clock. After
every draw the loop also hands them to `App::drawn`, which copies the pad's,
the tree's and the grid's drawn windows into their scroll hints (see The
pad), because only the renderer knows how tall a pane is: a key moves the
cursor, and the next frame moves the view only as far as it has to. The number of hits is bounded by the
screen and not by the rows fetched, so drawing them costs what a frame
costs. Replay drives the same `App::pointer` through its mouse verbs against
the same hits, which is how every gesture below is checked without a
terminal (`scripts/replay/qa/mouse.keys`).

- A click fires when the button comes up, on the target that was pressed, and
  only if the pointer never left that target and row on the way. Sliding off
  takes a press back.
- A double-click is a second click on the same target and row within 400 ms.
  It uses both clicks up, so a third is a single again.
- A right-click in a pane selects what is under it, then opens that pane's
  context menu (see Menus). On a tab, the footer, a button or an overlay it
  does what a left click does.
- The wheel scrolls what is under the pointer, by 3, and never moves the
  focus: the help, the inspector, the tree, the grid, an object's source
  and the pad.
  A sideways wheel, or Shift with the wheel, scrolls the grid a column a
  notch. The tree's and the grid's cursors come along only as far as they
  must to stay in view, because the app's window is a hint clamped round
  the cursor.
- A drag selects on pad text, scrolls on a scrollbar's thumb, moves a seam,
  and does nothing anywhere else.
- Two seams move: the left border of Scratch and Results (Objects' right
  border is its scrollbar), and Scratch's bottom border. `shell.split` keeps
  Objects' share of the width and Scratch's of the right-hand column in
  percent, 30 and 40 to start with, so a resize keeps the proportions.
  `Split::areas` lays the panes out and leaves Objects and the right column
  at least 20 columns, Scratch and Results at least 3 rows, at every size, so
  a split made on a big screen survives a small one; a 60-column screen gets
  a 20-column Objects rather than 30 % of it. A `Target::Seam` carries the
  area it divides, which turns the pointer back into a percent. It is pushed
  after the panes and before what they draw on their borders, so title
  buttons stay on top. A held seam is lit in the accent colour on the hover
  ground wherever the pointer goes, and a drag draws a frame only when the
  split changes. A click on a seam does nothing and focuses neither side; a
  double-click puts 30/40 back. The split is not saved between runs.
- The tree, the grid's rows, the pad and an object's source get a scrollbar
  over their pane's right border, between its corners, while there is more
  than fits. The track keeps the border's glyph and the thumb is `┃`, so it
  reads under `NO_COLOR`. One pure function, `pointer::thumb(offset, content,
  viewport, track)`, places the thumb for the painter, and its inverse
  `pointer::offset` turns a dragged thumb back into a row, so what is drawn is
  what is hit, in O(1). The track above and below the thumb is a PageUp and a
  PageDown button for the pane; a page moves the view as far as the cursor,
  so the cursor keeps its row on the screen and a click on the track always
  scrolls. A `Target::Thumb` carries the content, the
  viewport and the track it was drawn on; dragging it moves the view through
  the pane's wheel function, which pulls the cursor along the same way, and
  like the wheel it leaves the focus alone. Pressing it and letting go does
  nothing.
- Clicking a tab shows it. Clicking anywhere in a pane focuses it.
- In the tree a click moves the cursor to the row, a click on its `▸` or
  `▾` is Space, and a double-click is Enter: a table's select goes into the
  pad, a procedure's source into the results pane. In the grid a click
  selects the cell, a double-click is Enter (the inspector), and a click on
  a header selects its column and presses `o`, which sorts by it: ascending,
  descending, then the order the rows came in, with the header's last
  character turned into `▲` or `▼` so a narrow column still shows it. Those
  are the keys themselves, pressed after the click has moved the cursor, so
  a load, a select or a sort is decided in one place.
- A click never moves the view. `Tree`, `Cells`, `Header` and `Source`
  carry the window they were drawn from (`top`, and the grid's `left`), and
  `Objects::click` and `Results::click` set the scroll hint from that and
  the cursor directly, or a column hint `h` and `l` left behind would pull
  the columns back left.
- A click beside the help, the inspector, the export prompt or a menu closes
  the one on top, the way Esc does, and reaches nothing under it.
- What the pointer rests on is painted in the theme's hover style, restyled
  over the finished frame from the same hits a click reads. Only things that
  are there to be clicked light up: a tab, a button, a thumb or a seam, not
  a pane.

A press paints nothing, and the pointer moving costs a frame only when the
target or row under it changes, so resting the mouse on the app costs one
frame and not one per event. The loop handles a burst of events together, but
a mouse event behind one that changed the screen is held for the next turn,
after the new frame is drawn, so it lands on what the person was looking at.
Mouse capture goes on with the alternate screen and off before it is left,
for the editor Ctrl-E opens as well as on the way out (see Terminal restore).

### Buttons

A button is `Target::Button { pane, key }`, and clicking it focuses `pane`
and runs `App::key(key)`, so a button can do nothing a key cannot. The title
bars carry them right-aligned over the top border (`▶ Run`, `▶▶ All`,
`✎ Editor`, `■ Stop` over Scratch; `Export`, `+10k`, `◀`, `▶`, `■ Cancel`
over Results; `⟳ Reload`, `/ Filter`, `×` over Objects), dropped from the
left when the title needs the room. The placeholders `[ Connect ]`,
`[ Retry ]` and `[ Clear filter ]` are buttons, as are `? Help` at the end
of the tab bar, every footer hint that names one key, the footer's
connection state (`c` or `C`, always the Objects pane's), the error's `×`,
the export prompt's `Export` and `Cancel`, and the help's and the
inspector's `×`. A click on the prompt's text puts its cursor there.

A button is drawn only where its key does what the button says. Esc cancels
a running query before it clears a filter or an error, so the filter's `×`
and the error's `×` are hidden while one runs, and the error's also while
the tree has a filter; `[`, `]`, `m` and `e` do nothing over an object's
source, so their buttons are not there either. The footer's Esc hint is not
a button at all: while the hints show, nothing is running and no help is
open, so Esc has nothing to do of what it says. Two clicks do a little more
than their key, on purpose. A help row closes the help and then presses its
key, because the help keeps the scroll keys for itself. Any click in
Objects first ends typing the filter, the way Enter does, so a button's
key is not typed into it. `every_button_drawn_is_exactly_its_key_and_does_something`
in `src/ui/tests/mouse.rs` clicks every button in a table of states and
presses its key on a copy, and the two apps have to come out the same.

### Menus

A right-click in a pane selects without acting — the row, the cell, the
header's column, the pad's cursor, but not the glyph's Space, the header's
`o` or a double-click, which are entries of the menu — and then opens
`shell.mouse.menu` at the pointer. In the pad a right-click inside the
selection keeps it, so the menu's Ctrl-C copies it. On a pane's border or an
empty pane it opens the menu and moves nothing; on a tab, the footer, a
button or an overlay it is a left click and opens none.

`pointer::MENU` names the entries as rows of `KEYS`: Objects Enter, s, i, y,
r, /, Space; Results Enter, y, Y, o, e, m, [, ]; Scratch Ctrl-R, F5, Ctrl-C,
Ctrl-X, Ctrl-V, Ctrl-A, Ctrl-Z, Ctrl-E. Their labels are read through
`keys_for(pane)`, because `KEYS` has an Enter and a `y` for more than one
pane, and a test checks every name is a key of its pane. Each entry is what it does on the left and its key
on the right. The box opens right and down from the pointer, or left and up
from it where that would run off the frame, clamped to it at every size down
to 60x15. It pushes `Outside`, its body, then a `Target::MenuItem(n)` per
entry, and is drawn after everything else; the entry Enter would pick is
painted like the pad's cursor, and the pointer lights the one it rests on.

Picking an entry, by a click or by Enter, closes the menu, focuses its pane
and runs `App::key` with its key, so it is exactly the key:
`picking_each_entry_is_exactly_its_key` checks a click, `j`s and Enter, and
the key itself come out the same in a table of states. An entry whose key
has nothing to act on in the current state stays in the menu and does what
the key does, which is nothing or a footer message; the "does something"
half of the check is made only in the state where every key has work.

While the menu is open it takes every key, ahead of the prompt, the help and
Esc's other meanings: Up, Down, `j` and `k` move the highlight, Enter picks,
and any other key closes the menu and goes no further, Esc included. A click
beside it closes it and reaches nothing. The help, the inspector and the
prompt only open from a key or a click, which the menu takes, so none of
them is ever open under it.

### The pad

The pad keeps a scroll hint, the line and column its pane starts from, and
`Scratch::window` clamps it just far enough to put the cursor on screen. The
run loop hands each frame's `Target::Pad { top, left, gutter }` back through
`App::drawn`, so the hint is always what was last drawn and a key that moves
the cursor inside the view leaves the view alone. Only the renderer knows how
tall the pad is; this is how the app follows it without asking.

- The button going down puts the cursor on the character under it (a column
  counts characters, one per cell), clamped to the line and to the last
  line. The gutter is column 0. Shift extends the selection instead.
- A click focuses Scratch. A double-click selects the run of letters, digits
  and underscores under the pointer, and nothing on anything else.
- A drag selects from where the button went down. Above or below the pad it
  is the line just past the edge, so the view scrolls a line per move.
- The wheel moves the drawn window by 3 lines and pulls the cursor along only
  as far as it has to, the way vim's Ctrl-E does, extending a selection if
  there is one. It stops with the last line at the bottom.
- None of it is an edit: no undo snapshot, no `[modified]`, no save.

## The scratch pad

One pad per connection, kept in `<state dir>/scratch/<connection>.sql`, where
a `/`, a `\` or a leading `.` in the name is written as its `%xx` so the file
stays in that directory. A pad is written beside its file and renamed over
it, and one that is not UTF-8 is loaded with `�` for what is not rather than
left empty to be saved over. The
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
`Ctrl-T`, `Ctrl-P`, `Ctrl-Q` and `F1` work everywhere. `Tab` is in the table
twice for the same reason — next pane outside the pad, two spaces inside it.
The finder's own keys — the arrows, Ctrl-N and Ctrl-P, the pages, Enter and
Esc — are not in the table, the way the help overlay's and the inspector's
are not: an overlay takes the keys of the pane under it and gives them back
when it closes.

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
ignored. Every argument is trimmed except `type`'s, `paste`'s and `clipboard`'s,
which are the rest of the line exactly as written.

| command | does |
|---|---|
| `key <name>` | one key press, as crossterm would deliver it |
| `type <text>` | one key press per character, spaces included |
| `paste <text>` | one `Event::Paste` with the whole text |
| `clipboard <text>` | put the text on the replay's fake system clipboard, which Ctrl-V reads |
| `resize <cols>x<rows>` | resize the backend and send `Event::Resize` |
| `wait busy` | until nothing is connecting or running; 60 s, then exit 3 |
| `wait <ms>` | sleep that many milliseconds |
| `wait text <substring>` | until the substring is on the frame; 30 s, then exit 3 |
| `frame <name>` | write `<frames-dir>/<name>.txt` now |
| `expect <substring>` | the substring is on the frame, or exit 4 |
| `expect-not <substring>` | the substring is not on the frame, or exit 4 |
| `click <where>` | left button down and up there |
| `double-click <where>` | two clicks there, inside the double-click time |
| `right-click <where>` | right button down and up there |
| `hover <where>` | the pointer moves there, no button down |
| `scroll up\|down\|left\|right <where>` | one notch of the wheel there |
| `drag X Y X Y` | left button down at the first cell, moved to the second, up there |

`<where>` is `X Y`, a column and a row counted from 0 the way a frame file
counts them, or `on <substring>`: the first cell of the first place the text
is on the frame, found cell by cell so that the blank after a wide glyph is
not part of what has to be typed. Text that is not on the frame is exit 4,
like a failed `expect`. Two clicks less than 400 ms apart on one target and
row are a double-click here too, even on different cells of it (a tree row's
glyph and then its name); put `wait 500` between them to keep them apart.

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
comes out as itself followed by a space. ratatui never sends the cell a wide
glyph covers, counting on the terminal to blank it, and `TestBackend` does
not, so after every turn the replay blanks it itself; otherwise a row keeps
whatever an older frame left there. That is fine for an `expect` and
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
frame and its persistence, the two terminal-restore checks below, the
stdin-EOF check, `scripts/qa/mouse-bytes.sh` and the draw latency. CI runs
it.

Everything that needs the two containers is behind `SQL_BENCH_TEST_DBS=1` in
the same script: `scripts/replay/qa/mouse.keys` at 120x40 and 80x24, every
gesture against SQL Server; the password-leak check, the `--max-rows` timing,
the connection lifecycle, the query and object replays, the query workflow, a
database outage, and `scripts/readme-frame.sh --check`. CI has no databases,
so the README's frame is regenerated locally — run `scripts/readme-frame.sh`
after anything that changes the layout, and commit what it writes. The
comparison ignores the milliseconds in the footer and the results title, which
are a clock and not a layout.

## Terminal restore

Four ways out of a run, and all four give the terminal back. A quit (`q`,
`Ctrl-Q`), an input that ran out, and an error returning `Err` up to `main`
all drop the `Restore` guard in `src/run/mod.rs`, whose `release_terminal`
turns mouse capture and bracketed paste off (`\e[?1000l` and `\e[?2004l`),
then raw mode, then leaves the alternate screen; the terminal's own `Drop`
shows the cursor after it. A shell handed a terminal that still reports the
mouse gets escape codes typed at it whenever the pointer moves, which is why
the mouse goes first. A panic runs the hook `ratatui::try_init` installed —
raw mode off, alternate screen left, *then* the message, so it lands on the
normal screen — and unwinds through the same guard, which turns the mouse off
and leaves the alternate screen once more. Either way `\e[?1000l` is written
before the last `\e[?1049l`.

Ctrl-E is the one way out that comes back: `release_terminal` hands the
terminal to `$VISUAL` or `$EDITOR` exactly as a quit would, and
`claim_terminal` takes raw mode, the alternate screen, bracketed paste and
the mouse back in the order startup took them.

A replay never takes the terminal at all, so QA checks this on a real pty:
`scripts/qa/panic-restore.sh` runs a debug build under `script` with
`--panic-after-ms` — a hidden flag that exists only under
`cfg(debug_assertions)`, and that panics where the loop waits for a key, so no
key has to arrive for it to fire — and asserts that the capture has the shell
drawn on it, that the run exits 101, and that `\e[?1049l\e[?25h` comes after
the panic message and is the last thing written, with `\e[?1000l` before it.

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
`\e[?1049l\e[?25h` is the last thing written, with `\e[?1000l` before it.

Replay injects crossterm events, so it never sees how a terminal encodes the
mouse. `scripts/qa/mouse-bytes.sh` types SGR mouse reports (`\e[<0;x;yM` and
`m`) into a pty under `script`: a click in the pad, Ctrl-E with `true` as the
editor, a click on `? Help`, Ctrl-Q. It asserts capture was asked for
(`\e[?1000h`, `\e[?1006h`), that `Help · Scratch` was drawn — so both
clicks were decoded and landed — that capture went off and on again around
the editor, and that the last `\e[?1000l` comes before the last `\e[?1049l`.
How tmux and a real terminal pass the mouse on is checked by hand.

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
| sort | rows, ms | `o` or a header click sorted the grid; `ms` is the whole event, which is the sort |

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
leaves it. The only other one is the signal thread in `run/mod.rs`, which
waits for SIGTERM and SIGHUP so a killed run still gives the terminal back;
tokio's `signal` feature is cheaper than a signal crate for two signals.

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

**One index per connection, searched in memory.** The obvious finder asks
the server `where name like '%…%'` on every keystroke, and the obvious tree
loads a branch at a time; ten databases of ten thousand objects each make
both of them a wait on every key. An index is one catalog query per
connection, a hundred thousand `DbObject`s are a few megabytes, and a pass
over as many lower-cased names is a millisecond — so the finder answers a
keystroke from memory, the tree opens a branch from memory, and the server
is asked for the things that are actually big: a table's columns, an
object's source. The cost is that the index is as old as the connection;
`r` on a kind asks for it again, and a reconnect always does.

**The pad types `?`; F1 is the help key that works everywhere.** `?` was
once help everywhere, including the pad, but it is a character SQL needs — a
`LIKE` pattern, a comment, a string, a driver placeholder — and a pad that
cannot type it cannot hold every statement. `?` stays help in the other
panes, and F1 and the `? Help` button reach it from the pad.

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
