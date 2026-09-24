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
use std::cmp::Ordering;
use std::ops::{Range, RangeInclusive};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::chord;
use crate::db::catalog::ColumnInfo;
use crate::db::model::{Cell, Column, DbError, QueryEvent};
use crate::export::{tsv_row, width as width_of};

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
    /// `y` or Ctrl-C: the range, or the cell under the cursor.
    CopyCell,
    /// `Y`: the rows the range spans, whole, under their column names.
    CopyRow,
    /// `e`: ask where to write the result set.
    Export,
    /// `o`: sort by the selected column.
    Sort,
    /// `/`: start typing a filter over the rows.
    Filter,
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
    /// The column the rows are sorted by, and whether downwards. [`None`]
    /// is the order they arrived in.
    pub sort: Option<(usize, bool)>,
    /// Which arrival each row was: `rows[i]` came `order[i]`th. Empty while
    /// they are in arrival order, which is the order `o` goes back to.
    order: Vec<usize>,
    /// How many rows at the end the filter hides. They are kept rather than
    /// dropped, so clearing the filter costs a sort and not a query.
    hidden: usize,
}

impl Set {
    /// The rows in the order `sort` says, with those `filter` hides moved to
    /// the end. Ties go by arrival, so the sort is stable and sorting back to
    /// [`None`] is exactly the order they came.
    fn arrange(&mut self, filter: &str) {
        let count = self.rows.len();
        if self.order.is_empty() {
            self.order = (0..count).collect();
        }
        let mut by: Vec<usize> = (0..count).collect();
        match self.sort {
            None => by.sort_unstable_by_key(|&at| self.order[at]),
            Some((column, descending)) => {
                let keys: Vec<Key> = self.rows.iter().map(|row| key(row.get(column))).collect();
                by.sort_unstable_by(|&a, &b| {
                    compare(&keys[a], &keys[b], descending).then(self.order[a].cmp(&self.order[b]))
                });
            }
        }
        self.hidden = 0;
        if !filter.is_empty() {
            let wanted = filter.to_lowercase();
            let hides: Vec<bool> = self.rows.iter().map(|row| !matches(row, &wanted)).collect();
            by.sort_by_key(|&at| hides[at]);
            self.hidden = hides.iter().filter(|hides| **hides).count();
        }
        let order = by.iter().map(|&at| self.order[at]).collect();
        self.order = if self.sort.is_some() || self.hidden > 0 {
            order
        } else {
            Vec::new()
        };
        let mut rows: Vec<Option<Vec<Cell>>> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(Some)
            .collect();
        self.rows = by.iter().filter_map(|&at| rows[at].take()).collect();
    }
}

/// Whether any cell of `row` reads as something with `wanted` (lower case)
/// in it, the way the grid draws it: `null` finds a NULL.
///
/// ponytail: every cell is lowered on every key typed, a copy of each value.
/// Fine to the fetch cap; a LOB-heavy set that makes typing lag wants a
/// case-insensitive search that does not copy.
fn matches(row: &[Cell], wanted: &str) -> bool {
    row.iter()
        .any(|cell| shown(cell).to_lowercase().contains(wanted))
}

/// What a cell sorts by. Oracle's `NUMBER` and SQL Server's numeric and
/// money arrive as [`Cell::Decimal`] text, and are numbers here.
enum Key<'a> {
    /// ponytail: an `i64` past 2^53 rounds, so two ids that far out can tie
    /// and keep their arrival order. An exact integer variant if one shows.
    Number(f64),
    /// Text, dates and bytes, as bytes. A date is ISO-shaped text, so that
    /// is its order too — though not across UTC offsets — and bytes order
    /// the way their hex does.
    Text(&'a [u8]),
    Null,
}

fn key(cell: Option<&Cell>) -> Key<'_> {
    match cell {
        None | Some(Cell::Null) => Key::Null,
        #[allow(clippy::cast_precision_loss)]
        Some(Cell::Int(value)) => Key::Number(*value as f64),
        Some(Cell::Float(value)) => Key::Number(*value),
        Some(Cell::Decimal(text)) => text.parse().map_or(Key::Text(text.as_bytes()), Key::Number),
        Some(Cell::Bool(value)) => Key::Text(if *value { b"true" } else { b"false" }),
        Some(Cell::Text(text) | Cell::DateTime(text)) => Key::Text(text.as_bytes()),
        Some(Cell::Bytes(bytes)) => Key::Text(bytes),
    }
}

/// NULL last whichever way, numbers before text in a column that has both.
fn compare(a: &Key, b: &Key, descending: bool) -> Ordering {
    let ordering = match (a, b) {
        (Key::Null, Key::Null) => return Ordering::Equal,
        (Key::Null, _) => return Ordering::Greater,
        (_, Key::Null) => return Ordering::Less,
        (Key::Number(a), Key::Number(b)) => a.total_cmp(b),
        (Key::Text(a), Key::Text(b)) => a.cmp(b),
        (Key::Number(_), Key::Text(_)) => Ordering::Less,
        (Key::Text(_), Key::Number(_)) => Ordering::Greater,
    };
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
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
    /// The other corner of the range selected, the cursor being this one.
    anchor: Option<(usize, usize)>,
    /// `v` is on: every move extends the range, rather than only a shifted
    /// arrow.
    visual: bool,
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
    /// What `/` narrowed the set on screen to, case-insensitively.
    filter: String,
    /// Whether `/` is still being typed into.
    filtering: bool,
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
        // Every statement of a run keeps its sets for `[` and `]`; the first
        // one, and `m`, start from none.
        if statement == 0 {
            self.sets.clear();
            self.shown = 0;
        }
        self.rows_affected = None;
        self.source = None;
        self.label = None;
        self.unfilter();
        self.clear_selection();
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
                    widths,
                    align,
                    ..Set::default()
                });
                // The newest set is the one on screen: a statement's last
                // result is what a person asked the statement for.
                self.shown = self.sets.len() - 1;
                self.clear_selection();
                // A cursor left on the last set's twelfth column would be on
                // no cell of this one. The first set keeps it for `m`.
                if self.sets.len() > 1 {
                    self.selected = (0, 0);
                    self.scroll = (0, 0);
                }
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
                    *width = (*width).max(measured(cell)).min(WIDTH_CAP);
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
    ///
    /// A move extends the range when it is a shifted arrow or `v` is on, and
    /// any other move drops it: the range is the rectangle between where it
    /// started and the cursor.
    pub fn key(&mut self, key: KeyEvent) -> Hit {
        if self.filtering {
            return self.filter_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl-E is not `e`: past Ctrl-C, Ctrl-D and Ctrl-U a chord is no
        // key of the grid's, or of the source view's.
        if chord(key)
            && matches!(key.code, KeyCode::Char(letter) if !(control && "cCdDuU".contains(letter)))
        {
            return Hit::Ignored;
        }
        if self.source.is_some() {
            return self.source_key(key);
        }
        let arrow = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
        );
        let moves = arrow
            || matches!(
                key.code,
                KeyCode::Char('j' | 'k' | 'h' | 'l' | 'g' | 'G' | '0' | '$')
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
            || (control && matches!(key.code, KeyCode::Char('d' | 'D' | 'u' | 'U')));
        if moves {
            if self.visual || (arrow && key.modifiers.contains(KeyModifiers::SHIFT)) {
                self.anchor.get_or_insert(self.selected);
            } else {
                self.anchor = None;
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let page = PAGE as isize;
        match key.code {
            KeyCode::Char('c' | 'C') if control => Hit::CopyCell,
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
            KeyCode::Char('o') => Hit::Sort,
            KeyCode::Char('/') => Hit::Filter,
            // A filter Enter committed is still a filter, and Esc is the way
            // out of one whether or not it is being typed into.
            KeyCode::Esc if !self.filter.is_empty() => {
                self.unfilter();
                Hit::Moved
            }
            KeyCode::Char('v') => {
                self.visual = !self.visual;
                self.anchor = self.visual.then_some(self.selected);
                Hit::Moved
            }
            _ => Hit::Ignored,
        }
    }

    /// `/`: start typing a filter over the rows of the set on screen.
    pub fn search(&mut self) {
        self.filtering = true;
    }

    /// A paste while the filter is being typed into: one more piece of it.
    pub fn paste_filter(&mut self, text: &str) {
        self.filter.push_str(text);
        self.refilter();
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub const fn filtering(&self) -> bool {
        self.filtering
    }

    /// The keys `/` takes for itself while it is being typed into.
    fn filter_key(&mut self, key: KeyEvent) -> Hit {
        match key.code {
            KeyCode::Char('u' | 'U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Char(_) if chord(key) => return Hit::Ignored,
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.refilter();
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.filtering = false;
                }
                self.refilter();
            }
            // Esc clears it; Enter keeps it and gives the keys back.
            KeyCode::Esc => {
                self.unfilter();
            }
            KeyCode::Enter => self.filtering = false,
            _ => return Hit::Ignored,
        }
        Hit::Moved
    }

    /// Drop the filter and show every row again. Whether there was one, so
    /// Esc knows it had something to do.
    pub fn unfilter(&mut self) -> bool {
        self.filtering = false;
        if self.filter.is_empty() {
            return false;
        }
        self.filter.clear();
        self.refilter();
        true
    }

    /// The set on screen narrowed to the filter anew, the cursor on its
    /// first row: the row it was on may be hidden now.
    fn refilter(&mut self) {
        self.clear_selection();
        self.selected.0 = 0;
        self.scroll.0 = 0;
        if let Some(set) = self.sets.get_mut(self.shown) {
            set.arrange(&self.filter);
        }
    }

    /// The range drops, and `v` with it. Whether there was one, so Esc
    /// knows it had something to do.
    pub fn clear_selection(&mut self) -> bool {
        self.visual = false;
        self.anchor.take().is_some()
    }

    /// The rows and the columns of the range, clamped to the set on screen.
    /// [`None`] without one.
    #[must_use]
    pub fn selection(&self) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let (row, column) = self.anchor?;
        let last_row = self.rows().len().checked_sub(1)?;
        let last_column = self.columns().len().checked_sub(1)?;
        let (to_row, to_column) = self.selected;
        let rows = row.min(to_row).min(last_row)..=row.max(to_row).min(last_row);
        let columns =
            column.min(to_column).min(last_column)..=column.max(to_column).min(last_column);
        Some((rows, columns))
    }

    /// Whether (`row`, `column`) is in the range.
    #[must_use]
    pub fn in_selection(&self, row: usize, column: usize) -> bool {
        self.selection()
            .is_some_and(|(rows, columns)| rows.contains(&row) && columns.contains(&column))
    }

    /// The range as tab-separated lines, quoted where a value would split a
    /// cell, and how many cells that is. [`None`] for no range or a range of
    /// one cell, which `y` copies as it is.
    #[must_use]
    pub fn selection_text(&self) -> Option<(String, usize)> {
        let (rows, columns) = self.selection()?;
        let count = rows.clone().count() * columns.clone().count();
        if count < 2 {
            return None;
        }
        let mut text = String::new();
        for row in &self.rows()[rows] {
            tsv_row(
                &mut text,
                columns
                    .clone()
                    .map(|column| row.get(column).map_or(Cow::Borrowed(""), Cell::display)),
            );
        }
        Some((text, count))
    }

    /// Every column of the rows the range spans — or of the cursor's row —
    /// under a line of the column names, and how many rows that is.
    #[must_use]
    pub fn rows_text(&self) -> Option<(String, usize)> {
        let rows = match self.selection() {
            Some((rows, _)) => rows,
            None if self.selected.0 < self.rows().len() => self.selected.0..=self.selected.0,
            None => return None,
        };
        let mut text = String::new();
        tsv_row(
            &mut text,
            self.columns().iter().map(|column| column.name.as_str()),
        );
        let width = self.columns().len();
        let count = rows.clone().count();
        for row in &self.rows()[rows] {
            tsv_row(
                &mut text,
                (0..width).map(|column| row.get(column).map_or(Cow::Borrowed(""), Cell::display)),
            );
        }
        Some((text, count))
    }

    /// Press on a cell at `from` and drag to `to`, in a window drawn from
    /// `(top, left)`: the range between them, the cursor at `to`.
    pub fn drag(&mut self, from: (usize, usize), to: (usize, usize), window: (usize, usize)) {
        if self.click(to.0, to.1, window) {
            self.anchor = Some(from);
        }
    }

    /// Shift-click: the range from the cursor, or from where one already
    /// started, to the cell clicked.
    pub fn extend(&mut self, row: usize, column: usize, window: (usize, usize)) {
        let from = self.anchor.unwrap_or(self.selected);
        self.drag(from, (row, column), window);
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

    /// A click on cell (`row`, `column`) of a window drawn from `(top,
    /// left)`. The window is set to what was drawn rather than worked out by
    /// `at_row`, whose page rule would move a view whose bottom row was
    /// clicked, or from a column hint that `h` and `l` leave behind the
    /// view. Whether there was a cell there.
    ///
    /// ponytail: the first `j` after a click ten or more rows below `top`
    /// still moves the view once, by `at_row`'s page rule. A real page
    /// height in the app would end that.
    pub fn click(&mut self, row: usize, column: usize, (top, left): (usize, usize)) -> bool {
        if row >= self.rows().len() || column >= self.columns().len() {
            return false;
        }
        self.clear_selection();
        self.selected = (row, column);
        self.scroll = (top, left);
        true
    }

    /// A click on a column's header, drawn with `left` at the left edge:
    /// the column is selected and the view stays where it is. The click
    /// then presses `o`.
    pub fn click_header(&mut self, column: usize, left: usize) {
        if column < self.columns().len() {
            self.selected.1 = column;
            self.scroll.1 = left;
        }
    }

    /// The wheel over a window `height` rows high drawn from `top`: the
    /// window moves by `by` and the cursor comes along only as far as it
    /// has to, to stay on it.
    ///
    /// ponytail: the cursor is pulled along because the window is a hint
    /// clamped round it; scrolling it off the screen needs the pane's height
    /// in the app.
    pub fn wheel(&mut self, top: usize, by: isize, height: usize) {
        let count = self.rows().len();
        let top = top
            .saturating_add_signed(by)
            .min(count.saturating_sub(height));
        self.scroll.0 = top;
        self.selected.0 = self
            .selected
            .0
            .clamp(top, top + height.max(1) - 1)
            .min(count.saturating_sub(1));
    }

    /// The sideways wheel over columns drawn from `left`: one column a
    /// notch.
    ///
    /// ponytail: going left, the selection moves to the new left edge even
    /// when it would still have fitted, because how many columns fit is the
    /// renderer's to know. The pane's width in the app would let it stay.
    pub fn wheel_columns(&mut self, left: usize, by: isize) {
        let left = left
            .saturating_add_signed(by)
            .min(self.columns().len().saturating_sub(1));
        self.scroll.1 = left;
        self.selected.1 = if by < 0 {
            self.selected.1.min(left)
        } else {
            self.selected.1.max(left)
        };
    }

    /// The wheel over an object's source drawn from `top`, `height` lines
    /// high.
    pub fn wheel_source(&mut self, top: usize, by: isize, height: usize) {
        if let Some(source) = self.source.as_mut() {
            source.scroll = top
                .saturating_add_signed(by)
                .min(source.lines.len().saturating_sub(height));
        }
    }

    /// The line the source view was drawn from, which is where its keys
    /// move from next.
    pub fn source_from(&mut self, top: usize) {
        if let Some(source) = self.source.as_mut() {
            source.scroll = top;
        }
    }

    /// `o`: the selected column ascending, then descending, then the order
    /// the rows came in. The cursor keeps its row number, not its row. How
    /// many rows were put in order.
    pub fn sort(&mut self) -> usize {
        self.clear_selection();
        let column = self.selected.1;
        let Some(set) = self.sets.get_mut(self.shown) else {
            return 0;
        };
        if column >= set.columns.len() {
            return 0;
        }
        set.sort = match set.sort {
            Some((by, false)) if by == column => Some((column, true)),
            Some((by, true)) if by == column => None,
            _ => Some((column, false)),
        };
        set.arrange(&self.filter);
        set.rows.len()
    }

    /// `[` and `]`, wrapping round the way Ctrl-T wraps round the tabs.
    fn switch_set(&mut self, delta: isize) -> Hit {
        if self.sets.len() < 2 {
            return Hit::Moved;
        }
        self.unfilter();
        let count = self.sets.len();
        self.shown = (self.shown + count).saturating_add_signed(delta) % count;
        self.clear_selection();
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

    /// The rows the filter lets through, which is every row without one.
    #[must_use]
    pub fn rows(&self) -> &[Vec<Cell>] {
        self.set()
            .map_or(&[], |set| &set.rows[..set.rows.len() - set.hidden])
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

    /// `3 statements, 2 result sets, 1 row affected`, for a run of more
    /// than one statement.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        (self.of > 1).then(|| {
            format!(
                "{}, {}, {} affected",
                counted(self.ran as u64, "statement"),
                counted(self.sets_total as u64, "result set"),
                counted(self.affected_total, "row")
            )
        })
    }

    /// The pane's title, which is where every number about the last run is.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(source) = &self.source {
            return format!("Source · {} · {} lines", source.title, source.lines.len());
        }
        let mut title = self.run_title();
        if self.filtering || !self.filter.is_empty() {
            title = format!("{title} · /{}", self.filter);
        }
        match self.selection() {
            Some((rows, columns)) => format!(
                "{title} · {}×{} selected",
                grouped(rows.count()),
                grouped(columns.count())
            ),
            None => title,
        }
    }

    fn run_title(&self) -> String {
        let set = if self.sets.len() > 1 {
            format!(" · set {}/{}", self.shown(), self.sets.len())
        } else {
            String::new()
        };
        match &self.status {
            Status::Idle => "Results".to_owned(),
            Status::Running { since, rows_so_far } => format!(
                "Running · {} · {}",
                seconds(since.elapsed()),
                counted(*rows_so_far as u64, "row")
            ),
            Status::Done {
                rows,
                truncated,
                elapsed,
            } => {
                // With more than one set the title counts the one on screen,
                // and only the last can have been cut short.
                let (rows, truncated) = match self.set() {
                    Some(set) if self.sets.len() > 1 => (
                        set.rows.len(),
                        *truncated && self.shown + 1 == self.sets.len(),
                    ),
                    _ => (*rows, *truncated),
                };
                let what = match (self.rows_affected, rows) {
                    (Some(affected), 0) => format!("{} affected", counted(affected, "row")),
                    _ => format!(
                        "{}{}",
                        self.of(rows),
                        if truncated { " (truncated)" } else { "" }
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
                "Results{set} · cancelled after {}, {}",
                seconds(*elapsed),
                self.of(*rows)
            ),
            Status::Failed { .. } => "Results · failed".to_owned(),
        }
    }

    /// `100 rows`, or `3 of 100 rows` while a filter hides some.
    fn of(&self, rows: usize) -> String {
        let all = counted(rows as u64, "row");
        if self.filter.is_empty() {
            all
        } else {
            format!("{} of {all}", grouped(self.rows().len()))
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
            reset: false,
            connect_ms: 0,
            first_row_ms: 0,
            total_ms: 0,
        });
        self.label = Some(label);
    }

    /// `s`: the text that made the object, read only.
    pub fn show_source(&mut self, title: String, text: &str) {
        self.unfilter();
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

    /// The whole of the source, the way it came, for `y`.
    #[must_use]
    pub fn source_text(&self) -> Option<String> {
        self.source.as_ref().map(|source| source.lines.join("\n"))
    }

    /// The source view's keys: the grid's movement keys, over lines, and
    /// `y` or `Y` for all of it — a line of a definition is rarely wanted.
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
            KeyCode::Char('c' | 'C') if control => return Hit::CopyCell,
            KeyCode::Char('y') => return Hit::CopyCell,
            KeyCode::Char('Y') => return Hit::CopyRow,
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

/// A header is as wide as the longer of its two lines, because the type row
/// is drawn under the name in the same column.
fn header_width(column: &Column) -> usize {
    width_of(&column.name).max(width_of(&column.type_name))
}

/// What a cell reads as in the grid. It differs from [`Cell::display`] in
/// the two ways a grid cares about: a NULL says so, and a blob is its first
/// bytes rather than all of them. A newline and the other control characters
/// are [`cut`]'s to draw, so a long text is never copied to be shown.
#[must_use]
pub fn shown(cell: &Cell) -> Cow<'_, str> {
    match cell {
        Cell::Null => Cow::Borrowed("NULL"),
        Cell::Bytes(bytes) => Cow::Owned(short_hex(bytes)),
        other => other.display(),
    }
}

/// How wide [`shown`] draws `cell`. A number is counted rather than
/// formatted, because every cell of every batch is measured.
fn measured(cell: &Cell) -> usize {
    match cell {
        Cell::Int(value) => {
            let digits = value
                .unsigned_abs()
                .checked_ilog10()
                .map_or(1, |log| log as usize + 1);
            digits + usize::from(*value < 0)
        }
        other => width_of(&shown(other)),
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

/// How many lines the whole of one cell comes to: text wrapped at
/// [`INSPECT_WIDTH`] with its own line breaks kept, bytes as a hex dump,
/// and NULL as the one word the grid shows.
///
/// Counted rather than built, because this is what a scroll key clamps
/// against and what the overlay is sized by — and the 1 MiB a LOB stops at
/// would otherwise be 1 MiB formatted per keystroke.
#[must_use]
pub fn inspect_height(cell: &Cell) -> usize {
    match cell {
        Cell::Null => 1,
        Cell::Bytes(bytes) => bytes.len().div_ceil(HEX_PER_LINE),
        other => wrapped_height(&other.display()),
    }
}

/// The `count` lines of that value from `top`, and no others. A value is as
/// long as a LOB is allowed to be and an overlay is forty lines tall, so
/// what a frame costs is the overlay and never the value.
#[must_use]
pub fn inspect_lines(cell: &Cell, top: usize, count: usize) -> Vec<String> {
    match cell {
        Cell::Null => vec!["NULL".to_owned()],
        Cell::Bytes(bytes) => bytes
            .chunks(HEX_PER_LINE)
            .enumerate()
            .skip(top)
            .take(count)
            .map(|(line, chunk)| hex_line(line * HEX_PER_LINE, chunk))
            .collect(),
        other => wrapped(&other.display(), top, count),
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

/// One paragraph of the text, its trailing carriage return dropped, as the
/// overlay wraps it: the [`INSPECT_WIDTH`] characters of each line.
fn paragraphs(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|paragraph| paragraph.strip_suffix('\r').unwrap_or(paragraph))
}

/// How many lines the wrapped text comes to. A paragraph exactly as wide as
/// the overlay is one line and not one line and an empty one, and an empty
/// paragraph is still a line.
fn wrapped_height(text: &str) -> usize {
    paragraphs(text)
        .map(|paragraph| paragraph.chars().count().div_ceil(INSPECT_WIDTH).max(1))
        .sum()
}

/// The `count` wrapped lines from `top`, cut where the text is too long and
/// broken where it breaks itself. Only those lines are built: the rest of
/// the value is counted past, not formatted.
fn wrapped(text: &str, top: usize, count: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(count.min(64));
    let mut line = 0;
    for paragraph in paragraphs(text) {
        let height = paragraph.chars().count().div_ceil(INSPECT_WIDTH).max(1);
        if line + height > top {
            let skip = top.saturating_sub(line);
            let mut characters = paragraph.chars().skip(skip * INSPECT_WIDTH);
            for _ in skip..height {
                if lines.len() == count {
                    return lines;
                }
                lines.push(
                    characters
                        .by_ref()
                        .take(INSPECT_WIDTH)
                        .map(crate::export::printable)
                        .collect(),
                );
            }
        }
        line += height;
    }
    lines
}

/// A cell cut to `width` terminal columns, the last of which says there was
/// more.
#[must_use]
pub fn cut(text: &str, width: usize) -> Cow<'_, str> {
    crate::export::cut_to(text, width)
}

/// `1,234`: a row count is read, not computed with.
#[must_use]
pub fn grouped(number: usize) -> String {
    grouped_u64(u64::try_from(number).unwrap_or(u64::MAX))
}

/// `1 row`, `2 rows`, `10,000 rows`.
#[must_use]
pub fn counted(number: u64, noun: &str) -> String {
    let plural = if number == 1 { "" } else { "s" };
    format!("{} {noun}{plural}", grouped_u64(number))
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
            reset: false,
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
        assert_eq!(
            results.title(),
            "Results · set 2/2 · 2 rows · 8 ms",
            "the rows of the set on screen"
        );

        results.key(key("["));
        assert_eq!(results.columns()[0].name, "first");
        results.key(key("]"));
        assert_eq!(results.columns()[0].name, "second");
        results.key(key("]"));
        assert_eq!(results.columns()[0].name, "first", "`]` wraps round");
    }

    #[test]
    fn a_second_set_of_one_statement_puts_the_cursor_on_its_first_cell() {
        let mut results = started();
        results.apply(columns(&[("a", "int"), ("b", "int"), ("c", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(1), Cell::Int(2), Cell::Int(3)]]));
        results.key(key("$"));
        assert_eq!(results.selected(), (0, 2));
        results.apply(columns(&[("only", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(4)]]));
        assert_eq!(results.selected(), (0, 0));
        assert_eq!(results.cell(), Some(&Cell::Int(4)), "a cell Enter can open");
    }

    #[test]
    fn a_run_of_several_statements_counts_what_they_did() {
        let mut results = Results::default();
        results.start(Instant::now(), 0, 3, false);
        results.apply(RowsAffected(1));
        results.apply(done(0, false, 2));
        assert_eq!(results.title(), "Results · 1 row affected · 2 ms");

        results.start(Instant::now(), 1, 3, false);
        results.apply(columns(&[("id", "int")]));
        results.apply(Rows(vec![vec![Cell::Int(1)]]));
        results.apply(done(1, false, 3));
        results.start(Instant::now(), 2, 3, false);
        results.apply(columns(&[("id", "int")]));
        results.apply(done(0, false, 1));
        assert_eq!(
            results.summary().as_deref(),
            Some("3 statements, 2 result sets, 1 row affected")
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
    fn shift_arrows_select_a_rectangle_and_a_plain_move_drops_it() {
        let mut results = filled(10, 5);
        results.key(key("j"));
        results.key(key("l"));
        assert_eq!(results.selection(), None);
        for spec in ["Shift-Down", "Shift-Down", "Shift-Right"] {
            results.key(key(spec));
        }
        assert_eq!(results.selection(), Some((1..=3, 1..=2)));
        assert_eq!(results.title(), "Results · 10 rows · 42 ms · 3×2 selected");
        // Back past where it started turns the rectangle round the anchor.
        for spec in [
            "Shift-Up",
            "Shift-Up",
            "Shift-Up",
            "Shift-Left",
            "Shift-Left",
        ] {
            results.key(key(spec));
        }
        assert_eq!(results.selection(), Some((0..=1, 0..=1)));
        results.key(key("Down"));
        assert_eq!(results.selection(), None, "an unshifted arrow");
        results.key(key("Shift-Down"));
        results.key(key("j"));
        assert_eq!(results.selection(), None, "and `j` the same");
    }

    #[test]
    fn v_makes_every_move_extend_until_it_is_pressed_again() {
        let mut results = filled(10, 5);
        results.key(key("j"));
        results.key(key("l"));
        results.key(key("v"));
        assert_eq!(
            results.selection(),
            Some((1..=1, 1..=1)),
            "the cursor alone"
        );
        results.key(key("G"));
        assert_eq!(
            results.selection(),
            Some((1..=9, 1..=1)),
            "`v G`: the column down"
        );
        results.key(key("v"));
        assert_eq!(results.selection(), None, "`v` again ends it");

        results.key(key("v"));
        results.key(key("$"));
        assert_eq!(
            results.selection(),
            Some((9..=9, 1..=4)),
            "`v $`: the row on"
        );
        results.key(key("k"));
        results.key(key("h"));
        assert_eq!(results.selection(), Some((8..=9, 1..=3)), "hjkl extend too");
        assert!(results.clear_selection());
        assert!(!results.clear_selection(), "nothing left to clear");
        results.key(key("j"));
        assert_eq!(results.selection(), None, "and `v` went with it");
    }

    #[test]
    fn a_sort_another_set_or_a_new_run_drops_the_range() {
        let mut results = filled(10, 5);
        results.key(key("v"));
        results.key(key("j"));
        assert!(results.selection().is_some());
        results.sort();
        assert_eq!(results.selection(), None, "sorted");

        results.key(key("v"));
        results.key(key("j"));
        results.apply(columns(&[("second", "int")]));
        assert_eq!(results.selection(), None, "a new set on screen");

        let mut results = filled(10, 5);
        results.key(key("v"));
        results.key(key("j"));
        results.start(Instant::now(), 0, 1, true);
        assert_eq!(results.selection(), None, "`m` ran it again");
    }

    #[test]
    fn a_range_copies_as_tab_separated_lines_quoted_where_a_value_would_split() {
        let mut results = started();
        results.apply(columns(&[("a", "int"), ("b", "text"), ("c", "text")]));
        results.apply(Rows(vec![
            vec![
                Cell::Int(1),
                Cell::Text("tab\there".to_owned()),
                Cell::Text("x".to_owned()),
            ],
            vec![
                Cell::Int(2),
                Cell::Null,
                Cell::Text("say \"hi\"".to_owned()),
            ],
        ]));
        results.key(key("Shift-Down"));
        results.key(key("Shift-Right"));
        assert_eq!(
            results.selection_text(),
            Some(("1\t\"tab\there\"\n2\t\n".to_owned(), 4)),
            "a NULL is nothing and a tab inside a value is quoted"
        );
        assert_eq!(
            results.rows_text(),
            Some((
                "a\tb\tc\n1\t\"tab\there\"\tx\n2\t\t\"say \"\"hi\"\"\"\n".to_owned(),
                2
            )),
            "every column of both rows, under the names"
        );
        results.key(key("Up"));
        assert_eq!(results.selection_text(), None, "no range is the cell's own");
        results.key(key("v"));
        assert_eq!(results.selection_text(), None, "and so is a range of one");
    }

    #[test]
    fn a_cell_reads_the_way_a_grid_can_draw_it() {
        assert_eq!(shown(&Cell::Null), "NULL");
        assert_eq!(shown(&Cell::Int(-12)), "-12");
        let text = Cell::Text("two\nlines\tand\x1b".to_owned());
        assert_eq!(
            cut(&shown(&text), 40),
            "two⏎lines and␛",
            "a control character is one glyph wide"
        );
        assert_eq!(width_of(&shown(&text)), 14, "and measured as the glyph");
        assert_eq!(
            shown(&Cell::Bytes((0..32).collect::<Vec<u8>>())),
            "0x000102030405060708090a0b0c0d0e0f…",
            "the first sixteen bytes and a mark that there were more"
        );
        assert_eq!(shown(&Cell::Bytes(vec![0xff])), "0xff");
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abcd", 4), "abcd");
        // T5.4: a column is terminal columns and a wide glyph is two of
        // them, so four of these do not fit in a column four wide.
        assert_eq!(cut("李雷李雷", 4), "李…");
        assert_eq!(cut("李雷", 4), "李雷");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }
}
