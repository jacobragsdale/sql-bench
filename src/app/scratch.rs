//! The scratch pad: the lines of SQL one tab holds, the cursor that edits
//! them, and the splitter that says which statement is under it.
//!
//! Lines in a `Vec<String>` and a cursor in characters, as the decisions log
//! in `docs/DESIGN.md` says: a pad is a screenful of SQL, not a document, and
//! a rope would cost more to read than it ever saved. Nothing here touches
//! the clock or the disk — [`Scratch::settle`] is handed the time by the run
//! loop, which is also what writes the file.

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::config::Kind;
use crate::db::code_start;

/// How long after the last edit the pad is written to disk. It is also what
/// ends an undo burst, so one Ctrl-Z takes back everything typed without a
/// pause in it and never more than that.
pub const SETTLE: Duration = Duration::from_millis(500);

/// What Tab types. A tab character in SQL is a merge conflict waiting to
/// happen, and two spaces are what every statement in `scripts/seed` uses.
const INDENT: &str = "  ";

/// How far PageUp and PageDown go.
// ponytail: a fixed page, because the pane's height is known to the renderer
// and the renderer cannot write to the app; take the height from a resize if
// a page ever has to match the pane.
const PAGE: usize = 10;

/// What a key meant, for [`crate::app::App`] to turn into an [`Action`].
///
/// [`Action`]: crate::app::Action
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The key was not one of the pad's.
    Unchanged,
    /// The pad changed and the screen has to be painted again. The text may
    /// be the same as it was — a move is an `Edited` too — because only the
    /// pad itself has to know the difference, through [`Scratch::settle`].
    Edited,
    /// Ctrl-R: run the statement under the cursor.
    RunStatement,
    /// F5: run every statement in the pad.
    RunAll,
    /// Ctrl-E: hand the pad to `$VISUAL` or `$EDITOR`.
    OpenEditor,
    /// Ctrl-C with a selection: this text, into the clipboard.
    Copy(String),
    /// Ctrl-C with none: the statement under the cursor, which only the app
    /// can find because only it knows the backend.
    CopyStatement,
    /// Ctrl-X: this text, into the clipboard, and already out of the pad.
    Cut(String),
}

/// One tab's pad.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scratch {
    /// Never empty: a pad with nothing in it is one empty line, so the cursor
    /// always has a line to be on.
    lines: Vec<String>,
    /// Line, and the character column within it.
    cursor: (usize, usize),
    /// Where a Shift-arrow selection started; the cursor is its other end.
    selection: Option<(usize, usize)>,
    /// The first line and column the pane last showed, which it goes on
    /// showing from for as long as the cursor is on it. A hint, because only
    /// the renderer knows how tall the pane is: [`Scratch::window`] clamps it.
    scroll: (usize, usize),
    /// The pad as it was when the burst that is being typed started.
    // ponytail: one snapshot, so Ctrl-Z takes back the last burst and no
    // more; an undo stack is the upgrade if anyone asks for a second Ctrl-Z.
    undo: Option<(Vec<String>, (usize, usize))>,
    /// The column an up or down tries to land on, kept across short lines.
    goal: Option<usize>,
    /// Whether a burst is open, which is what says a snapshot has been taken.
    burst: bool,
    /// Edited since the last [`Scratch::settle`], which is how an app that
    /// reads no clock still knows when the last edit was.
    edited: bool,
    /// When the last edit was, as `settle` was told.
    dirty_since: Option<Instant>,
    /// Whether the file on disk is behind the pad.
    modified: bool,
    /// The lines of the statement that failed, painted until the next edit.
    flagged: Option<Range<usize>>,
    /// Counts edits, so a run can tell whether the lines it split are still
    /// where they were when its statement fails.
    revision: u64,
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            selection: None,
            scroll: (0, 0),
            undo: None,
            goal: None,
            burst: false,
            edited: false,
            dirty_since: None,
            modified: false,
            flagged: None,
            revision: 0,
        }
    }
}

impl Scratch {
    /// The pad a file held: saved by definition, cursor at the top.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self {
            lines: split_lines(text),
            ..Self::default()
        }
    }

    /// Text in the pad's lines exactly as it is: no tab turned into spaces
    /// and no control character dropped, because inside a literal they are
    /// data. What the command line runs is split through here; what the pad
    /// shows is not.
    #[must_use]
    pub fn verbatim(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        Self {
            lines: text.split('\n').map(str::to_owned).collect(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub const fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether there is nothing in the pad at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(String::is_empty)
    }

    /// Whether the pad is ahead of the file, which the pane title says.
    #[must_use]
    pub const fn modified(&self) -> bool {
        self.modified
    }

    /// Whatever came back from `$EDITOR`, or any other whole-pad replacement.
    pub fn set_text(&mut self, text: &str) {
        let lines = split_lines(text);
        if lines == self.lines {
            return;
        }
        self.lines = lines;
        self.selection = None;
        self.undo = None;
        self.burst = false;
        self.flagged = None;
        self.revision += 1;
        self.modified = true;
        self.edited = true;
        self.clamp_cursor();
    }

    /// The pad was written to disk as it now is.
    pub fn saved(&mut self) {
        self.modified = false;
    }

    /// Move the save timer on and say whether the pad should be written now.
    ///
    /// The run loop calls this every turn with its own clock, because the app
    /// reads none: an edit only sets a flag, and the first settle after it
    /// turns that flag into a time. A pause of [`SETTLE`] also closes the
    /// undo burst, so what is typed after it is taken back on its own.
    pub fn settle(&mut self, now: Instant) -> bool {
        if self.edited {
            self.edited = false;
            self.dirty_since = Some(now);
            return false;
        }
        let Some(at) = self.dirty_since else {
            return false;
        };
        if now.duration_since(at) < SETTLE {
            return false;
        }
        self.dirty_since = None;
        self.burst = false;
        true
    }

    /// Whether a settle is still owed, which is what keeps the loop turning
    /// while nobody is typing.
    #[must_use]
    pub const fn settling(&self) -> bool {
        self.edited || self.dirty_since.is_some()
    }

    /// The selection as an ordered half-open range of (line, column), or
    /// `None` when there is nothing selected.
    #[must_use]
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.selection?;
        if anchor == self.cursor {
            return None;
        }
        Some(if anchor < self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        })
    }

    /// What Ctrl-C copies.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let ((from_line, from_column), (to_line, to_column)) = self.selection()?;
        if from_line == to_line {
            return Some(slice(&self.lines[from_line], from_column, to_column));
        }
        let mut text = slice(&self.lines[from_line], from_column, usize::MAX);
        for line in &self.lines[from_line + 1..to_line] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&slice(&self.lines[to_line], 0, to_column));
        Some(text)
    }

    /// Where a pane `height` rows by `width` columns starts showing the pad:
    /// where it last did, moved only as far as it takes to put the cursor on
    /// it, and never so far down that rows are left empty under the last
    /// line. The line is a line and the column a terminal column, because a
    /// CJK character is two of them. A cursor the first screen's width has
    /// room for puts the view back at the left edge, or a short line gone to
    /// from the end of a long one would show nothing left of the cursor.
    #[must_use]
    pub fn window(&self, height: usize, width: usize) -> (usize, usize) {
        let (height, width) = (height.max(1), width.max(1));
        let (line, column) = self.cursor;
        let mut characters = self.lines[line].chars();
        // Past the end of the line a column is a blank, one terminal column.
        let (mut at, mut counted) = (0, 0);
        for character in characters.by_ref().take(column) {
            at += char_width(character);
            counted += 1;
        }
        at += column - counted;
        let under = characters
            .next()
            .map_or(1, |character| char_width(character).max(1));
        (
            self.scroll
                .0
                .min(line)
                .max(line.saturating_sub(height - 1))
                .min(self.lines.len().saturating_sub(height)),
            if at + under <= width {
                0
            } else {
                self.scroll
                    .1
                    .min(at)
                    .max((at + under).saturating_sub(width))
            },
        )
    }

    /// The character of `line` drawn over terminal column `cell`, or the end
    /// of the line when it is past it: where a click there puts the cursor.
    #[must_use]
    pub fn column_at(&self, line: usize, cell: usize) -> usize {
        let text = &self.lines[line.min(self.lines.len() - 1)];
        let mut drawn = 0;
        text.chars()
            .position(|character| {
                drawn += char_width(character);
                drawn > cell
            })
            .unwrap_or_else(|| text.chars().count() + cell.saturating_sub(drawn))
    }

    /// Show the pad from this line and column, the way a frame just did or a
    /// click found it, so what the cursor does next moves the view from there.
    pub const fn show_from(&mut self, top: usize, left: usize) {
        self.scroll = (top, left);
    }

    /// A click: the cursor on this line and character, each clamped to the
    /// text, keeping the selection's anchor when `extend`. A move, never an
    /// edit, so it ends no undo burst and owes the disk nothing.
    pub fn place(&mut self, (line, column): (usize, usize), extend: bool) {
        let line = line.min(self.lines.len() - 1);
        self.move_to((line, column.min(self.line_length(line))), extend);
    }

    /// A double-click: the run of letters, digits and underscores the cursor
    /// is on, selected. Anywhere else there is no word, and nothing is.
    /// Esc: the selection drops and the cursor stays. Whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        let had = self.selection().is_some();
        self.selection = None;
        had
    }

    pub fn select_word(&mut self) {
        let (line, column) = self.cursor;
        let characters: Vec<char> = self.lines[line].chars().collect();
        let word = |at: usize| {
            characters
                .get(at)
                .is_some_and(|character| character.is_alphanumeric() || *character == '_')
        };
        if !word(column) {
            return;
        }
        let start = (0..column)
            .rev()
            .take_while(|at| word(*at))
            .last()
            .unwrap_or(column);
        let end = (column..characters.len())
            .find(|at| !word(*at))
            .unwrap_or(characters.len());
        self.move_to((line, start), false);
        self.move_to((line, end), true);
    }

    /// The wheel over a pane `rows` high that showed the pad from `top` and
    /// `left`: `by` lines further, and the cursor pulled along just far
    /// enough to stay on it, the way vim's Ctrl-E does, with the selection
    /// growing if there is one.
    // ponytail: the cursor is pulled along because `window` clamps the view
    // to it; truly free scrolling needs viewport heights in the app.
    pub fn wheel(&mut self, (top, left): (usize, usize), rows: usize, by: isize) {
        let rows = rows.max(1);
        let top = top
            .saturating_add_signed(by)
            .min(self.lines.len().saturating_sub(rows));
        self.scroll = (top, left);
        let line = self.cursor.0.clamp(top, top + rows - 1);
        if line != self.cursor.0 {
            let rows = isize::try_from(line).unwrap_or(isize::MAX)
                - isize::try_from(self.cursor.0).unwrap_or(isize::MAX);
            self.move_rows(rows, self.selection.is_some());
        }
    }

    /// One key. Everything that is not the pad's own is [`Outcome::Unchanged`],
    /// which is how the shell keeps the keys it handles itself.
    pub fn handle(&mut self, key: KeyEvent) -> Outcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A key that deletes takes the selection and nothing more, the way
        // it does in every editor that has one.
        let deletes = matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
            || control && matches!(key.code, KeyCode::Char('u' | 'U' | 'k' | 'K' | 'w' | 'W'));
        if deletes && self.take_selection() {
            return Outcome::Edited;
        }
        match key.code {
            KeyCode::Char('r' | 'R') if control => Outcome::RunStatement,
            KeyCode::F(5) => Outcome::RunAll,
            KeyCode::Char('e' | 'E') if control => Outcome::OpenEditor,
            KeyCode::Char('c' | 'C') if control => self
                .selected_text()
                .map_or(Outcome::CopyStatement, Outcome::Copy),
            KeyCode::Char('x' | 'X') if control => match self.selected_text() {
                Some(text) => {
                    self.take_selection();
                    Outcome::Cut(text)
                }
                None => Outcome::Unchanged,
            },
            KeyCode::Char('z' | 'Z') if control => self.undo(),
            KeyCode::Char('a' | 'A') if control => self.select_all(),
            KeyCode::Char('u' | 'U') if control => self.delete_to_line_start(),
            KeyCode::Char('k' | 'K') if control => self.delete_to_line_end(),
            KeyCode::Char('w' | 'W') if control => self.delete_word_back(),
            KeyCode::Left if control => {
                let to = self.word_left();
                self.move_to(to, shift)
            }
            KeyCode::Right if control => {
                let to = self.word_right();
                self.move_to(to, shift)
            }
            KeyCode::Left => {
                let to = self.left();
                self.move_to(to, shift)
            }
            KeyCode::Right => {
                let to = self.right();
                self.move_to(to, shift)
            }
            KeyCode::Up => self.move_rows(-1, shift),
            KeyCode::Down => self.move_rows(1, shift),
            // The view goes a page with the cursor, as the grid's does.
            #[allow(clippy::cast_possible_wrap)]
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = if key.code == KeyCode::PageUp {
                    -(PAGE as isize)
                } else {
                    PAGE as isize
                };
                self.scroll.0 = self.scroll.0.saturating_add_signed(page);
                self.move_rows(page, shift)
            }
            KeyCode::Home if control => self.move_to((0, 0), shift),
            KeyCode::End if control => {
                let last = self.lines.len() - 1;
                self.move_to((last, self.line_length(last)), shift)
            }
            KeyCode::Home => self.move_to((self.cursor.0, 0), shift),
            KeyCode::End => self.move_to((self.cursor.0, self.line_length(self.cursor.0)), shift),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Enter => {
                self.take_selection();
                self.insert_newline()
            }
            KeyCode::Tab => {
                self.take_selection();
                self.insert(INDENT)
            }
            KeyCode::Char(character) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.take_selection();
                self.insert(&character.to_string())
            }
            _ => Outcome::Unchanged,
        }
    }

    /// Bracketed paste: the whole block at the cursor, or over the
    /// selection, line breaks kept.
    pub fn paste(&mut self, text: &str) -> Outcome {
        let cleaned = cleaned(text);
        if cleaned.is_empty() {
            return Outcome::Unchanged;
        }
        self.take_selection();
        self.insert(&cleaned)
    }

    /// Ctrl-A: the whole pad, the cursor at its end.
    fn select_all(&mut self) -> Outcome {
        let last = self.lines.len() - 1;
        self.selection = Some((0, 0));
        self.cursor = (last, self.line_length(last));
        self.goal = None;
        Outcome::Edited
    }

    /// Delete the selection, if there is one, as the start of an edit: what
    /// is typed next lands in its place, and one Ctrl-Z takes back both.
    fn take_selection(&mut self) -> bool {
        let Some(((from_line, from_column), (to_line, to_column))) = self.selection() else {
            return false;
        };
        self.begin_edit();
        let tail = slice(&self.lines[to_line], to_column, usize::MAX);
        let line = &mut self.lines[from_line];
        line.truncate(byte_index(line, from_column));
        line.push_str(&tail);
        self.lines.drain(from_line + 1..=to_line);
        self.cursor = (from_line, from_column);
        true
    }

    /// The statement the cursor is in, and the lines it is on.
    ///
    /// A cursor on a blank line or a terminator gets the statement before it,
    /// because that is where it was just typed.
    #[must_use]
    pub fn statement_at_cursor(&self, kind: Kind) -> Option<(String, Range<usize>)> {
        let line = self.cursor.0;
        let statements = self.statements(kind);
        statements
            .iter()
            .find(|(_, range)| range.contains(&line))
            .or_else(|| statements.iter().rev().find(|(_, range)| range.end <= line))
            .cloned()
    }

    /// Every statement in the pad, in order, each trimmed and never empty.
    ///
    /// A statement ends at a line ending in `;`, at a `GO` of its own for SQL
    /// Server, at a `/` of its own for Oracle, at a blank line, or at the end
    /// of the pad. `begin` (and on Oracle `declare`, or a `create` of a
    /// procedure, function, package, trigger or type body) opens a block
    /// whose `end` closes it, so the semicolons inside a PL/SQL block do not
    /// split it. On SQL Server a `declare` or a `create` of a procedure,
    /// function or trigger runs on to the next blank line or `GO` instead,
    /// because its variables and its body live as long as the batch. Oracle
    /// runs one statement per call, so outside a block its lines are also cut
    /// at every `;` that is not in a quote or a comment. The words are matched
    /// whatever their case.
    // ponytail: line-based, so a `;` or blank line inside a multi-line string
    // literal still splits, and in PL/SQL a `case` expression's `end;` on a
    // line of its own counts as the block's; a real tokenizer if either bites.
    // Two Oracle statements on one line share its range, so `Ctrl-R` runs
    // the first; a column in the range if that matters.
    #[must_use]
    pub fn statements(&self, kind: Kind) -> Vec<(String, Range<usize>)> {
        // Each statement's lines, and whether it opened a block or a batch.
        let mut spans: Vec<(Range<usize>, bool)> = Vec::new();
        let mut start: Option<usize> = None;
        let mut depth = 0usize;
        let mut block = false;
        let mut batch = false;
        // A T-SQL procedure, function or trigger: its batch is all of it up
        // to `GO`, blank lines and all, the way SQL Server reads one.
        let mut program = false;
        // Oracle subprograms declared inside a block, whose `end;` is theirs.
        let mut nested = 0usize;
        let mut opened = false;
        // A blank line that ends the statement unless the next code line
        // carries it on, and the first note after that blank line.
        let mut pending: Option<usize> = None;
        let mut note: Option<usize> = None;
        let mut flush = |start: &mut Option<usize>, end: usize, opened: bool| {
            if let Some(from) = start.take() {
                spans.push((from..end, opened));
            }
        };
        for (number, line) in self.lines.iter().enumerate() {
            let trimmed = line.trim();
            let lowered = trimmed.to_ascii_lowercase();
            // The words that say what a line starts are after any note in
            // front of them: `/* why */ begin` still opens a block.
            let lower = code_start(&lowered);
            let terminator = match kind {
                Kind::Mssql => lower == "go",
                Kind::Oracle => trimmed == "/",
            };
            if terminator {
                flush(&mut start, pending.unwrap_or(number), opened);
                (depth, block, batch, program, nested) = (0, false, false, false, 0);
                (pending, note) = (None, None);
                continue;
            }
            if trimmed.is_empty() {
                if depth == 0 && !block && !program && start.is_some() {
                    pending.get_or_insert(number);
                }
                continue;
            }
            if let Some(blank) = pending {
                if lower.is_empty() {
                    // Only a note: which statement it belongs to is for
                    // the code after it to say.
                    note.get_or_insert(number);
                    continue;
                }
                if !continues(lower) {
                    flush(&mut start, blank, opened);
                    batch = false;
                    start = Some(note.unwrap_or(number));
                    opened = false;
                }
                (pending, note) = (None, None);
            }
            if start.is_none() {
                start = Some(number);
                opened = false;
            }
            match (kind, program_of(lower, kind)) {
                (Kind::Oracle, Some("package" | "type body")) => {
                    // A package or a type body has an `end` and no `begin`.
                    block = true;
                    depth += 1;
                }
                (Kind::Oracle, Some("procedure" | "function" | "trigger")) => block = true,
                (Kind::Mssql, Some("procedure" | "function" | "trigger")) => program = true,
                _ => {}
            }
            if starts_word(lower, "declare") {
                match kind {
                    Kind::Oracle => block = true,
                    Kind::Mssql => batch = true,
                }
            }
            let code = code_of(lower);
            // A subprogram declared in a block's declarations, `procedure p
            // is`, has a `begin` and an `end;` of its own; a forward one
            // ends in `;` there and then.
            if kind == Kind::Oracle
                && block
                && depth == 0
                && (starts_word(lower, "procedure") || starts_word(lower, "function"))
                && !code.ends_with(';')
            {
                nested += 1;
            }
            // `begin` opens a block at the start of a line, and at the end of
            // one too: `end else begin`, `if @x = 1 begin`.
            let begins = starts_word(lower, "begin") && !begins_transaction(lower);
            if begins || ends_word(code, "begin") {
                block = true;
                depth += 1;
            }
            // A block on one line closes itself: `begin null; end;`.
            let closing = closes_block(code) || begins && ends_word(code, "end");
            if closing {
                depth = depth.saturating_sub(1);
            }
            opened |= block || batch || program;
            if closing && depth == 0 {
                if nested > 0 {
                    // The declared subprogram's own `end;`.
                    nested -= 1;
                    continue;
                }
                // T-SQL's `end` needs no `;`, so the block is over either way
                // and the next blank line may end the statement.
                block &= code.ends_with(';');
            }
            // Inside a block only the `end` that closes it ends the
            // statement, however many semicolons the body has.
            if code.ends_with(';') && depth == 0 && !batch && !program && (!block || closing) {
                flush(&mut start, number + 1, opened);
                block = false;
            }
        }
        flush(&mut start, pending.unwrap_or(self.lines.len()), opened);
        spans
            .into_iter()
            .flat_map(|(range, opened)| {
                let text = self.lines[range.clone()].join("\n").trim().to_owned();
                let pieces = if kind == Kind::Oracle && !opened {
                    split_semicolons(&text)
                } else {
                    vec![text.clone()]
                };
                // Each piece on its own lines: a driver's `line 2` counts
                // from the first of them. A span starts on a line with code,
                // so the trim took no line off the front.
                let mut from = 0;
                pieces
                    .into_iter()
                    .filter(|piece| has_code(piece))
                    .map(move |piece| {
                        let at = text[from..].find(&piece).map_or(from, |at| from + at);
                        from = at + piece.len();
                        let first = range.start + text[..at].matches('\n').count();
                        let lines = first..first + piece.matches('\n').count() + 1;
                        (piece, lines)
                    })
            })
            .collect()
    }

    fn undo(&mut self) -> Outcome {
        let Some((lines, cursor)) = self.undo.take() else {
            return Outcome::Unchanged;
        };
        self.lines = lines;
        self.cursor = cursor;
        self.selection = None;
        self.goal = None;
        self.burst = false;
        self.flagged = None;
        self.revision += 1;
        self.modified = true;
        self.edited = true;
        Outcome::Edited
    }

    /// The lines a failed statement was on, which the pane paints until the
    /// next edit. `None` clears it, which a new run does.
    pub fn flag(&mut self, lines: Option<Range<usize>>) {
        self.flagged = lines;
    }

    #[must_use]
    pub fn flagged(&self) -> Option<&Range<usize>> {
        self.flagged.as_ref()
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The bookkeeping every edit does: the snapshot the burst is taken back
    /// to, and the flags the settle reads.
    fn begin_edit(&mut self) {
        self.flagged = None;
        self.revision += 1;
        if !self.burst {
            self.undo = Some((self.lines.clone(), self.cursor));
            self.burst = true;
        }
        self.selection = None;
        self.goal = None;
        self.modified = true;
        self.edited = true;
    }

    /// `text` at the cursor, line breaks and all, the cursor after it. The
    /// new lines go in with one splice: a line at a time, every line below
    /// moved down again for each, a paste of fifty thousand lines above
    /// others was half a second.
    fn insert(&mut self, text: &str) -> Outcome {
        self.begin_edit();
        let (line, column) = self.cursor;
        let at = byte_index(&self.lines[line], column);
        let tail = self.lines[line].split_off(at);
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or_default();
        self.lines[line].push_str(first);
        let mut added: Vec<String> = parts.map(str::to_owned).collect();
        match added.last_mut() {
            None => {
                self.lines[line].push_str(&tail);
                self.cursor.1 = column + first.chars().count();
            }
            Some(last) => {
                let end = last.chars().count();
                last.push_str(&tail);
                let count = added.len();
                self.lines.splice(line + 1..line + 1, added);
                self.cursor = (line + count, end);
            }
        }
        Outcome::Edited
    }

    fn insert_newline(&mut self) -> Outcome {
        self.begin_edit();
        self.break_line();
        Outcome::Edited
    }

    /// Splits the line at the cursor and puts the cursor on the new one.
    fn break_line(&mut self) {
        let (line, column) = self.cursor;
        let at = byte_index(&self.lines[line], column);
        let rest = self.lines[line].split_off(at);
        self.lines.insert(line + 1, rest);
        self.cursor = (line + 1, 0);
    }

    fn backspace(&mut self) -> Outcome {
        let (line, column) = self.cursor;
        if column > 0 {
            self.begin_edit();
            self.remove(line, column - 1, column);
            self.cursor.1 = column - 1;
            return Outcome::Edited;
        }
        if line == 0 {
            return Outcome::Unchanged;
        }
        self.begin_edit();
        let tail = self.lines.remove(line);
        let end = self.line_length(line - 1);
        self.lines[line - 1].push_str(&tail);
        self.cursor = (line - 1, end);
        Outcome::Edited
    }

    fn delete(&mut self) -> Outcome {
        let (line, column) = self.cursor;
        if column < self.line_length(line) {
            self.begin_edit();
            self.remove(line, column, column + 1);
            return Outcome::Edited;
        }
        if line + 1 >= self.lines.len() {
            return Outcome::Unchanged;
        }
        self.begin_edit();
        let tail = self.lines.remove(line + 1);
        self.lines[line].push_str(&tail);
        Outcome::Edited
    }

    fn delete_to_line_start(&mut self) -> Outcome {
        let (line, column) = self.cursor;
        if column == 0 {
            return Outcome::Unchanged;
        }
        self.begin_edit();
        self.remove(line, 0, column);
        self.cursor.1 = 0;
        Outcome::Edited
    }

    fn delete_to_line_end(&mut self) -> Outcome {
        let (line, column) = self.cursor;
        let end = self.line_length(line);
        if column >= end {
            return self.delete();
        }
        self.begin_edit();
        self.remove(line, column, end);
        Outcome::Edited
    }

    fn delete_word_back(&mut self) -> Outcome {
        let (line, column) = self.cursor;
        let (word_line, word_column) = self.word_left();
        if (word_line, word_column) == (line, column) {
            return Outcome::Unchanged;
        }
        self.begin_edit();
        if word_line == line {
            self.remove(line, word_column, column);
            self.cursor.1 = word_column;
            return Outcome::Edited;
        }
        // The cursor was at the start of a line, so the word before it is the
        // line break itself.
        self.backspace()
    }

    fn remove(&mut self, line: usize, from: usize, to: usize) {
        let text = &mut self.lines[line];
        let (from, to) = (byte_index(text, from), byte_index(text, to));
        text.replace_range(from..to, "");
    }

    /// Puts the cursor somewhere, keeping or dropping the selection.
    fn move_to(&mut self, to: (usize, usize), extend: bool) -> Outcome {
        if extend {
            self.selection.get_or_insert(self.cursor);
        } else {
            self.selection = None;
        }
        self.goal = None;
        let same = self.cursor == to;
        self.cursor = to;
        if same {
            Outcome::Unchanged
        } else {
            Outcome::Edited
        }
    }

    /// Up or down, keeping the column where the line is long enough.
    fn move_rows(&mut self, rows: isize, extend: bool) -> Outcome {
        let goal = self.goal.unwrap_or(self.cursor.1);
        let last = self.lines.len() - 1;
        let line = self.cursor.0.saturating_add_signed(rows).min(if rows > 0 {
            last
        } else {
            self.cursor.0
        });
        let outcome = self.move_to((line, goal.min(self.line_length(line))), extend);
        self.goal = Some(goal);
        outcome
    }

    fn left(&self) -> (usize, usize) {
        let (line, column) = self.cursor;
        match (column, line) {
            (0, 0) => (0, 0),
            (0, line) => (line - 1, self.line_length(line - 1)),
            (column, line) => (line, column - 1),
        }
    }

    fn right(&self) -> (usize, usize) {
        let (line, column) = self.cursor;
        if column < self.line_length(line) {
            return (line, column + 1);
        }
        if line + 1 < self.lines.len() {
            return (line + 1, 0);
        }
        (line, column)
    }

    /// The start of the word before the cursor, over the whitespace first.
    fn word_left(&self) -> (usize, usize) {
        let (line, column) = self.cursor;
        if column == 0 {
            return self.left();
        }
        let characters: Vec<char> = self.lines[line].chars().collect();
        let mut start = column.min(characters.len());
        while start > 0 && characters[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !characters[start - 1].is_whitespace() {
            start -= 1;
        }
        (line, start)
    }

    /// The end of the word after the cursor.
    fn word_right(&self) -> (usize, usize) {
        let (line, column) = self.cursor;
        let characters: Vec<char> = self.lines[line].chars().collect();
        if column >= characters.len() {
            return self.right();
        }
        let mut end = column;
        while end < characters.len() && characters[end].is_whitespace() {
            end += 1;
        }
        while end < characters.len() && !characters[end].is_whitespace() {
            end += 1;
        }
        (line, end)
    }

    fn line_length(&self, line: usize) -> usize {
        self.lines.get(line).map_or(0, |line| line.chars().count())
    }

    fn clamp_cursor(&mut self) {
        let line = self.cursor.0.min(self.lines.len() - 1);
        self.cursor = (line, self.cursor.1.min(self.line_length(line)));
    }
}

/// Text from outside — a paste, the editor, the file a pad was kept in — as
/// the pad holds it: a tab is [`INDENT`] and the other control characters
/// go, because the terminal would draw none of them and the cursor would be
/// a column off for each. A line break is `\n` whatever it came as: xterm
/// and the VTE terminals send a pasted one as a lone `\r`.
fn cleaned(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for character in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
        match character {
            '\n' => cleaned.push('\n'),
            '\t' => cleaned.push_str(INDENT),
            character if character.is_control() => {}
            character => cleaned.push(character),
        }
    }
    cleaned
}

/// A file's text as lines, always at least one. A byte-order mark is how
/// an editor said UTF-8, not the first character of the first statement.
fn split_lines(text: &str) -> Vec<String> {
    let text = cleaned(text.strip_prefix('\u{feff}').unwrap_or(text));
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// How many terminal columns a character of the pad is drawn in.
#[must_use]
pub fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Characters `from..to` of a line, `to` past the end meaning the rest.
fn slice(line: &str, from: usize, to: usize) -> String {
    line.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

/// Whether a statement holds anything to run: a note on lines of its own,
/// or a stray `;`, is not a statement, and Oracle says ORA-00900 to one —
/// which would stop a run of statements that were all fine.
fn has_code(text: &str) -> bool {
    let mut rest = text;
    loop {
        let code = code_start(rest);
        match code.strip_prefix(';') {
            Some(after) => rest = after,
            None => return !code.is_empty(),
        }
    }
}

/// Whether the lowercased line starts with this word and not merely with
/// those letters — `beginning` is not a `begin`.
fn starts_word(lower: &str, word: &str) -> bool {
    lower.strip_prefix(word).is_some_and(|rest| {
        rest.chars()
            .next()
            .is_none_or(|character| !character.is_alphanumeric() && character != '_')
    })
}

/// What a `create [or replace] [editionable]` line creates, if it is a
/// program: `procedure`, `function`, `trigger`, `package` (spec or body) or
/// `type body`. SQL Server's `alter` and `create or alter` define one too;
/// Oracle's `alter procedure … compile` does not, and has no `end` to wait for.
fn program_of(lower: &str, kind: Kind) -> Option<&'static str> {
    if !starts_word(lower, "create") && !(kind == Kind::Mssql && starts_word(lower, "alter")) {
        return None;
    }
    let mut words = lower
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .skip_while(|word| {
            [
                "create",
                "or",
                "replace",
                "alter",
                "editionable",
                "noneditionable",
            ]
            .contains(word)
        });
    match (words.next()?, words.next()) {
        ("procedure" | "proc", _) => Some("procedure"),
        ("function", _) => Some("function"),
        ("trigger", _) => Some("trigger"),
        ("package", _) => Some("package"),
        ("type", Some("body")) => Some("type body"),
        _ => None,
    }
}

/// Whether a line carries on the statement before it rather than starting
/// one: a clause no statement begins with, or a leading comma. Across a
/// blank line it is what keeps a `delete` with its `where`, rather than
/// running it on its own without one.
fn continues(lower: &str) -> bool {
    lower.starts_with([',', ')'])
        || [
            "where",
            "and",
            "or",
            "from",
            "join",
            "inner",
            "left",
            "right",
            "full",
            "cross",
            "outer",
            "on",
            "output",
            "group",
            "order",
            "having",
            "union",
            "intersect",
            "except",
            "minus",
            "when",
            "fetch",
            "offset",
        ]
        .iter()
        .any(|word| starts_word(lower, word))
}

/// Whether the line's code ends with this word: `end else begin`.
fn ends_word(code: &str, word: &str) -> bool {
    code.trim_end_matches(';')
        .trim_end()
        .strip_suffix(word)
        .is_some_and(|before| {
            before
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric() && character != '_')
        })
}

/// `begin tran` and its spellings start a transaction, not a block, and
/// have no `end` to wait for.
fn begins_transaction(lower: &str) -> bool {
    let next = lower["begin".len()..].trim_start();
    ["tran", "transaction", "distributed"]
        .iter()
        .any(|word| starts_word(next, word))
}

/// The line without a trailing `--` comment, so `select 1; -- why` still
/// ends at its semicolon.
fn code_of(line: &str) -> &str {
    let mut quoted = false;
    let mut previous = ' ';
    for (at, character) in line.char_indices() {
        match character {
            '\'' => quoted = !quoted,
            '-' if !quoted && previous == '-' => return line[..at - 1].trim_end(),
            _ => {}
        }
        previous = character;
    }
    line
}

/// One statement's text cut at every `;` outside a quote or a comment, the
/// `;` going with what it ends. A trailing comment stays with the statement
/// before it rather than being sent on its own.
fn split_semicolons(text: &str) -> Vec<String> {
    /// Copies characters until what was copied ends with `end`.
    fn until(chars: &mut std::str::Chars<'_>, piece: &mut String, end: &str) {
        let from = piece.len();
        for character in chars.by_ref() {
            piece.push(character);
            if piece[from..].ends_with(end) {
                return;
            }
        }
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut piece = String::new();
    let mut code = false;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        piece.push(character);
        let next = chars.clone().next();
        match character {
            // Oracle's other quote, `q'[it's; fine]'`, ends at the delimiter
            // that closes it and not at the next `'`.
            '\'' if is_q_quote(&piece[..piece.len() - 1]) => {
                code = true;
                if let Some(open) = chars.next() {
                    piece.push(open);
                    let close = match open {
                        '[' => ']',
                        '(' => ')',
                        '{' => '}',
                        '<' => '>',
                        other => other,
                    };
                    until(&mut chars, &mut piece, &format!("{close}'"));
                }
            }
            '\'' | '"' => {
                code = true;
                until(&mut chars, &mut piece, &character.to_string());
            }
            '-' if next == Some('-') => until(&mut chars, &mut piece, "\n"),
            '/' if next == Some('*') => {
                piece.push('*');
                chars.next();
                until(&mut chars, &mut piece, "*/");
            }
            ';' if code => {
                pieces.push(piece.trim().to_owned());
                piece.clear();
                code = false;
                // A block after it on the line is one statement to its end:
                // its own semicolons are not where it stops.
                let rest = chars.as_str();
                let next = code_start(rest).to_ascii_lowercase();
                if starts_word(&next, "begin") || starts_word(&next, "declare") {
                    pieces.push(rest.trim().to_owned());
                    return pieces;
                }
            }
            ';' => {}
            character if !character.is_whitespace() => code = true,
            _ => {}
        }
    }
    match pieces.last_mut() {
        Some(last) if !code => last.push_str(piece.trim_end()),
        _ => pieces.push(piece.trim().to_owned()),
    }
    pieces
}

/// Whether a `'` after `text` opens Oracle's `q'…'` (or `nq'…'`): the `q`
/// a word of its own and not the end of a name.
fn is_q_quote(text: &str) -> bool {
    let Some(before) = text.strip_suffix(['q', 'Q']) else {
        return false;
    };
    let before = before.strip_suffix(['n', 'N']).unwrap_or(before);
    before
        .chars()
        .next_back()
        .is_none_or(|character| !character.is_alphanumeric() && character != '_')
}

/// Whether this line's code closes a `begin`. `end if`, `end loop` and `end
/// case` close something that never opened one, and a block that ended at
/// the first `end loop;` would be a statement cut in half; so does a `case`
/// expression's `end` that goes on — `end as total,`, `end)`, `end + 1`.
fn closes_block(code: &str) -> bool {
    let Some(rest) = code.strip_prefix("end") else {
        return false;
    };
    if rest
        .chars()
        .next()
        .is_some_and(|character| character.is_alphanumeric() || character == '_')
    {
        return false;
    }
    let rest = rest.trim_start();
    if ["if", "loop", "case", "as"]
        .iter()
        .any(|word| starts_word(rest, word))
    {
        return false;
    }
    // Nothing, the `;`, a label (`end my_proc;`) or T-SQL's `end else`.
    rest.is_empty()
        || rest.starts_with(';')
        || rest.starts_with(|character: char| {
            character.is_alphabetic() || character == '_' || character == '"'
        })
}

fn byte_index(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map_or(text.len(), |(index, _)| index)
}

/// The file `<connection>.sql` is saved as, or `None` for no name at all. A
/// connection may be called anything — `prod/reporting`, `../../etc` — so
/// what would make the name a path, a slash either way, a NUL or a leading
/// dot, is written as its `%xx`: the pad stays in its directory and is still
/// saved, where refusing the name would lose it on every quit.
#[must_use]
pub fn file_name(connection: &str) -> Option<String> {
    if connection.is_empty() {
        return None;
    }
    let mut name = String::with_capacity(connection.len() + ".sql".len());
    for (at, character) in connection.char_indices() {
        match character {
            '/' => name.push_str("%2F"),
            '\\' => name.push_str("%5C"),
            '\0' => name.push_str("%00"),
            '.' if at == 0 => name.push_str("%2E"),
            character => name.push(character),
        }
    }
    Some(format!("{name}.sql"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::key;

    /// The buffer every edit test starts from, and a cursor in the middle of
    /// it: line 1, just after `from `.
    fn pad() -> Scratch {
        let mut scratch =
            Scratch::new("select id, name\nfrom bench.customers\nwhere country = 'US'");
        scratch.cursor = (1, 5);
        scratch
    }

    fn press(scratch: &mut Scratch, spec: &str) -> Outcome {
        scratch.handle(key(spec))
    }

    /// The pad after these keys, as text, with the cursor where it ended.
    fn after(specs: &[&str]) -> (String, (usize, usize)) {
        let mut scratch = pad();
        for spec in specs {
            press(&mut scratch, spec);
        }
        (scratch.text(), scratch.cursor)
    }

    #[test]
    fn typing_a_character_a_tab_and_a_newline_puts_them_where_the_cursor_is() {
        assert_eq!(
            after(&["x"]),
            (
                "select id, name\nfrom xbench.customers\nwhere country = 'US'".to_owned(),
                (1, 6)
            )
        );
        assert_eq!(
            after(&["Tab"]),
            (
                "select id, name\nfrom   bench.customers\nwhere country = 'US'".to_owned(),
                (1, 7)
            ),
            "Tab is two spaces and never a tab character"
        );
        assert_eq!(
            after(&["Enter"]),
            (
                "select id, name\nfrom \nbench.customers\nwhere country = 'US'".to_owned(),
                (2, 0)
            )
        );
    }

    #[test]
    fn backspace_and_delete_take_one_character_and_then_the_line_break() {
        assert_eq!(
            after(&["Backspace"]),
            (
                "select id, name\nfrombench.customers\nwhere country = 'US'".to_owned(),
                (1, 4)
            )
        );
        assert_eq!(
            after(&["Delete"]),
            (
                "select id, name\nfrom ench.customers\nwhere country = 'US'".to_owned(),
                (1, 5)
            )
        );

        let mut scratch = pad();
        scratch.cursor = (1, 0);
        press(&mut scratch, "Backspace");
        assert_eq!(
            (scratch.text(), scratch.cursor),
            (
                "select id, namefrom bench.customers\nwhere country = 'US'".to_owned(),
                (0, 15)
            ),
            "a backspace at the left edge joins the line to the one above"
        );

        scratch.cursor = (0, 35);
        press(&mut scratch, "Delete");
        assert_eq!(
            scratch.text(),
            "select id, namefrom bench.customerswhere country = 'US'",
            "and a delete at the right edge pulls the next one up"
        );

        let mut empty = Scratch::default();
        assert_eq!(press(&mut empty, "Backspace"), Outcome::Unchanged);
        assert_eq!(press(&mut empty, "Delete"), Outcome::Unchanged);
    }

    #[test]
    fn the_line_keys_cut_to_the_start_to_the_end_and_over_the_word_before() {
        assert_eq!(
            after(&["Ctrl-U"]),
            (
                "select id, name\nbench.customers\nwhere country = 'US'".to_owned(),
                (1, 0)
            )
        );
        assert_eq!(
            after(&["Ctrl-K"]),
            (
                "select id, name\nfrom \nwhere country = 'US'".to_owned(),
                (1, 5)
            )
        );
        assert_eq!(
            after(&["Right", "Right", "Right", "Right", "Right", "Ctrl-W"]),
            (
                "select id, name\nfrom .customers\nwhere country = 'US'".to_owned(),
                (1, 5)
            ),
            "the word before the cursor goes, and the whitespace before that"
        );
        assert_eq!(
            after(&["Ctrl-W"]),
            (
                "select id, name\nbench.customers\nwhere country = 'US'".to_owned(),
                (1, 0)
            ),
            "the whitespace before the cursor goes with the word"
        );

        let mut scratch = pad();
        scratch.cursor = (1, 20);
        assert_eq!(
            press(&mut scratch, "Ctrl-K"),
            Outcome::Edited,
            "at the end of a line Ctrl-K takes the line break"
        );
        assert_eq!(
            scratch.text(),
            "select id, name\nfrom bench.customerswhere country = 'US'"
        );
    }

    #[test]
    fn moving_keeps_the_column_where_the_line_is_long_enough() {
        let mut scratch = pad();
        scratch.cursor = (1, 18);
        press(&mut scratch, "Up");
        assert_eq!(scratch.cursor, (0, 15), "the line above is shorter");
        press(&mut scratch, "Down");
        assert_eq!(scratch.cursor, (1, 18), "and the column comes back");

        assert_eq!(after(&["Home"]).1, (1, 0));
        assert_eq!(after(&["End"]).1, (1, 20));
        assert_eq!(
            after(&["Ctrl-Right"]).1,
            (1, 20),
            "over the space and the word"
        );
        assert_eq!(after(&["Ctrl-Left"]).1, (1, 0));
        assert_eq!(after(&["Left", "Left", "Left", "Left", "Left"]).1, (1, 0));
        assert_eq!(
            after(&["Left", "Left", "Left", "Left", "Left", "Left"]).1,
            (0, 15),
            "and one more goes up over the line break"
        );
        assert_eq!(after(&["PageUp"]).1, (0, 5));
        assert_eq!(after(&["PageDown"]).1, (2, 5));
        assert_eq!(
            after(&["End", "Right"]).1,
            (2, 0),
            "and down over the next one"
        );
    }

    #[test]
    fn a_shift_arrow_selects_what_it_moves_over_and_ctrl_c_copies_it() {
        let mut scratch = pad();
        assert_eq!(
            press(&mut scratch, "Ctrl-C"),
            Outcome::CopyStatement,
            "nothing selected, so the app copies the statement"
        );

        for _ in 0..4 {
            press(&mut scratch, "Shift-Right");
        }
        assert_eq!(scratch.selection(), Some(((1, 5), (1, 9))));
        assert_eq!(
            press(&mut scratch, "Ctrl-C"),
            Outcome::Copy("benc".to_owned())
        );

        press(&mut scratch, "Shift-Down");
        assert_eq!(
            scratch.selected_text().as_deref(),
            Some("bench.customers\nwhere cou"),
            "a selection over a line break carries the break"
        );

        press(&mut scratch, "Left");
        assert_eq!(scratch.selection(), None, "a plain move drops it");
    }

    /// The pad with `benc` on line 1 through `whe` on line 2 selected.
    fn selected() -> Scratch {
        let mut scratch = pad();
        press(&mut scratch, "Shift-Down");
        for _ in 0..2 {
            press(&mut scratch, "Shift-Left");
        }
        assert_eq!(
            scratch.selected_text().as_deref(),
            Some("bench.customers\nwhe")
        );
        scratch
    }

    #[test]
    fn what_is_typed_or_pasted_replaces_the_selection_and_one_undo_takes_both_back() {
        let original = pad().text();
        let replaced = "select id, name\nfrom re country = 'US'";
        for (keys, text, cursor) in [
            (
                &["x"][..],
                "select id, name\nfrom xre country = 'US'",
                (1, 6),
            ),
            (
                &["Enter"],
                "select id, name\nfrom \nre country = 'US'",
                (2, 0),
            ),
            (
                &["Tab"],
                "select id, name\nfrom   re country = 'US'",
                (1, 7),
            ),
            (&["Backspace"], replaced, (1, 5)),
            (&["Delete"], replaced, (1, 5)),
            (&["Ctrl-U"], replaced, (1, 5)),
            (&["Ctrl-K"], replaced, (1, 5)),
            (&["Ctrl-W"], replaced, (1, 5)),
        ] {
            let mut scratch = selected();
            for key in keys {
                assert_eq!(press(&mut scratch, key), Outcome::Edited, "{keys:?}");
            }
            assert_eq!(
                (scratch.text(), scratch.cursor),
                (text.to_owned(), cursor),
                "{keys:?}"
            );
            assert_eq!(scratch.selection(), None, "{keys:?}");
            press(&mut scratch, "Ctrl-Z");
            assert_eq!(scratch.text(), original, "{keys:?} is one undo");
        }

        let mut scratch = selected();
        scratch.paste("a\nb");
        assert_eq!(
            scratch.text(),
            "select id, name\nfrom a\nbre country = 'US'"
        );
        press(&mut scratch, "Ctrl-Z");
        assert_eq!(scratch.text(), original, "a paste over it is one undo too");
    }

    #[test]
    fn ctrl_x_cuts_the_selection_and_nothing_without_one() {
        let mut scratch = selected();
        assert_eq!(
            press(&mut scratch, "Ctrl-X"),
            Outcome::Cut("bench.customers\nwhe".to_owned())
        );
        assert_eq!(scratch.text(), "select id, name\nfrom re country = 'US'");
        assert_eq!(press(&mut scratch, "Ctrl-X"), Outcome::Unchanged);
        press(&mut scratch, "Ctrl-Z");
        assert_eq!(scratch.text(), pad().text());
    }

    #[test]
    fn ctrl_a_selects_the_whole_pad_and_typing_replaces_it() {
        let mut scratch = pad();
        assert_eq!(press(&mut scratch, "Ctrl-A"), Outcome::Edited);
        assert_eq!(scratch.selected_text(), Some(pad().text()));
        assert_eq!(scratch.cursor, (2, 20));
        press(&mut scratch, "y");
        assert_eq!((scratch.text(), scratch.cursor), ("y".to_owned(), (0, 1)));
        press(&mut scratch, "Ctrl-Z");
        assert_eq!(scratch.text(), pad().text());
    }

    #[test]
    fn a_paste_whose_lines_end_in_a_lone_carriage_return_keeps_its_lines() {
        let mut scratch = Scratch::default();
        scratch.paste("select 1 as a\rselect 2 as b\r\nselect 3");
        assert_eq!(
            scratch.lines(),
            ["select 1 as a", "select 2 as b", "select 3"]
        );
    }

    #[test]
    fn a_paste_is_one_edit_however_many_lines_it_has() {
        let mut scratch = pad();
        assert_eq!(scratch.paste("x\r\ny\tz"), Outcome::Edited);
        assert_eq!(
            scratch.text(),
            "select id, name\nfrom x\ny  zbench.customers\nwhere country = 'US'"
        );
        assert_eq!(scratch.cursor, (2, 4));
        assert_eq!(scratch.paste(""), Outcome::Unchanged);
    }

    #[test]
    fn ctrl_z_takes_back_the_burst_that_a_pause_ended_and_no_more() {
        let started = Instant::now();
        let mut scratch = pad();
        assert_eq!(
            press(&mut scratch, "Ctrl-Z"),
            Outcome::Unchanged,
            "nothing typed yet"
        );

        press(&mut scratch, "x");
        press(&mut scratch, "y");
        assert!(
            !scratch.settle(started),
            "the first settle only starts the clock"
        );
        assert!(
            !scratch.settle(started + SETTLE / 2),
            "half a pause is no pause"
        );
        assert!(
            scratch.settle(started + SETTLE),
            "and then the pad is owed to the disk"
        );
        scratch.saved();

        // A new burst after the pause, which is all Ctrl-Z takes back.
        press(&mut scratch, "z");
        press(&mut scratch, "Ctrl-Z");
        assert_eq!(
            scratch.text(),
            "select id, name\nfrom xybench.customers\nwhere country = 'US'"
        );
        assert!(scratch.modified(), "an undo is a change like any other");
        assert_eq!(
            press(&mut scratch, "Ctrl-Z"),
            Outcome::Unchanged,
            "one level, and one only"
        );
    }

    #[test]
    fn a_pad_is_settled_once_and_saved_once() {
        let started = Instant::now();
        let mut scratch = pad();
        assert!(!scratch.settling(), "an untouched pad owes nothing");
        press(&mut scratch, "x");
        assert!(scratch.settling());
        assert!(!scratch.settle(started));
        assert!(scratch.settle(started + SETTLE));
        assert!(!scratch.settling(), "and it is not owed twice");
        assert!(!scratch.settle(started + SETTLE * 4));
    }

    #[test]
    fn the_editor_round_trip_replaces_the_pad_and_leaves_the_cursor_inside_it() {
        let mut scratch = pad();
        scratch.cursor = (2, 19);
        scratch.set_text("select 1");
        assert_eq!(scratch.text(), "select 1");
        assert_eq!(
            scratch.cursor,
            (0, 8),
            "the cursor is put back inside the pad"
        );
        assert!(scratch.modified(), "what came back is not what is on disk");

        let mut same = pad();
        same.set_text(&same.text());
        assert!(
            !same.modified(),
            "an editor that saved nothing changed nothing"
        );

        let mut tabbed = pad();
        tabbed.set_text("begin\r\n\tnull;\u{7}\r\nend;\n");
        assert_eq!(
            tabbed.lines(),
            ["begin", "  null;", "end;"],
            "a tab is the pad's indent and a bell is nothing, as in a paste"
        );
    }

    /// Six statements: two plain, a block, a GO batch, one ended by a
    /// semicolon and one trailing with no terminator at all.
    const SIX: &str = "select 1;

select *
from bench.customers
where country = 'US';

begin
  insert into bench.events (kind) values ('a');
  insert into bench.events (kind) values ('b');
end;

update bench.customers set country = 'CA'
GO
exec bench.refresh_stats;

select count(*) from bench.events";

    fn split(text: &str, kind: Kind) -> Vec<String> {
        Scratch::new(text)
            .statements(kind)
            .into_iter()
            .map(|(sql, _)| sql)
            .collect()
    }

    #[test]
    fn the_splitter_finds_all_six_statements_of_the_fixture() {
        assert_eq!(
            split(SIX, Kind::Mssql),
            [
                "select 1;",
                "select *\nfrom bench.customers\nwhere country = 'US';",
                "begin\n  insert into bench.events (kind) values ('a');\n  insert into bench.events (kind) values ('b');\nend;",
                "update bench.customers set country = 'CA'",
                "exec bench.refresh_stats;",
                "select count(*) from bench.events",
            ]
        );
    }

    #[test]
    fn go_ends_a_batch_for_sql_server_and_a_slash_one_for_oracle() {
        assert_eq!(
            split("select 1\ngo\nselect 2\n/\nselect 3", Kind::Mssql),
            ["select 1", "select 2\n/\nselect 3"],
            "GO is SQL Server's, whatever its case, and a slash is not"
        );
        assert_eq!(
            split("select 1\ngo\nselect 2\n/\nselect 3", Kind::Oracle),
            ["select 1\ngo\nselect 2", "select 3"]
        );
    }

    #[test]
    fn a_plsql_block_is_one_statement_however_many_semicolons_it_has() {
        let block = "declare
  v number;
begin
  for row in (select 1 from dual) loop
    begin
      select count(*) into v from bench.events;
    end;
  end loop;
end;
/
select v from dual";
        assert_eq!(
            split(block, Kind::Oracle),
            [
                "declare\n  v number;\nbegin\n  for row in (select 1 from dual) loop\n    begin\n      select count(*) into v from bench.events;\n    end;\n  end loop;\nend;",
                "select v from dual",
            ],
            "the blank-line and semicolon rules stop at the edge of a block"
        );
        assert_eq!(
            split("select beginning, ending from t;", Kind::Oracle),
            ["select beginning, ending from t;"],
            "a word that starts with begin is not a begin"
        );
    }

    #[test]
    fn a_tsql_transaction_or_declare_does_not_swallow_the_rest_of_the_pad() {
        assert_eq!(
            split(
                "begin tran;\nupdate t set a = 1;\ncommit;\n\nselect 1;\n\nselect 2;",
                Kind::Mssql
            ),
            [
                "begin tran;",
                "update t set a = 1;",
                "commit;",
                "select 1;",
                "select 2;"
            ],
            "begin tran opens a transaction, not a block"
        );
        assert_eq!(
            split(
                "declare @x int = 1;\nselect @x;\n\nselect 2;\n\nselect 3;",
                Kind::Mssql
            ),
            ["declare @x int = 1;\nselect @x;", "select 2;", "select 3;"],
            "a declare keeps its batch together up to the blank line"
        );
        assert_eq!(
            split(
                "if 1 = 1\nbegin\n  print 'a';\nend\n\nselect 2;",
                Kind::Mssql
            ),
            ["if 1 = 1\nbegin\n  print 'a';\nend", "select 2;"],
            "an end without a semicolon still closes the block"
        );
    }

    #[test]
    fn an_oracle_program_is_one_statement_through_its_declarations() {
        let procedure = "create or replace procedure p is\n  v number;\nbegin\n  null;\nend;";
        assert_eq!(
            split(&format!("{procedure}\n/\nselect 1 from dual"), Kind::Oracle),
            [procedure, "select 1 from dual"]
        );
        let package = "create package body k as\n  procedure a is\n  begin\n    null;\n  end;\n  procedure b is begin null; end;\nend k;";
        assert_eq!(
            split(&format!("{package}\nselect 1 from dual;"), Kind::Oracle),
            [package, "select 1 from dual;"],
            "a package body ends at its own end, not its first procedure's"
        );
    }

    #[test]
    fn oracle_statements_on_one_line_are_cut_at_their_semicolons() {
        assert_eq!(
            split(
                "select 1 a from dual; select ';' b from dual; -- two",
                Kind::Oracle
            ),
            ["select 1 a from dual;", "select ';' b from dual; -- two"],
            "a quoted semicolon is text and a trailing comment stays put"
        );
        assert_eq!(
            split("select 1; select 2", Kind::Mssql),
            ["select 1; select 2"],
            "SQL Server takes the line as one batch"
        );
        assert_eq!(
            split("begin null; null; end;", Kind::Oracle),
            ["begin null; null; end;"],
            "a block is never cut"
        );
    }

    #[test]
    fn a_trailing_comment_does_not_hide_the_semicolon() {
        assert_eq!(
            split("select 1; -- first\nselect '--' from t;", Kind::Mssql),
            ["select 1; -- first", "select '--' from t;"]
        );
    }

    #[test]
    fn a_statement_is_trimmed_and_never_empty() {
        assert_eq!(split("\n\n   \n\n", Kind::Mssql), Vec::<String>::new());
        assert_eq!(split(";", Kind::Mssql), Vec::<String>::new());
        assert_eq!(split("  select 1  \n\n\n", Kind::Mssql), ["select 1"]);
    }

    #[test]
    fn a_note_on_its_own_is_not_a_statement_and_one_in_front_of_a_block_is_read_past() {
        for kind in [Kind::Mssql, Kind::Oracle] {
            assert_eq!(
                split(
                    "-- setup\n\nselect 1 from dual;\n-- done\n/* really */\n\n;",
                    kind
                ),
                ["select 1 from dual;"],
                "{kind:?}"
            );
        }
        assert_eq!(
            split(
                "/* why */ begin\n  null;\nend;\nselect 2 from dual;",
                Kind::Oracle
            ),
            ["/* why */ begin\n  null;\nend;", "select 2 from dual;"],
            "the block is opened by the begin after the note"
        );
        assert_eq!(
            split(
                "-- note\ndeclare @x int = 1;\nselect @x;\n\nselect 2;",
                Kind::Mssql
            ),
            ["-- note\ndeclare @x int = 1;\nselect @x;", "select 2;"]
        );
    }

    #[test]
    fn a_tsql_procedure_runs_to_go_blank_lines_and_all() {
        let procedure = "create or alter procedure dbo.purge as\n  set nocount on;\n\n  \
                         delete from dbo.t where old = 1;";
        assert_eq!(
            split(&format!("{procedure}\nGO\nselect 1;"), Kind::Mssql),
            [procedure, "select 1;"],
            "the delete is the procedure's, not a statement run on its own"
        );
        let altered = "alter procedure p as\n  select 1;\n\n  select 2;";
        assert_eq!(split(altered, Kind::Mssql), [altered]);
    }

    #[test]
    fn a_blank_line_before_a_clause_does_not_cut_the_statement_it_belongs_to() {
        for kind in [Kind::Mssql, Kind::Oracle] {
            assert_eq!(
                split("delete from t\n\nwhere a = 999;", kind),
                ["delete from t\n\nwhere a = 999;"],
                "{kind:?}: never a delete without its where"
            );
            assert_eq!(
                split("delete from t\n\n-- only the old ones\nwhere x < 5;", kind),
                ["delete from t\n\n-- only the old ones\nwhere x < 5;"],
                "{kind:?}"
            );
            assert_eq!(
                split("select 1 from t\n\n-- next\nselect 2 from t", kind),
                ["select 1 from t", "-- next\nselect 2 from t"],
                "{kind:?}: a new statement still starts after a blank line"
            );
        }
    }

    #[test]
    fn a_case_expressions_end_and_an_end_else_begin_keep_a_tsql_block_whole() {
        let case = "if 1 = 1\nbegin\n  select case\n    when 1 = 1 then 'a'\n  end as w;\nend";
        assert_eq!(
            split(&format!("{case}\n\nselect 2;"), Kind::Mssql),
            [case, "select 2;"]
        );
        let branches = "if 1 = 1\nbegin\n  select 1;\nend else begin\n  select 2;\nend";
        assert_eq!(
            split(&format!("{branches}\n\nselect 3;"), Kind::Mssql),
            [branches, "select 3;"]
        );
    }

    #[test]
    fn an_oracle_subprogram_declared_in_a_block_does_not_end_it() {
        let block = "declare\n  procedure p is\n  begin\n    null;\n  end;\nbegin\n  p;\nend;";
        assert_eq!(
            split(&format!("{block}\nselect 1 from dual;"), Kind::Oracle),
            [block, "select 1 from dual;"]
        );
        let outer = "create or replace procedure outer is\n  function f return number is\n  \
                     begin\n    return 1;\n  end;\n  procedure later;\nbegin\n  null;\nend;";
        assert_eq!(
            split(&format!("{outer}\n/\nselect 1 from dual;"), Kind::Oracle),
            [outer, "select 1 from dual;"],
            "a forward declaration has no end of its own"
        );
    }

    #[test]
    fn oracle_quotes_and_one_line_blocks_are_not_cut_inside() {
        assert_eq!(
            split("begin null; end;\nselect 1 from dual;", Kind::Oracle),
            ["begin null; end;", "select 1 from dual;"],
            "a block on one line closes itself"
        );
        assert_eq!(
            split(
                "select q'[it's; fine]' a from dual; select 2 from dual;",
                Kind::Oracle
            ),
            ["select q'[it's; fine]' a from dual;", "select 2 from dual;"]
        );
        assert_eq!(
            split("select 1 from dual; begin null; end;", Kind::Oracle),
            ["select 1 from dual;", "begin null; end;"]
        );
    }

    #[test]
    fn the_statement_under_the_cursor_is_the_one_it_is_in_or_the_one_before_it() {
        let mut scratch = Scratch::new(SIX);
        let at = |scratch: &Scratch| {
            scratch
                .statement_at_cursor(Kind::Mssql)
                .expect("a statement")
        };

        scratch.cursor = (3, 0);
        assert_eq!(
            at(&scratch),
            (
                "select *\nfrom bench.customers\nwhere country = 'US';".to_owned(),
                2..5
            )
        );

        scratch.cursor = (5, 0);
        assert_eq!(
            at(&scratch).1,
            2..5,
            "a blank line belongs to what was typed before it"
        );

        scratch.cursor = (12, 0);
        assert_eq!(
            at(&scratch).0,
            "update bench.customers set country = 'CA'",
            "and so does a GO"
        );

        scratch.cursor = (15, 0);
        assert_eq!(at(&scratch).0, "select count(*) from bench.events");

        assert_eq!(
            Scratch::default().statement_at_cursor(Kind::Mssql),
            None,
            "an empty pad runs nothing"
        );
    }

    /// A driver counts a statement's lines from its own first, so each
    /// piece of an Oracle line needs its own and not the whole run of them.
    #[test]
    fn two_oracle_statements_across_three_lines_are_each_on_their_own() {
        let scratch = Scratch::new("select 1\nfrom dual; select 2\nfrom dual;");
        assert_eq!(
            scratch.statements(Kind::Oracle),
            [
                ("select 1\nfrom dual;".to_owned(), 0..2),
                ("select 2\nfrom dual;".to_owned(), 1..3),
            ]
        );
    }

    #[test]
    fn the_window_keeps_the_cursor_on_the_screen_both_ways() {
        let mut scratch = Scratch::new(
            &(0..40)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert_eq!(scratch.window(10, 20), (0, 0));

        scratch.cursor = (9, 0);
        assert_eq!(
            scratch.window(10, 20),
            (0, 0),
            "the last row is still on it"
        );
        scratch.cursor = (10, 0);
        assert_eq!(scratch.window(10, 20), (1, 0));

        scratch.cursor = (10, 199);
        assert_eq!(
            scratch.window(10, 20),
            (1, 180),
            "and a long line scrolls sideways"
        );
        scratch.show_from(1, 180);
        scratch.cursor = (11, 3);
        assert_eq!(
            scratch.window(10, 20),
            (2, 0),
            "and back for a short one, which would otherwise show only `e 11`"
        );
    }

    #[test]
    fn the_window_stays_where_it_was_shown_from_while_the_cursor_is_on_it() {
        let mut scratch = Scratch::new(
            &(0..40)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        scratch.show_from(20, 0);
        scratch.cursor = (25, 0);
        assert_eq!(scratch.window(6, 20), (20, 0), "on it, so it stays");
        scratch.cursor = (26, 0);
        assert_eq!(scratch.window(6, 20), (21, 0), "one past, one down");
        scratch.cursor = (19, 0);
        assert_eq!(scratch.window(6, 20), (19, 0), "one above, one up");
        scratch.show_from(38, 0);
        scratch.cursor = (39, 0);
        assert_eq!(scratch.window(6, 20), (34, 0), "no rows under the last");
    }

    #[test]
    fn a_word_is_letters_digits_and_underscores_and_a_click_is_clamped_to_the_text() {
        let mut scratch = Scratch::new("select order_id, x2 from t\nend");
        scratch.place((0, 9), false);
        scratch.select_word();
        assert_eq!(scratch.selected_text().as_deref(), Some("order_id"));
        scratch.place((0, 18), false);
        scratch.select_word();
        assert_eq!(scratch.selected_text().as_deref(), Some("x2"));
        for (at, why) in [(15, "a comma"), (6, "a space"), (99, "past the end")] {
            scratch.place((0, at), false);
            scratch.select_word();
            assert_eq!(scratch.selected_text(), None, "{why}");
        }
        assert_eq!(scratch.cursor(), (0, 26));
        scratch.place((9, 9), false);
        assert_eq!(scratch.cursor(), (1, 3));
    }

    #[test]
    fn the_wheel_moves_the_view_and_the_cursor_only_as_far_as_it_has_to() {
        let mut scratch = Scratch::new(
            &(0..20)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        scratch.cursor = (2, 4);
        scratch.wheel((0, 0), 6, 3);
        assert_eq!((scratch.window(6, 20), scratch.cursor), ((3, 0), (3, 4)));
        scratch.wheel((3, 0), 6, -3);
        assert_eq!((scratch.window(6, 20), scratch.cursor), ((0, 0), (3, 4)));
        scratch.wheel((12, 0), 6, 3);
        assert_eq!(
            scratch.window(6, 20),
            (14, 0),
            "the last line at the bottom"
        );

        // With a selection the cursor drags its end along.
        scratch.place((14, 0), false);
        scratch.place((14, 2), true);
        scratch.wheel((14, 0), 6, -12);
        assert_eq!(scratch.selection(), Some(((7, 2), (14, 0))));
        assert!(!scratch.modified(), "scrolling is not an edit");
    }

    #[test]
    fn a_file_name_is_the_connection_name_and_nothing_that_climbs_out_of_the_directory() {
        assert_eq!(file_name("local mssql"), Some("local mssql.sql".to_owned()));
        assert_eq!(file_name(""), None);
        for (name, file) in [
            (".", "%2E.sql"),
            ("..", "%2E..sql"),
            ("../etc/passwd", "%2E.%2Fetc%2Fpasswd.sql"),
            ("prod/reporting", "prod%2Freporting.sql"),
            ("a\\b", "a%5Cb.sql"),
        ] {
            assert_eq!(file_name(name).as_deref(), Some(file), "{name:?}");
        }
    }
}
