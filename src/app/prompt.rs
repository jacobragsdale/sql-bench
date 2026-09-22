//! The footer's one-line prompt: where an export is about to be written, and
//! the handful of editing keys one line needs.
//!
//! Not the scratch pad. That one has lines, undo, a selection and a file
//! behind it, and none of them are what a path being typed over wants.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// One line being typed into the footer. The cursor counts characters and
/// not bytes, because it is where a person is and not where a `String` is.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Prompt {
    pub text: String,
    pub cursor: usize,
}

impl Prompt {
    /// A prompt holding `text`, with the cursor past the end of it — which
    /// is what makes Ctrl-U clear the prefill in one key.
    #[must_use]
    pub fn new(text: String) -> Self {
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// One key. Enter and Esc belong to whoever opened the prompt and never
    /// reach here.
    pub fn handle(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let length = self.text.chars().count();
        match key.code {
            KeyCode::Char('u' | 'U') if control => {
                self.text = self.text.chars().skip(self.cursor).collect();
                self.cursor = 0;
            }
            KeyCode::Char(character) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, character);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(length),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = length,
            _ => {}
        }
    }

    /// A paste: `text` at the cursor, and the cursor after it.
    pub fn insert(&mut self, text: &str) {
        let at = self.byte(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    /// The cursor onto the character drawn `column` cells from the start of
    /// the text, or past its end: a click on it.
    pub fn place(&mut self, column: usize) {
        let mut cells = 0;
        self.cursor = self
            .text
            .chars()
            .position(|character| {
                cells += character.width().unwrap_or(0);
                cells > column
            })
            .unwrap_or_else(|| self.text.chars().count());
    }

    /// Where character `at` starts, or the end of the text.
    fn byte(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map_or(self.text.len(), |(index, _)| index)
    }
}

/// `~/sql-bench-<connection>-<date>-<time>.csv`: what the export prompt opens
/// prefilled with.
///
/// The stamp is UTC. A file name wants one that sorts and is never two names
/// for one second, and `time`'s local offset is not built into this binary.
#[must_use]
pub fn export_path(connection: &str) -> String {
    let stamp = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    let now = time::OffsetDateTime::now_utc()
        .format(&stamp)
        .unwrap_or_default();
    format!("~/sql-bench-{connection}-{now}.csv")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::key;

    fn press(prompt: &mut Prompt, specs: &[&str]) {
        for spec in specs {
            prompt.handle(key(spec));
        }
    }

    #[test]
    fn a_prompt_opens_at_the_end_of_its_prefill_and_ctrl_u_clears_it() {
        let mut prompt = Prompt::new("~/out.csv".to_owned());
        assert_eq!(prompt.cursor, 9);
        press(&mut prompt, &["Ctrl-U"]);
        assert_eq!((prompt.text.as_str(), prompt.cursor), ("", 0));

        press(
            &mut prompt,
            &["/", "t", "m", "p", "/", "a", ".", "j", "s", "o", "n"],
        );
        assert_eq!(prompt.text, "/tmp/a.json");
        assert_eq!(prompt.cursor, 11);
    }

    #[test]
    fn the_editing_keys_move_and_delete_by_character() {
        let mut prompt = Prompt::new("Zoë.csv".to_owned());
        press(&mut prompt, &["Home"]);
        assert_eq!(prompt.cursor, 0);
        press(&mut prompt, &["Right", "Right", "Right"]);
        // Past a two-byte character, and the byte index is not the cursor.
        press(&mut prompt, &["Backspace"]);
        assert_eq!(prompt.text, "Zo.csv");
        press(&mut prompt, &["Left", "x"]);
        assert_eq!(prompt.text, "Zxo.csv");
        press(&mut prompt, &["End", "!"]);
        assert_eq!(prompt.text, "Zxo.csv!");

        // Ctrl-U from the middle keeps what is after the cursor.
        press(&mut prompt, &["Home", "Right", "Right", "Ctrl-U"]);
        assert_eq!((prompt.text.as_str(), prompt.cursor), ("o.csv!", 0));
        // Nothing to delete, and nowhere further left to go.
        press(&mut prompt, &["Backspace", "Left"]);
        assert_eq!((prompt.text.as_str(), prompt.cursor), ("o.csv!", 0));
    }

    #[test]
    fn a_click_lands_on_the_character_drawn_there() {
        let mut prompt = Prompt::new("日本.csv".to_owned());
        prompt.place(0);
        assert_eq!(prompt.cursor, 0);
        // Each of the first two is two cells wide.
        prompt.place(3);
        assert_eq!(prompt.cursor, 1);
        prompt.place(4);
        assert_eq!(prompt.cursor, 2);
        prompt.place(40);
        assert_eq!(prompt.cursor, 6, "past the end is the end");
    }

    #[test]
    fn the_prefill_names_the_connection_and_ends_in_csv() {
        let path = export_path("local-mssql");
        assert!(path.starts_with("~/sql-bench-local-mssql-"), "{path}");
        assert!(path.ends_with(".csv"), "{path}");
        // `~/sql-bench-` + name + `-20260915-142530.csv`
        assert_eq!(path.len(), "~/sql-bench-local-mssql-".len() + 19);
    }
}
