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
    /// a file that cannot be read is not worth stopping a run over: the
    /// message goes to the footer once the app is up.
    pub fn restore(&self, app: &mut App) -> Option<String> {
        let mut failed = None;
        for tab in &mut app.tabs {
            let Some(path) = self.path(&tab.name) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => tab.scratch = Scratch::new(&text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => failed = Some(format!("{}: {error}", path.display())),
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
        std::fs::write(&path, format!("{text}\n"))
            .with_context(|| format!("writing {}", path.display()))
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
    fn a_connection_named_like_a_path_is_never_saved_outside_the_state_directory() {
        let store = Store::new(Some(PathBuf::from("/state")));
        assert_eq!(
            store.path("local mssql"),
            Some(PathBuf::from("/state/scratch/local mssql.sql")),
            "a space is part of a name, not a reason to rename the file"
        );
        for name in ["../../etc/passwd", "a/b", ".hidden", ""] {
            assert_eq!(store.path(name), None, "{name} is not a file name");
        }
        assert_eq!(Store::new(None).path("local-mssql"), None);
    }
}
