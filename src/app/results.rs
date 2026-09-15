//! One tab's result grid: what a running query has reported so far, where
//! the cell cursor is, and what the pane's title says about it.
//!
//! Rows arrive a batch at a time and are kept as they came — the column
//! widths are the only thing recomputed, and only over the new batch — so a
//! draw costs the visible window and never the whole scan. Everything the
//! renderer needs is a pure read of this state.
//!
//! The one clock the app reads is here: `Running · 2.1 s` and `cancelled
//! after 1.2 s` cannot come from stored state alone, and a timer the run
//! loop pushed in every turn would be the same clock with more moving parts.

use std::borrow::Cow;
use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::db::catalog::ColumnInfo;
use crate::db::model::{Cell, Column, DbError, QueryEvent};

/// The widest a column is drawn, however long its values are. A column of
/// 2 kB payloads would otherwise push every other column off the screen.
pub const WIDTH_CAP: usize = 40;

/// How far PageUp and PageDown go, and half of what Ctrl-D and Ctrl-U do.
/// The pane's real height is not known here — the renderer clamps the window
/// it is given — so this is the same fixed page the scratch pad uses.
const PAGE: usize = 10;

/// What `m` asks for on top of the cap that truncated the last run.
pub const MORE_ROWS: usize = 10_000;

/// What a key in the results pane meant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hit {
    /// Not one of the grid's keys.
    Ignored,
    /// The cursor or the result set moved; the screen has to be painted.
    Moved,
    /// `m`: run the same statement again with a higher cap.
    MoreRows,
    /// Enter: open the whole of the selected cell.
    Inspect,
    /// `y`: the selected cell, as text.
    CopyCell,
    /// `Y`: the whole row, tab-separated.
    CopyRow,
    /// `e`: ask where to write the result set.
    Export,
}

/// The open cell inspector: an overlay showing one whole value.
///
/// The cell is read from the grid when the overlay is drawn rather than
/// copied when it opens, so a hundred kilobyte value costs an overlay and
/// not a second copy of itself.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Inspector {
    /// The first line of the value that is showing.
    pub scroll: usize,
}

/// How wide the inspector wraps its value, which is the inside of an overlay
/// four columns wider: two of border, two of padding. The app wraps and the
/// renderer draws, so both have to mean the same width — and a hex dump line
/// is what fixes it, being as wide as sixteen bytes make it.
pub const INSPECT_WIDTH: usize = 68;

/// Where a query is.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Running {
        since: Instant,
        rows_so_far: usize,
    },
    Done {
        rows: usize,
        truncated: bool,
        elapsed: Duration,
    },
    /// The driver said no — or Esc cancelled it, which is
    /// [`DbError::Cancelled`] and still shows the rows that did arrive.
    Failed {
        error: DbError,
        elapsed: Duration,
        rows: usize,
    },
}

/// Which way a column's values are drawn. A column holds numbers when
/// something in it is one and nothing in it is anything else, and those are
/// the columns that read better against the right edge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Align {
    /// Nothing but NULLs so far.
    #[default]
    Unknown,
    Right,
    Left,
}

/// What `s` on a procedure put in the pane: its text, read only, with its
/// own scroll. It is not a result set — there are no columns to move a cell
/// cursor across — so it sits beside them rather than among them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source {
    pub title: String,
    pub lines: Vec<String>,
    /// The first line showing; the renderer clamps it to the pane.
    pub scroll: usize,
}

/// One result set of one statement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Set {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    /// Character widths, capped at [`WIDTH_CAP`] and never below the header.
    pub widths: Vec<usize>,
    pub align: Vec<Align>,
}

/// One tab's results pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    sets: Vec<Set>,
    /// Which set `[` and `]` have on screen.
    shown: usize,
    pub status: Status,
    /// The cell cursor: (row, column) of the set on screen.
    selected: (usize, usize),
    /// Where the window starts. The pane's height is not known here, so this
    /// is a hint [`Results::window`] clamps against the real one.
    scroll: (usize, usize),
    /// What the last statement changed, when it changed rather than returned.
    pub rows_affected: Option<u64>,
    /// The object source `s` is showing instead of a grid.
    source: Option<Source>,
    /// What the pane is showing when it is not the last statement's rows:
    /// `bench.customers columns`.
    label: Option<String>,
    /// The lines of the statements this run was asked for, in order, so the
    /// pad can flag the one that failed. They never leave the app.
    statements: Vec<Range<usize>>,
    /// The statement running now, as an index into `statements`.
    current: usize,
    /// How many statements of this run have started, how many there are, and
    /// what they have produced — which is the run-all summary.
    ran: usize,
    of: usize,
    sets_total: usize,
    affected_total: u64,
}

impl Results {
    /// A statement is starting. `keep_view` is `m` asking for more rows of
    /// the same statement, which keeps the cursor where the person left it.
    pub fn start(&mut self, at: Instant, statement: usize, of: usize, keep_view: bool) {
        if statement == 0 && !keep_view {
            self.ran = 0;
            self.sets_total = 0;
            self.affected_total = 0;
        }
        self.sets.clear();
        self.shown = 0;
        self.rows_affected = None;
        self.source = None;
        self.label = None;
        if !keep_view {
            self.selected = (0, 0);
            self.scroll = (0, 0);
        }
        self.current = statement;
        self.ran = statement + 1;
        self.of = of;
        self.status = Status::Running {
            since: at,
            rows_so_far: 0,
        };
    }

    /// The line ranges of the statements a run was asked for, kept so that a
    /// failure can say which one it was.
    pub fn expect(&mut self, statements: Vec<Range<usize>>) {
        self.statements = statements;
    }

    /// The lines of the statement that is running, or that just failed.
    #[must_use]
    pub fn statement_lines(&self) -> Option<Range<usize>> {
        self.statements.get(self.current).cloned()
    }

    /// One event from the running query.
    pub fn apply(&mut self, event: QueryEvent) {
        match event {
            QueryEvent::Columns(columns) => {
                self.sets_total += 1;
                let widths = columns
                    .iter()
                    .map(|column| header_width(column).min(WIDTH_CAP))
                    .collect();
                let align = vec![Align::default(); columns.len()];
                self.sets.push(Set {
                    columns,
                    rows: Vec::new(),
                    widths,
                    align,
                });
                // The newest set is the one on screen: a statement's last
                // result is what a person asked the statement for.
                self.shown = self.sets.len() - 1;
            }
            QueryEvent::Rows(batch) => self.keep(batch),
            QueryEvent::RowsAffected(rows) => {
                self.rows_affected = Some(self.rows_affected.unwrap_or(0) + rows);
                self.affected_total += rows;
            }
            QueryEvent::Done {
                rows,
                truncated,
                total_ms,
                ..
            } => {
                self.status = Status::Done {
                    rows,
                    truncated,
                    elapsed: Duration::from_millis(u64::from(total_ms)),
                };
            }
            QueryEvent::Error(error) => {
                let (elapsed, rows) = match &self.status {
                    Status::Running { since, rows_so_far } => (since.elapsed(), *rows_so_far),
                    _ => (Duration::ZERO, self.rows().len()),
                };
                self.status = Status::Failed {
                    error,
                    elapsed,
                    rows,
                };
            }
        }
    }

    /// A batch, kept as it came; only the new rows are measured.
    fn keep(&mut self, batch: Vec<Vec<Cell>>) {
        if let Status::Running { rows_so_far, .. } = &mut self.status {
            *rows_so_far += batch.len();
        }
        let Some(set) = self.sets.last_mut() else {
            return;
        };
        for row in &batch {
            for (index, cell) in row.iter().enumerate() {
                let Some(width) = set.widths.get_mut(index) else {
                    continue;
                };
                if *width < WIDTH_CAP {
                    *width = (*width).max(shown(cell).chars().count()).min(WIDTH_CAP);
                }
                if let Some(align) = set.align.get_mut(index) {
                    *align = match (*align, cell) {
                        (Align::Left, _) | (_, Cell::Bool(_) | Cell::Text(_) | Cell::Bytes(_)) => {
                            Align::Left
                        }
                        (_, Cell::Int(_) | Cell::Float(_) | Cell::Decimal(_)) => Align::Right,
                        (align, _) => align,
                    };
                }
            }
        }
        set.rows.extend(batch);
    }

    /// One key of the results pane.
    pub fn key(&mut self, key: KeyEvent) -> Hit {
        if self.source.is_some() {
            return self.source_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        #[allow(clippy::cast_possible_wrap)]
        let page = PAGE as isize;
        match key.code {
            KeyCode::Char('d' | 'D') if control => self.by_rows(page / 2),
            KeyCode::Char('u' | 'U') if control => self.by_rows(-page / 2),
            KeyCode::Char('j') | KeyCode::Down => self.by_rows(1),
            KeyCode::Char('k') | KeyCode::Up => self.by_rows(-1),
            KeyCode::Char('h') | KeyCode::Left => self.by_columns(-1),
            KeyCode::Char('l') | KeyCode::Right => self.by_columns(1),
            KeyCode::PageDown => self.by_rows(page),
            KeyCode::PageUp => self.by_rows(-page),
            KeyCode::Char('g') => self.at_row(0),
            KeyCode::Char('G') => self.at_row(usize::MAX),
            KeyCode::Char('0') => self.at_column(0),
            KeyCode::Char('$') => self.at_column(usize::MAX),
            KeyCode::Char('[') => self.switch_set(-1),
            KeyCode::Char(']') => self.switch_set(1),
            KeyCode::Char('m') => Hit::MoreRows,
            KeyCode::Enter => Hit::Inspect,
            KeyCode::Char('y') => Hit::CopyCell,
            KeyCode::Char('Y') => Hit::CopyRow,
            KeyCode::Char('e') => Hit::Export,
            _ => Hit::Ignored,
        }
    }

    fn by_rows(&mut self, delta: isize) -> Hit {
        self.at_row(self.selected.0.saturating_add_signed(delta))
    }

    fn by_columns(&mut self, delta: isize) -> Hit {
        self.at_column(self.selected.1.saturating_add_signed(delta))
    }

    fn at_row(&mut self, row: usize) -> Hit {
        let row = row.min(self.rows().len().saturating_sub(1));
        self.selected.0 = row;
        if row < self.scroll.0 {
            self.scroll.0 = row;
        } else if row >= self.scroll.0 + PAGE {
            self.scroll.0 = row + 1 - PAGE;
        }
        Hit::Moved
    }

    fn at_column(&mut self, column: usize) -> Hit {
        let column = column.min(self.columns().len().saturating_sub(1));
        self.selected.1 = column;
        self.scroll.1 = self.scroll.1.min(column);
        Hit::Moved
    }

    /// `[` and `]`, wrapping round the way Ctrl-T wraps round the tabs.
    fn switch_set(&mut self, delta: isize) -> Hit {
        if self.sets.len() < 2 {
            return Hit::Moved;
        }
        let count = self.sets.len();
        self.shown = (self.shown + count).saturating_add_signed(delta) % count;
        self.selected = (0, 0);
        self.scroll = (0, 0);
        Hit::Moved
    }

    #[must_use]
    pub fn set(&self) -> Option<&Set> {
        self.sets.get(self.shown)
    }

    #[must_use]
    pub fn columns(&self) -> &[Column] {
        self.set().map_or(&[], |set| &set.columns)
    }

    #[must_use]
    pub fn rows(&self) -> &[Vec<Cell>] {
        self.set().map_or(&[], |set| &set.rows)
    }

    #[must_use]
    pub fn widths(&self) -> &[usize] {
        self.set().map_or(&[], |set| &set.widths)
    }

    #[must_use]
    pub fn align(&self) -> &[Align] {
        self.set().map_or(&[], |set| &set.align)
    }

    #[must_use]
    pub const fn selected(&self) -> (usize, usize) {
        self.selected
    }

    /// The cell the cursor is on, which is what Enter inspects and `y`
    /// copies. [`None`] when the set has no rows.
    #[must_use]
    pub fn cell(&self) -> Option<&Cell> {
        self.rows().get(self.selected.0)?.get(self.selected.1)
    }

    /// The column that cell is in, for the inspector's title.
    #[must_use]
    pub fn column(&self) -> Option<&Column> {
        self.columns().get(self.selected.1)
    }

    /// The row the cursor is on, tab-separated — which is what a spreadsheet
    /// pastes into columns. A NULL is nothing, the way an export writes it.
    #[must_use]
    pub fn row_text(&self) -> Option<String> {
        let row = self.rows().get(self.selected.0)?;
        let mut text = String::new();
        for (index, cell) in row.iter().enumerate() {
            if index > 0 {
                text.push('\t');
            }
            text.push_str(&cell.display());
        }
        Some(text)
    }

    #[must_use]
    pub const fn sets(&self) -> usize {
        self.sets.len()
    }

    /// Which set is on screen, counting from one, for `set 1/2`.
    #[must_use]
    pub const fn shown(&self) -> usize {
        self.shown + 1
    }

    /// How many statements of this run have started, and how many there
    /// are: `statement 2 of 3 failed`.
    #[must_use]
    pub const fn progress(&self) -> (usize, usize) {
        (self.ran, self.of)
    }

    #[must_use]
    pub const fn running(&self) -> bool {
        matches!(self.status, Status::Running { .. })
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        matches!(
            self.status,
            Status::Done {
                truncated: true,
                ..
            }
        )
    }

    /// The failure the pane shows the message of — a cancel is not one,
    /// because the rows it did fetch are still worth looking at.
    #[must_use]
    pub const fn failure(&self) -> Option<&DbError> {
        match &self.status {
            Status::Failed { error, .. } if !matches!(error, DbError::Cancelled) => Some(error),
            _ => None,
        }
    }

    /// `3 statements, 2 result sets, 1 rows affected`, for a run of more
    /// than one statement.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        (self.of > 1).then(|| {
            format!(
                "{} statements, {} result sets, {} rows affected",
                self.ran, self.sets_total, self.affected_total
            )
        })
    }

    /// The pane's title, which is where every number about the last run is.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(source) = &self.source {
            return format!("Source · {} · {} lines", source.title, source.lines.len());
        }
        let set = if self.sets.len() > 1 {
            format!(" · set {}/{}", self.shown(), self.sets.len())
        } else {
            String::new()
        };
        match &self.status {
            Status::Idle => "Results".to_owned(),
            Status::Running { since, rows_so_far } => format!(
                "Running · {} · {} rows",
                seconds(since.elapsed()),
                grouped(*rows_so_far)
            ),
            Status::Done {
                rows,
                truncated,
                elapsed,
            } => {
                let what = match (self.rows_affected, *rows) {
                    (Some(affected), 0) => format!("{} rows affected", grouped_u64(affected)),
                    _ => format!(
                        "{} rows{}",
                        grouped(*rows),
                        if *truncated { " (truncated)" } else { "" }
                    ),
                };
                match &self.label {
                    Some(label) => format!("{label} · {what}"),
                    None => format!(
                        "Results{set} · {what} · {} ms",
                        grouped(usize::try_from(elapsed.as_millis()).unwrap_or(usize::MAX))
                    ),
                }
            }
            Status::Failed {
                error: DbError::Cancelled,
                elapsed,
                rows,
            } => format!(
                "Results{set} · cancelled after {}, {} rows",
                seconds(*elapsed),
                grouped(*rows)
            ),
            Status::Failed { .. } => "Results · failed".to_owned(),
        }
    }

    /// The first row and the first column of a window `rows` high and
    /// `width` characters wide, so that the selected cell is on it.
    ///
    /// The pane's size is known here and nowhere else, which is why the
    /// scroll this clamps is a hint rather than the truth.
    #[must_use]
    pub fn window(&self, rows: usize, width: usize) -> (usize, usize) {
        let rows = rows.max(1);
        let count = self.rows().len();
        let top = self
            .scroll
            .0
            .min(self.selected.0)
            .max(self.selected.0.saturating_sub(rows - 1))
            .min(count.saturating_sub(rows));
        let mut left = self.scroll.1.min(self.selected.1);
        let widths = self.widths();
        while left < self.selected.1 && span(widths, left, self.selected.1) > width {
            left += 1;
        }
        (top, left)
    }
}

/// The object browser's two views of the pane: a table's columns as a grid,
/// and an object's source as text. Both replace whatever the last statement
/// left, and the next statement replaces them.
impl Results {
    /// `i`: the columns as a result set, so the grid draws them the way it
    /// draws everything else.
    pub fn show_columns(&mut self, label: String, columns: &[ColumnInfo]) {
        let rows: Vec<Vec<Cell>> = columns
            .iter()
            .map(|column| {
                vec![
                    Cell::Text(column.name.clone()),
                    Cell::Text(column.type_text.clone()),
                    Cell::Text(if column.nullable { "yes" } else { "no" }.to_owned()),
                    Cell::Text(if column.is_pk { "yes" } else { "no" }.to_owned()),
                ]
            })
            .collect();
        let count = rows.len();
        self.start(Instant::now(), 0, 1, false);
        self.apply(QueryEvent::Columns(
            ["name", "type", "nullable", "pk"]
                .into_iter()
                .map(|name| Column {
                    name: name.to_owned(),
                    type_name: String::new(),
                })
                .collect(),
        ));
        self.apply(QueryEvent::Rows(rows));
        self.apply(QueryEvent::Done {
            rows: count,
            truncated: false,
            connect_ms: 0,
            first_row_ms: 0,
            total_ms: 0,
        });
        self.label = Some(label);
    }

    /// `s`: the text that made the object, read only.
    pub fn show_source(&mut self, title: String, text: &str) {
        self.source = Some(Source {
            title,
            lines: text.lines().map(str::to_owned).collect(),
            scroll: 0,
        });
    }

    #[must_use]
    pub const fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// The source view's keys: the grid's movement keys, over lines.
    fn source_key(&mut self, key: KeyEvent) -> Hit {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        #[allow(clippy::cast_possible_wrap)]
        let page = PAGE as isize;
        let Some(source) = self.source.as_mut() else {
            return Hit::Ignored;
        };
        let last = source.lines.len().saturating_sub(1);
        let by = |scroll: usize, delta: isize| scroll.saturating_add_signed(delta).min(last);
        source.scroll = match key.code {
            KeyCode::Char('d' | 'D') if control => by(source.scroll, page / 2),
            KeyCode::Char('u' | 'U') if control => by(source.scroll, -page / 2),
            KeyCode::Char('j') | KeyCode::Down => by(source.scroll, 1),
            KeyCode::Char('k') | KeyCode::Up => by(source.scroll, -1),
            KeyCode::PageDown => by(source.scroll, page),
            KeyCode::PageUp => by(source.scroll, -page),
            KeyCode::Char('g') => 0,
            KeyCode::Char('G') => last,
            _ => return Hit::Ignored,
        };
        Hit::Moved
    }
}

/// How wide the columns `from..=to` are drawn, separators included.
fn span(widths: &[usize], from: usize, to: usize) -> usize {
    widths
        .iter()
        .take(to + 1)
        .skip(from)
        .map(|width| width + 2)
        .sum::<usize>()
        .saturating_sub(2)
}

// ponytail: widths are counted in characters, so a CJK name drawn two cells
// wide leans the columns after it — the same ceiling `export::width` names,
// and the same fix: ratatui's own `Span::width` if a grid of Chinese text is
// ever worth the rewrite.
/// A header is as wide as the longer of its two lines, because the type row
/// is drawn under the name in the same column.
fn header_width(column: &Column) -> usize {
    column
        .name
        .chars()
        .count()
        .max(column.type_name.chars().count())
}

/// What a cell reads as in the grid. It differs from [`Cell::display`] in
/// the three ways a grid cares about: a NULL says so, a blob is its first
/// bytes rather than all of them, and a newline is one glyph wide.
#[must_use]
pub fn shown(cell: &Cell) -> Cow<'_, str> {
    match cell {
        Cell::Null => Cow::Borrowed("NULL"),
        Cell::Bytes(bytes) => Cow::Owned(short_hex(bytes)),
        Cell::Text(text) | Cell::DateTime(text) | Cell::Decimal(text)
            if text.contains(['\n', '\r']) =>
        {
            Cow::Owned(text.replace(['\n', '\r'], "⏎"))
        }
        other => other.display(),
    }
}

/// `0x` and the first sixteen bytes, because a megabyte of BLOB formatted
/// every frame is a megabyte of hex nobody reads.
fn short_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(2 + 32 + 3);
    text.push_str("0x");
    for byte in bytes.iter().take(16) {
        let _ = write!(text, "{byte:02x}");
    }
    if bytes.len() > 16 {
        text.push('…');
    }
    text
}

/// `body · nvarchar(max) · 102,400 chars`: what the inspector is showing,
/// and how much of it there is. Bytes are counted in bytes and everything
/// else in characters, because that is the unit each was stored in.
#[must_use]
pub fn inspect_title(column: &Column, cell: &Cell) -> String {
    let size = match cell {
        Cell::Null => "NULL".to_owned(),
        Cell::Bytes(bytes) => format!("{} bytes", grouped(bytes.len())),
        other => format!("{} chars", grouped(other.display().chars().count())),
    };
    format!("{} · {} · {size}", column.name, column.type_name)
}

// ponytail: every line of the value is built on every key and every frame,
// so the 1 MiB a LOB stops at is 1 MiB formatted per keystroke. Cheap enough
// for a value a person opened on purpose; window it if a megabyte of hex ever
// shows up in a trace.
/// The whole of one cell, a line at a time: text wrapped at
/// [`INSPECT_WIDTH`] with its own line breaks kept, bytes as a hex dump, and
/// NULL as the word the grid shows.
#[must_use]
pub fn inspect_lines(cell: &Cell) -> Vec<String> {
    match cell {
        Cell::Null => vec!["NULL".to_owned()],
        Cell::Bytes(bytes) => bytes
            .chunks(HEX_PER_LINE)
            .enumerate()
            .map(|(line, chunk)| hex_line(line * HEX_PER_LINE, chunk))
            .collect(),
        other => wrapped(&other.display()),
    }
}

/// How many bytes one hex dump line holds.
const HEX_PER_LINE: usize = 16;

/// `000000  4c6f7265 6d206970 73756d20 4c6f7265  Lorem ipsum Lore`: the
/// offset, the bytes in fours, and the printable ones again on the right.
fn hex_line(offset: usize, chunk: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{offset:06x}  ");
    for group in 0..HEX_PER_LINE / 4 {
        if group > 0 {
            text.push(' ');
        }
        for index in group * 4..group * 4 + 4 {
            match chunk.get(index) {
                Some(byte) => {
                    let _ = write!(text, "{byte:02x}");
                }
                None => text.push_str("  "),
            }
        }
    }
    text.push_str("  ");
    for byte in chunk {
        text.push(if byte.is_ascii_graphic() || *byte == b' ' {
            char::from(*byte)
        } else {
            '.'
        });
    }
    text
}

/// The text in lines of [`INSPECT_WIDTH`] characters, cut where it is too
/// long and broken where it breaks itself.
fn wrapped(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut characters = paragraph
            .strip_suffix('\r')
            .unwrap_or(paragraph)
            .chars()
            .peekable();
        loop {
            lines.push(characters.by_ref().take(INSPECT_WIDTH).collect::<String>());
            // A paragraph exactly as wide as the overlay is one line and not
            // one line and an empty one.
            if characters.peek().is_none() {
                break;
            }
        }
    }
    lines
}

/// A cell cut to `width` characters, the last of which says there was more.
#[must_use]
pub fn cut(text: &str, width: usize) -> Cow<'_, str> {
    if width == 0 || text.chars().count() <= width {
        return Cow::Borrowed(text);
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    Cow::Owned(cut)
}

/// `1,234`: a row count is read, not computed with.
#[must_use]
pub fn grouped(number: usize) -> String {
    grouped_u64(u64::try_from(number).unwrap_or(u64::MAX))
}

#[must_use]
pub fn grouped_u64(number: u64) -> String {
    let digits = number.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// `1.2 s`, one decimal: how long something took, at the precision a person
/// watching it has.
fn seconds(elapsed: Duration) -> String {
    format!("{:.1} s", elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{filled, key};
    use crate::db::model::QueryEvent::{Columns, Done, Error, Rows, RowsAffected};

    fn columns(names: &[(&str, &str)]) -> QueryEvent {
        Columns(
            names
                .iter()
                .map(|(name, type_name)| Column {
                    name: (*name).to_owned(),
                    type_name: (*type_name).to_owned(),
                })
                .collect(),
        )
    }

    fn started() -> Results {
        let mut results = Results::default();
        results.start(Instant::now(), 0, 1, false);
        results
    }

    fn done(rows: usize, truncated: bool, total_ms: u32) -> QueryEvent {
        Done {
            rows,
            truncated,
            connect_ms: 0,
            first_row_ms: 1,
            total_ms,
        }
    }

    #[test]
    fn batches_add_up_and_only_the_new_rows_are_measured() {
        let mut results = started();
        results.apply(columns(&[("id", "int"), ("name", "nvarchar(100)")]));
        assert_eq!(
            results.widths(),
            [3, 13],
            "a header is a column's minimum, type row included"
        );

        results.apply(Rows(vec![vec![Cell::Int(1), Cell::Text("Zoë".to_owned())]]));
        assert_eq!(results.widths(), [3, 13]);
        results.apply(Rows(vec![vec![
            Cell::Int(1_000_000),
            Cell::Text("a".repeat(80)),
        ]]));
        assert_eq!(results.widths(), [7, WIDTH_CAP], "capped at forty");
        assert_eq!(results.rows().len(), 2, "both batches are kept");
        assert_eq!(
            results.align(),
            [Align::Right, Align::Left],
            "numbers to the right, text to the left"
        );

        assert!(results.running());
        assert_eq!(results.title(), "Running · 0.0 s · 2 rows");
        results.apply(done(2, false, 42));
        assert!(!results.running());
        assert_eq!(results.title(), "Results · 2 rows · 42 ms");
    }

    #[test]
    fn a_cap_that_stopped_the_scan_says_so_and_offers_more() {
        let mut results = started();
        results.apply(columns(&[("id", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(1)]]));
        results.apply(done(10_000, true, 1_234));
        assert!(results.truncated());
        assert_eq!(
            results.title(),
            "Results · 10,000 rows (truncated) · 1,234 ms"
        );

        // `m` re-runs the same statement, which keeps the cursor where it is.
        results.key(key("G"));
        let selected = results.selected();
        results.start(Instant::now(), 0, 1, true);
        assert_eq!(results.selected(), selected, "more rows keeps the view");
        assert!(results.rows().is_empty(), "the re-run starts from nothing");
    }

    #[test]
    fn a_cancel_keeps_the_rows_it_got_and_says_how_long_it_ran() {
        let mut results = Results::default();
        results.start(Instant::now() - Duration::from_millis(1_200), 0, 1, false);
        results.apply(columns(&[("id", "int")]));
        results.apply(Rows((0..4_500).map(|n| vec![Cell::Int(n)]).collect()));
        results.apply(Error(DbError::Cancelled));
        assert_eq!(
            results.title(),
            "Results · cancelled after 1.2 s, 4,500 rows"
        );
        assert_eq!(results.rows().len(), 4_500, "the rows that did arrive stay");
        assert!(
            results.failure().is_none(),
            "a cancel is not a message to show instead of the rows"
        );
    }

    #[test]
    fn a_failure_is_the_drivers_own_message_and_the_statement_it_was_on() {
        let mut results = Results::default();
        results.expect(vec![0..1, 2..3]);
        results.start(Instant::now(), 1, 2, false);
        results.apply(Error(DbError::Query {
            message: "Invalid object name 'bench.nope'.".to_owned(),
            line: Some(3),
        }));
        assert_eq!(
            results.failure().map(ToString::to_string),
            Some("line 3: Invalid object name 'bench.nope'.".to_owned())
        );
        assert_eq!(results.statement_lines(), Some(2..3), "the one that failed");
        assert_eq!(results.progress(), (2, 2), "the second of two");
        assert_eq!(results.title(), "Results · failed");
    }

    #[test]
    fn several_result_sets_show_the_last_and_switch_with_the_brackets() {
        let mut results = started();
        results.apply(columns(&[("first", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(1)]]));
        results.apply(columns(&[("second", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(2)], vec![Cell::Int(3)]]));
        results.apply(done(3, false, 8));
        assert_eq!(results.sets(), 2);
        assert_eq!(results.shown(), 2, "the last set is the one on screen");
        assert_eq!(results.columns()[0].name, "second");
        assert_eq!(results.title(), "Results · set 2/2 · 3 rows · 8 ms");

        results.key(key("["));
        assert_eq!(results.columns()[0].name, "first");
        results.key(key("]"));
        assert_eq!(results.columns()[0].name, "second");
        results.key(key("]"));
        assert_eq!(results.columns()[0].name, "first", "`]` wraps round");
    }

    #[test]
    fn a_run_of_several_statements_counts_what_they_did() {
        let mut results = Results::default();
        results.start(Instant::now(), 0, 3, false);
        results.apply(RowsAffected(1));
        results.apply(done(0, false, 2));
        assert_eq!(results.title(), "Results · 1 rows affected · 2 ms");

        results.start(Instant::now(), 1, 3, false);
        results.apply(columns(&[("id", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(1)]]));
        results.apply(done(1, false, 3));
        results.start(Instant::now(), 2, 3, false);
        results.apply(columns(&[("id", "int")]));
        results.apply(done(0, false, 1));
        assert_eq!(
            results.summary().as_deref(),
            Some("3 statements, 2 result sets, 1 rows affected")
        );
        assert_eq!(
            started().summary(),
            None,
            "one statement is not a run to summarise"
        );
    }

    #[test]
    fn the_window_follows_the_cursor_and_never_runs_off_the_end() {
        let mut results = filled(1_000, 4);
        assert_eq!(results.window(20, 80), (0, 0));

        results.key(key("G"));
        assert_eq!(results.selected().0, 999);
        assert_eq!(
            results.window(20, 80),
            (980, 0),
            "the last page is a full one"
        );
        results.key(key("g"));
        assert_eq!(results.window(20, 80), (0, 0));

        for _ in 0..25 {
            results.key(key("j"));
        }
        let (top, _) = results.window(20, 80);
        assert!(
            (6..=25).contains(&top),
            "the cursor is on the window: {top}"
        );

        // The columns scroll the same way: `$` is the last one, and a window
        // too narrow for them all starts far enough right to show it.
        results.key(key("$"));
        assert_eq!(results.selected().1, 3);
        let (_, left) = results.window(20, 20);
        assert!(left > 0, "a narrow window scrolls sideways: {left}");
        results.key(key("0"));
        assert_eq!(results.window(20, 20).1, 0);
    }

    #[test]
    fn a_cell_reads_the_way_a_grid_can_draw_it() {
        assert_eq!(shown(&Cell::Null), "NULL");
        assert_eq!(shown(&Cell::Int(-12)), "-12");
        assert_eq!(
            shown(&Cell::Text("two\nlines".to_owned())),
            "two⏎lines",
            "a newline is one glyph wide"
        );
        assert_eq!(
            shown(&Cell::Bytes((0..32).collect::<Vec<u8>>())),
            "0x000102030405060708090a0b0c0d0e0f…",
            "the first sixteen bytes and a mark that there were more"
        );
        assert_eq!(shown(&Cell::Bytes(vec![0xff])), "0xff");
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abcd", 4), "abcd");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }
}
