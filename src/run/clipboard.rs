//! The system clipboard, through whichever command-line tool this machine
//! has. No crate: every desktop already ships a pair of programs that read
//! and write it, and finding them on `PATH` is a dozen lines.
//!
//! OSC 52 still goes out on every copy (see [`super::Driver`]), because it
//! is the only thing that reaches the clipboard of the machine a person is
//! sitting at over SSH. It cannot be read back, so a paste needs the tool.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a clipboard tool gets before it is killed. A tool that has
/// never answered in that long is one waiting for a display that is gone.
pub const TIMEOUT: Duration = Duration::from_millis(500);

/// Where the clipboard is, as far as this run knows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Clipboard {
    /// No tool found: copies go out as OSC 52 only, and Ctrl-V pastes what
    /// sql-bench itself last copied.
    #[default]
    None,
    /// The command a copy is piped into, and the one a paste reads.
    Tool {
        copy: Vec<&'static str>,
        paste: Vec<&'static str>,
    },
    /// A replay's clipboard: `clipboard <text>` puts text on it, a copy
    /// writes it and Ctrl-V reads it, and no program is ever run.
    Fake(String),
}

impl Clipboard {
    /// The first tool this machine has, in the order a desktop would pick:
    /// Wayland's own, then X11's two, then macOS's, then Windows' from WSL.
    /// `var` reads the environment and `mac` says what the OS is, so a test
    /// can hand it any machine it likes.
    pub fn detect(var: impl Fn(&str) -> Option<String>, mac: bool) -> Self {
        let path = var("PATH").unwrap_or_default();
        let has = |name: &str| {
            std::env::split_paths(&path).any(|directory| is_file(&directory.join(name)))
        };
        let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
        let tool = |copy: &[&'static str], paste: &[&'static str]| Self::Tool {
            copy: copy.to_vec(),
            paste: paste.to_vec(),
        };
        if set("WAYLAND_DISPLAY") && has("wl-copy") && has("wl-paste") {
            return tool(&["wl-copy"], &["wl-paste", "--no-newline"]);
        }
        if set("DISPLAY") && has("xclip") {
            return tool(
                &["xclip", "-selection", "clipboard"],
                &["xclip", "-selection", "clipboard", "-o"],
            );
        }
        if set("DISPLAY") && has("xsel") {
            return tool(&["xsel", "-b", "-i"], &["xsel", "-b", "-o"]);
        }
        if mac && has("pbcopy") && has("pbpaste") {
            return tool(&["pbcopy"], &["pbpaste"]);
        }
        if (set("WSL_DISTRO_NAME") || set("WSL_INTEROP"))
            && has("clip.exe")
            && has("powershell.exe")
        {
            // ponytail: clip.exe reads the console code page, so text past
            // ASCII may arrive mangled on Windows' side; a UTF-16 conversion
            // before the pipe is the fix if anyone copies names like that.
            return tool(
                &["clip.exe"],
                // `Write` and not `Get-Clipboard` alone, which ends the text
                // with a line break that is not on the clipboard.
                &[
                    "powershell.exe",
                    "-NoProfile",
                    "-Command",
                    "[Console]::Out.Write((Get-Clipboard -Raw))",
                ],
            );
        }
        Self::None
    }
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.is_file())
}

/// Pipe `text` into the copy tool on a thread of its own, so a big copy
/// never holds up the loop. Best effort: a tool that fails leaves OSC 52
/// and the app's own clipboard, and nobody is told.
pub fn write(argv: Vec<&'static str>, text: String) {
    let _ = std::thread::Builder::new()
        .name("copy".to_owned())
        .spawn(move || {
            let Some((program, args)) = argv.split_first() else {
                return;
            };
            let Ok(mut child) = Command::new(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut input) = child.stdin.take() {
                // ponytail: a tool that stops reading with more than a pipe
                // of text still to come holds this thread until it exits; a
                // timer that kills it is the upgrade if one ever does.
                let _ = input.write_all(text.as_bytes());
            }
            // Waited on here, dropped stdin and all, so no zombie is left.
            finish(&mut child, Instant::now() + TIMEOUT);
        });
}

/// What the paste tool prints, or `None` for a tool that failed, printed
/// nothing, or took longer than `timeout` — which it is killed for. Blocks
/// for up to `timeout`, so it runs on a worker and never on the loop.
#[must_use]
pub fn read(argv: &[&str], timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let (program, args) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut output = child.stdout.take()?;
    // Read on a thread of its own, because a tool with more to say than a
    // pipe holds does not exit until somebody reads it.
    let (sender, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = sender.send(output.read_to_end(&mut bytes).map(|_| bytes));
    });
    let bytes = received.recv_timeout(timeout);
    let status = finish(&mut child, deadline)?;
    let bytes = bytes.ok()?.ok()?;
    if !status.success() || bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Wait for the child until `deadline`, then kill it, and reap it either
/// way. `None` is a child that had to be killed.
fn finish(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A machine whose `PATH` is one directory holding these programs, with
    /// these variables set.
    fn machine(tools: &[&str], vars: &[(&str, &str)], mac: bool) -> Clipboard {
        let bin = tempfile::tempdir().expect("a directory");
        for tool in tools {
            std::fs::write(bin.path().join(tool), "").expect("a tool");
        }
        let mut env: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        env.insert("PATH".to_owned(), bin.path().display().to_string());
        Clipboard::detect(|name| env.get(name).cloned(), mac)
    }

    fn first(clipboard: &Clipboard) -> &str {
        match clipboard {
            Clipboard::Tool { copy, .. } => copy[0],
            other => panic!("no tool: {other:?}"),
        }
    }

    #[test]
    fn the_tool_is_the_first_this_machine_has_and_can_use() {
        let everything = ["wl-copy", "wl-paste", "xclip", "xsel", "pbcopy", "pbpaste"];
        let wayland = [("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")];
        assert_eq!(
            machine(&everything, &wayland, false),
            Clipboard::Tool {
                copy: vec!["wl-copy"],
                paste: vec!["wl-paste", "--no-newline"],
            }
        );
        let x11 = [("DISPLAY", ":0")];
        assert_eq!(first(&machine(&everything, &x11, false)), "xclip");
        assert_eq!(first(&machine(&["xsel"], &x11, false)), "xsel");
        assert_eq!(
            machine(&["xclip", "xsel"], &[], false),
            Clipboard::None,
            "X11's tools with no display to talk to"
        );
        assert_eq!(
            machine(&["wl-copy"], &wayland, false),
            Clipboard::None,
            "half of a pair is no pair"
        );
        assert_eq!(first(&machine(&everything, &[], true)), "pbcopy");
        assert_eq!(machine(&["pbcopy", "pbpaste"], &[], false), Clipboard::None);
        let wsl = [("WSL_DISTRO_NAME", "Ubuntu")];
        assert_eq!(
            first(&machine(&["clip.exe", "powershell.exe"], &wsl, false)),
            "clip.exe"
        );
        assert_eq!(
            machine(&["clip.exe", "powershell.exe"], &[], false),
            Clipboard::None,
            "not WSL"
        );
        assert_eq!(machine(&[], &wayland, true), Clipboard::None);
    }

    #[test]
    fn a_read_is_what_the_tool_printed_and_none_for_a_failure_or_a_hang() {
        assert_eq!(
            read(&["printf", "select 1\\nfrom t"], TIMEOUT).as_deref(),
            Some("select 1\nfrom t")
        );
        assert_eq!(read(&["printf", ""], TIMEOUT), None, "nothing on it");
        assert_eq!(read(&["false"], TIMEOUT), None, "a tool that failed");
        assert_eq!(read(&["no-such-clipboard-tool"], TIMEOUT), None);
        let started = Instant::now();
        assert_eq!(read(&["sleep", "5"], Duration::from_millis(50)), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "killed at the timeout, not waited for"
        );
    }
}
