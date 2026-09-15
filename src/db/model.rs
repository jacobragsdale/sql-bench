//! What a row is, once a driver has been asked for one.
//!
//! Both drivers report in these types, so everything above them — the grid,
//! the exporters, the headless subcommands — is written once. Values that
//! have no lossless Rust equivalent are carried as the text the driver
//! produced (`Decimal`, `DateTime`) rather than rounded into `f64` or
//! reinterpreted in a timezone nobody asked for.

use std::borrow::Cow;

/// One value in one row.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Exact text, scale preserved.
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    /// As the driver formats it; no timezone invented.
    DateTime(String),
}

impl Cell {
    /// The one text form of a value. Every renderer and every exporter goes
    /// through here, so a cell can never read one way in the grid and
    /// another in a CSV. `Null` is empty because that is what an export
    /// wants; a renderer that marks nulls matches [`Cell::Null`] itself.
    #[must_use]
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            Self::Null => Cow::Borrowed(""),
            Self::Bool(value) => Cow::Borrowed(if *value { "true" } else { "false" }),
            Self::Int(value) => Cow::Owned(value.to_string()),
            Self::Float(value) => Cow::Owned(value.to_string()),
            Self::Decimal(text) | Self::Text(text) | Self::DateTime(text) => Cow::Borrowed(text),
            // ponytail: the whole blob is formatted, so a megabyte of bytes
            // is two megabytes of hex. Cap it here if a BLOB column ever
            // shows up in a profile.
            Self::Bytes(bytes) => Cow::Owned(hex(bytes)),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One column of one result set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

/// What a running query reports back, in order, over the channel
/// `Connection::query` hands out.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryEvent {
    /// Once per result set.
    Columns(Vec<Column>),
    /// One batch.
    Rows(Vec<Vec<Cell>>),
    RowsAffected(u64),
    Done {
        rows: usize,
        truncated: bool,
        connect_ms: u32,
        first_row_ms: u32,
        total_ms: u32,
    },
    Error(DbError),
}

/// Anything a database can say no with. `anyhow` is for the edges; inside
/// `db/` the reason is a value, because the UI shows it and the tests match
/// on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbError {
    Connect(String),
    Query {
        message: String,
        /// The line of the submitted batch, when the server says.
        line: Option<u32>,
    },
    Cancelled,
    Timeout,
    Unsupported(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(why) => write!(f, "cannot connect: {why}"),
            Self::Query {
                message,
                line: Some(line),
            } => write!(f, "line {line}: {message}"),
            Self::Query {
                message,
                line: None,
            } => f.write_str(message),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Timeout => f.write_str("timed out"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
        }
    }
}

impl std::error::Error for DbError {}

/// How much of a result set to fetch, and how often to report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryOptions {
    pub batch_size: usize,
    pub max_rows: Option<usize>,
}

impl Default for QueryOptions {
    /// Batches of 500 keep the grid filling smoothly without a channel
    /// message per row; the 10k cap is what "more rows" raises.
    fn default() -> Self {
        Self {
            batch_size: 500,
            max_rows: Some(10_000),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cell_has_one_text_form() {
        assert_eq!(Cell::Null.display(), "");
        assert_eq!(Cell::Bool(true).display(), "true");
        assert_eq!(Cell::Bool(false).display(), "false");
        assert_eq!(Cell::Int(-42).display(), "-42");
        assert_eq!(Cell::Float(1.5).display(), "1.5");
        assert_eq!(Cell::Decimal("10.2500".to_owned()).display(), "10.2500");
        assert_eq!(Cell::Text("hello".to_owned()).display(), "hello");
        assert_eq!(Cell::Bytes(vec![0x00, 0x0f, 0xff]).display(), "0x000fff");
        assert_eq!(
            Cell::DateTime("2026-09-14 21:32:00".to_owned()).display(),
            "2026-09-14 21:32:00"
        );
    }

    #[test]
    fn text_is_borrowed_not_copied() {
        let cell = Cell::Text("a long value the grid draws every frame".to_owned());
        assert!(matches!(cell.display(), Cow::Borrowed(_)));
    }

    #[test]
    fn every_failure_says_what_it_was() {
        assert_eq!(
            DbError::Connect("no route to host".to_owned()).to_string(),
            "cannot connect: no route to host"
        );
        assert_eq!(
            DbError::Query {
                message: "Invalid column name 'nope'.".to_owned(),
                line: Some(3),
            }
            .to_string(),
            "line 3: Invalid column name 'nope'."
        );
        assert_eq!(
            DbError::Query {
                message: "Invalid column name 'nope'.".to_owned(),
                line: None,
            }
            .to_string(),
            "Invalid column name 'nope'."
        );
        assert_eq!(DbError::Cancelled.to_string(), "cancelled");
        assert_eq!(DbError::Timeout.to_string(), "timed out");
        assert_eq!(
            DbError::Unsupported("oracle".to_owned()).to_string(),
            "not supported: oracle"
        );
    }

    #[test]
    fn the_default_options_are_the_documented_ones() {
        let options = QueryOptions::default();
        assert_eq!(options.batch_size, 500);
        assert_eq!(options.max_rows, Some(10_000));
    }
}
