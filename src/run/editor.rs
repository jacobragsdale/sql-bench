//! The `$EDITOR` round trip: the pad goes out to a file, whatever the editor
//! saves comes back. The terminal is handed over and taken back by
//! [`crate::run`], because the editor draws over whatever is on the screen.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The editor to hand the pad to: `$VISUAL`, then `$EDITOR`, and nothing
/// after that — a workbench that opened `vi` on a person who never asked for
/// it would be a workbench they could not get out of.
///
/// The variable is split on whitespace, so `code --wait` runs `code` with
/// `--wait` and the file after it; one that is empty counts as unset.
#[must_use]
pub fn command() -> Option<Vec<String>> {
    command_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())
}

#[must_use]
pub fn command_from(visual: Option<String>, editor: Option<String>) -> Option<Vec<String>> {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(|raw| {
            raw.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .find(|parts| !parts.is_empty())
}

/// Writes the pad out as `.sql`, runs the editor on it, and gives back what
/// was saved. The file goes whether or not the editor works out, and is
/// taken away again either way.
pub fn round_trip(directory: &Path, tab: usize, sql: &str, command: &[String]) -> Result<String> {
    let path = file(directory, tab);
    create(&path, sql).with_context(|| format!("writing {}", path.display()))?;
    let outcome = run(command, &path).and_then(|()| {
        std::fs::read_to_string(&path).with_context(|| format!("reading {} back", path.display()))
    });
    let _ = std::fs::remove_file(&path);
    outcome
}

/// A name of this process's own, so two sql-benches editing at once do not
/// hand each other their pads, with the clock in it so that nobody else on a
/// shared `/tmp` can plant a file under the name before it is written.
fn file(directory: &Path, tab: usize) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    directory.join(format!(
        "sql-bench-{}-{tab}-{nanos:09}.sql",
        std::process::id()
    ))
}

/// A file that did not exist, readable by nobody else: a pad can hold a
/// password, and a name that is already there — a symlink someone left in
/// `/tmp` — is refused rather than followed.
fn create(path: &Path, sql: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(sql.as_bytes())
}

/// Runs the editor and waits for it. It owns the terminal while it runs, so
/// everything it draws goes straight to the screen.
fn run(command: &[String], path: &Path) -> Result<()> {
    let (program, arguments) = command.split_first().context("no editor to run")?;
    let status = Command::new(program)
        .args(arguments)
        .arg(path)
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_wins_over_editor_and_an_empty_one_is_no_editor_at_all() {
        let owned = |text: &str| Some(text.to_owned());
        assert_eq!(
            command_from(owned("code --wait"), owned("vi")),
            Some(vec!["code".to_owned(), "--wait".to_owned()])
        );
        assert_eq!(
            command_from(None, owned("nano")),
            Some(vec!["nano".to_owned()])
        );
        assert_eq!(
            command_from(owned("   "), owned("nano")),
            Some(vec!["nano".to_owned()])
        );
        assert_eq!(command_from(None, None), None);
        assert_eq!(command_from(owned(""), owned("")), None);
    }

    #[test]
    fn what_the_editor_saves_is_what_comes_back_and_the_file_goes_away() {
        let directory = tempfile::tempdir().expect("a directory");
        let editor = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf 'select 2;' > \"$0\"".to_owned(),
        ];
        let edited = round_trip(directory.path(), 0, "select 1;", &editor).expect("an edit");
        assert_eq!(edited, "select 2;");
        let left = std::fs::read_dir(directory.path())
            .expect("a listing")
            .count();
        assert_eq!(left, 0, "the temp file is gone");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_new_and_private_and_a_planted_one_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("a directory");
        let editor = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "stat -c %a \"$0\" > \"$0.mode\"".to_owned(),
        ];
        round_trip(directory.path(), 0, "select 1;", &editor).expect("an edit");
        let mode = std::fs::read_dir(directory.path())
            .expect("a listing")
            .find_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
            .expect("the mode the editor saw");
        assert_eq!(mode.trim(), "600");

        let planted = directory.path().join("planted");
        std::fs::write(&planted, "").expect("a file");
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o644))
            .expect("permissions");
        assert!(create(&planted, "select 1;").is_err(), "an existing name");
    }

    #[test]
    fn an_editor_that_fails_is_an_error_and_not_an_emptied_pad() {
        let directory = tempfile::tempdir().expect("a directory");
        let editor = vec!["sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()];
        let error = round_trip(directory.path(), 1, "select 1;", &editor).expect_err("a failure");
        assert!(format!("{error:#}").contains("exited with"), "{error:#}");

        let missing = vec!["definitely-not-an-editor".to_owned()];
        let error = round_trip(directory.path(), 1, "select 1;", &missing).expect_err("no editor");
        assert!(format!("{error:#}").contains("could not run"), "{error:#}");
    }
}
