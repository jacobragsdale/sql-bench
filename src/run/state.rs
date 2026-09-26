//! Where a scratch pad lives between runs: one `.sql` file per connection
//! under `~/.local/state/sql-bench/scratch`, which `$SQL_BENCH_STATE_DIR`
//! moves somewhere else — a test, or a replay that must not touch the state
//! of the person running it.
//!
//! The app never reads or writes a file (rule 2), so every byte of this
//! happens here, off the back of the [`Action`](crate::app::Action)s the pad
//! asks for.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::scratch::{Scratch, file_name};
use crate::app::{App, Tab};

/// The directory the pads are in, or `None` when there is nowhere to put
/// them — no `$HOME` and no override, which is a run with no state, not a
/// failure.
#[derive(Clone, Debug, Default)]
pub struct Store {
    directory: Option<PathBuf>,
}

impl Store {
    /// `$SQL_BENCH_STATE_DIR`, else `$XDG_STATE_HOME/sql-bench`, else
    /// `~/.local/state/sql-bench` — and `scratch` inside whichever it is.
    #[must_use]
    pub fn from_env() -> Self {
        let root = std::env::var_os("SQL_BENCH_STATE_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_STATE_HOME")
                    .filter(|value| !value.is_empty())
                    .map(|state| PathBuf::from(state).join("sql-bench"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local/state/sql-bench"))
            });
        Self::new(root)
    }

    /// A store under this root, which is what a test hands a temp directory.
    #[must_use]
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            directory: root.map(|root| root.join("scratch")),
        }
    }

    /// The file this tab's pad is saved in, or `None` when the connection is
    /// named something that is not one file name.
    #[must_use]
    pub fn path(&self, connection: &str) -> Option<PathBuf> {
        Some(self.directory.as_ref()?.join(file_name(connection)?))
    }

    /// Fill every tab's pad from its file. A pad with no file is empty, and
    /// a file that cannot be read is not worth stopping a run over: what to
    /// say about it goes to the footer once the app is up.
    pub fn restore(&self, app: &mut App) -> Option<String> {
        let mut failed = None;
        for tab in &mut app.tabs {
            let Some(path) = self.path(&tab.name) else {
                continue;
            };
            match std::fs::read(&path) {
                // Bytes that are not UTF-8 come in as `�` rather than
                // leaving the pad empty, which the next save would write
                // over everything else in the file with.
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if matches!(text, std::borrow::Cow::Owned(_)) {
                        failed = Some(format!(
                            "scratch {} is not UTF-8: what was not reads as �",
                            path.display()
                        ));
                    }
                    tab.scratch = Scratch::new(&text);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    failed = Some(format!("scratch not loaded: {}: {error}", path.display()));
                }
            }
        }
        failed
    }

    /// Write one tab's pad, making the directory if it is not there yet.
    pub fn save(&self, tab: &Tab) -> Result<()> {
        let Some(path) = self.path(&tab.name) else {
            return Ok(());
        };
        let directory = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let text = tab.scratch.text();
        // Beside it and then renamed over it, so a run killed half way
        // through a save leaves the last whole pad rather than part of this.
        let partial = path.with_extension("sql.partial");
        std::fs::write(&partial, format!("{text}\n"))
            .with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::two_tabs;

    #[test]
    fn a_pad_comes_back_the_way_it_was_left_through_a_state_directory() {
        let directory = tempfile::tempdir().expect("a directory");
        let store = Store::new(Some(directory.path().to_path_buf()));
        let mut app = two_tabs();
        app.tabs[0].scratch.set_text("select 1\nfrom dual");
        store.save(&app.tabs[0]).expect("a written pad");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("scratch/local-mssql.sql"))
                .expect("the file"),
            "select 1\nfrom dual\n"
        );

        let mut restored = two_tabs();
        assert_eq!(store.restore(&mut restored), None);
        assert_eq!(restored.tabs[0].scratch.text(), "select 1\nfrom dual");
        assert!(
            !restored.tabs[0].scratch.modified(),
            "a pad off the disk is not ahead of it"
        );
        assert_eq!(restored.tabs[1].scratch.text(), "", "one file per tab");
    }

    #[test]
    fn a_pad_that_is_not_utf8_still_comes_back_rather_than_being_saved_over() {
        let directory = tempfile::tempdir().expect("a directory");
        let store = Store::new(Some(directory.path().to_path_buf()));
        let path = store.path("local-mssql").expect("a path");
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(&path, b"select 'Zo\xeb'\nfrom t\n").expect("a Latin-1 pad");
        let mut app = two_tabs();
        let said = store.restore(&mut app).expect("a word about it");
        assert!(said.contains("not UTF-8"), "{said}");
        assert_eq!(app.tabs[0].scratch.text(), "select 'Zo\u{fffd}'\nfrom t");
        store.save(&app.tabs[0]).expect("saved");
        assert!(
            !path.with_extension("sql.partial").exists(),
            "renamed into place"
        );
    }

    #[test]
    fn a_connection_named_like_a_path_is_never_saved_outside_the_state_directory() {
        let store = Store::new(Some(PathBuf::from("/state")));
        assert_eq!(
            store.path("local mssql"),
            Some(PathBuf::from("/state/scratch/local mssql.sql")),
            "a space is part of a name, not a reason to rename the file"
        );
        for (name, file) in [
            ("../../etc/passwd", "%2E.%2F..%2Fetc%2Fpasswd.sql"),
            ("prod/reporting", "prod%2Freporting.sql"),
            (".hidden", "%2Ehidden.sql"),
        ] {
            assert_eq!(
                store.path(name),
                Some(PathBuf::from("/state/scratch").join(file)),
                "{name} is one file in the directory, and still saved"
            );
        }
        assert_eq!(store.path(""), None);
        assert_eq!(Store::new(None).path("local-mssql"), None);
    }
}
