//! `SQL_BENCH_TRACE=<file>`: one appended line per traced event, which is
//! rule 4 in `CLAUDE.md`.
//!
//! The format is the one `docs/DESIGN.md` fixes: `unix_ms\tkind\tk=v...`.
//! Unset, nothing is written and the clock is never read, so a build that is
//! not being measured pays nothing for the instrument. A write that fails is
//! dropped: a trace file is never worth taking the app down for.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default)]
pub struct Trace {
    file: Option<PathBuf>,
}

impl Trace {
    /// The file `$SQL_BENCH_TRACE` names, if it names one.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os("SQL_BENCH_TRACE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        )
    }

    #[must_use]
    pub fn new(file: Option<PathBuf>) -> Self {
        Self { file }
    }

    /// Whether anything is being recorded — worth asking before formatting
    /// the numbers an [`event`](Self::event) would carry.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.file.is_some()
    }

    pub fn event(&self, kind: &str, fields: &[(&str, &str)]) {
        use std::io::Write as _;
        let Some(path) = self.file.as_deref() else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut line = format!("{now}\t{kind}");
        for (key, value) in fields {
            line.push('\t');
            line.push_str(key);
            line.push('=');
            line.push_str(value);
        }
        line.push('\n');
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_a_timestamp_a_kind_and_its_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.tsv");
        let trace = Trace::new(Some(path.clone()));
        trace.event("frame", &[("draw_ms", "3")]);
        trace.event("turn", &[("total_ms", "41"), ("input_ms", "38")]);
        let written = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert!(fields[0].parse::<u64>().unwrap() > 1_700_000_000_000);
        assert_eq!(&fields[1..], ["frame", "draw_ms=3"]);
        assert_eq!(
            lines[1].split('\t').skip(1).collect::<Vec<_>>(),
            ["turn", "total_ms=41", "input_ms=38"]
        );
    }

    #[test]
    fn an_unset_trace_writes_nothing_and_says_so() {
        let trace = Trace::new(None);
        assert!(!trace.is_on());
        trace.event("frame", &[("draw_ms", "1")]);
    }
}
