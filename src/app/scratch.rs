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

use crate::config::Kind;

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
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            selection: None,
            undo: None,
            goal: None,
            burst: false,
            edited: false,
            dirty_since: None,
            modified: false,
            flagged: None,
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
        if text == self.text() {
            return;
        }
        self.lines = split_lines(text);
        self.selection = None;
        self.undo = None;
        self.burst = false;
        self.flagged = None;
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

    /// Where a pane `height` rows by `width` columns starts showing the pad
    /// so that the cursor is on it: the first line and the first column.
    // ponytail: derived from the cursor rather than remembered, so the view
    // pins the cursor to the last row and column instead of scrolling by
    // pages; keep an offset in the pad if that ever reads badly.
    #[must_use]
    pub fn window(&self, height: usize, width: usize) -> (usize, usize) {
        let (line, column) = self.cursor;
        (
            line.saturating_sub(height.max(1) - 1),
            (column + 1).saturating_sub(width.max(1)),
        )
    }

    /// One key. Everything that is not the pad's own is [`Outcome::Unchanged`],
    /// which is how the shell keeps the keys it handles itself.
    pub fn handle(&mut self, key: KeyEvent) -> Outcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('r' | 'R') if control => Outcome::RunStatement,
            KeyCode::F(5) => Outcome::RunAll,
            KeyCode::Char('e' | 'E') if control => Outcome::OpenEditor,
            KeyCode::Char('c' | 'C') if control => self
                .selected_text()
                .map_or(Outcome::Unchanged, Outcome::Copy),
            KeyCode::Char('z' | 'Z') if control => self.undo(),
            KeyCode::Char('a' | 'A') if control => self.move_to((self.cursor.0, 0), shift),
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
            #[allow(clippy::cast_possible_wrap)]
            KeyCode::PageUp => self.move_rows(-(PAGE as isize), shift),
            #[allow(clippy::cast_possible_wrap)]
            KeyCode::PageDown => self.move_rows(PAGE as isize, shift),
            KeyCode::Home => self.move_to((self.cursor.0, 0), shift),
            KeyCode::End => self.move_to((self.cursor.0, self.line_length(self.cursor.0)), shift),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Tab => self.insert(INDENT),
            KeyCode::Char(character) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&character.to_string())
            }
            _ => Outcome::Unchanged,
        }
    }

    /// Bracketed paste: the whole block at the cursor, line breaks kept.
    pub fn paste(&mut self, text: &str) -> Outcome {
        let mut cleaned = String::with_capacity(text.len());
        for character in text.replace("\r\n", "\n").chars() {
            match character {
                '\n' => cleaned.push('\n'),
                '\t' => cleaned.push_str(INDENT),
                character if character.is_control() => {}
                character => cleaned.push(character),
            }
        }
        if cleaned.is_empty() {
            return Outcome::Unchanged;
        }
        self.insert(&cleaned)
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
    /// of the pad. `begin`/`declare` open a block whose `end` closes it, so
    /// the semicolons inside a PL/SQL block do not split it; the words are
    /// matched whatever their case.
    #[must_use]
    pub fn statements(&self, kind: Kind) -> Vec<(String, Range<usize>)> {
        let mut statements: Vec<(String, Range<usize>)> = Vec::new();
        let mut start: Option<usize> = None;
        let mut depth = 0usize;
        let mut block = false;
        let mut flush = |start: &mut Option<usize>, end: usize, lines: &[String]| {
            let Some(from) = start.take() else {
                return;
            };
            let text = lines[from..end].join("\n").trim().to_owned();
            if !text.is_empty() {
                statements.push((text, from..end));
            }
        };
        for (number, line) in self.lines.iter().enumerate() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            let terminator = match kind {
                Kind::Mssql => lower == "go",
                Kind::Oracle => trimmed == "/",
            };
            if terminator {
                flush(&mut start, number, &self.lines);
                (depth, block) = (0, false);
                continue;
            }
            if trimmed.is_empty() {
                if depth == 0 && !block {
                    flush(&mut start, number, &self.lines);
                }
                continue;
            }
            if start.is_none() {
                start = Some(number);
            }
            if starts_word(&lower, "declare") {
                block = true;
            }
            if starts_word(&lower, "begin") {
                block = true;
                depth += 1;
            }
            let closing = closes_block(&lower);
            if closing {
                depth = depth.saturating_sub(1);
            }
            // Inside a block only the `end` that closes it ends the
            // statement, however many semicolons the body has.
            if lower.ends_with(';') && depth == 0 && (!block || closing) {
                flush(&mut start, number + 1, &self.lines);
                block = false;
            }
        }
        flush(&mut start, self.lines.len(), &self.lines);
        statements
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

    /// The bookkeeping every edit does: the snapshot the burst is taken back
    /// to, and the flags the settle reads.
    fn begin_edit(&mut self) {
        self.flagged = None;
        if !self.burst {
            self.undo = Some((self.lines.clone(), self.cursor));
            self.burst = true;
        }
        self.selection = None;
        self.goal = None;
        self.modified = true;
        self.edited = true;
    }

    fn insert(&mut self, text: &str) -> Outcome {
        self.begin_edit();
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.break_line();
            }
            if !part.is_empty() {
                let (line, column) = self.cursor;
                let at = byte_index(&self.lines[line], column);
                self.lines[line].insert_str(at, part);
                self.cursor.1 = column + part.chars().count();
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

/// A file's text as lines, always at least one.
fn split_lines(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Characters `from..to` of a line, `to` past the end meaning the rest.
fn slice(line: &str, from: usize, to: usize) -> String {
    line.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
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

/// Whether this line closes a `begin`. `end if`, `end loop` and `end case`
/// close something that never opened one, and a block that ended at the
/// first `end loop;` would be a statement cut in half.
fn closes_block(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix("end") else {
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
    !["if", "loop", "case"]
        .iter()
        .any(|word| starts_word(rest, word))
}

fn byte_index(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map_or(text.len(), |(index, _)| index)
}

/// The file `<connection>.sql` is saved as, or `None` for a name that is not
/// one file — a connection may be called anything, including `../../etc`.
#[must_use]
pub fn file_name(connection: &str) -> Option<String> {
    let bad = connection.is_empty()
        || connection.contains(['/', '\\'])
        || connection.starts_with('.')
        || connection.contains('\0');
    (!bad).then(|| format!("{connection}.sql"))
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
        assert_eq!(after(&["Ctrl-A"]).1, (1, 0));
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
            Outcome::Unchanged,
            "nothing to copy"
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
    fn a_statement_is_trimmed_and_never_empty() {
        assert_eq!(split("\n\n   \n\n", Kind::Mssql), Vec::<String>::new());
        assert_eq!(split(";", Kind::Mssql), [";"]);
        assert_eq!(split("  select 1  \n\n\n", Kind::Mssql), ["select 1"]);
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
    }

    #[test]
    fn a_file_name_is_the_connection_name_and_nothing_that_climbs_out_of_the_directory() {
        assert_eq!(file_name("local mssql"), Some("local mssql.sql".to_owned()));
        for name in ["", ".", "..", "../etc/passwd", "a/b", "a\\b"] {
            assert_eq!(file_name(name), None, "{name:?}");
        }
    }
}
