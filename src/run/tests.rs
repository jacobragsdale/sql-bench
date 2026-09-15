//! The loop, driven on a `TestBackend` the way T3.2's replay will drive it.

use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::app::tests::{key, two_tabs};

/// The keys of a replay: each one once, then nothing, for ever.
struct Keys(std::vec::IntoIter<Event>);

impl Keys {
    fn new(specs: &[&str]) -> Self {
        Self(
            specs
                .iter()
                .map(|spec| Event::Key(key(spec)))
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }
}

impl InputSource for Keys {
    /// One key per turn: the zero-timeout call is the loop draining what is
    /// already queued, and a replay never has anything queued behind the key
    /// it just handed over.
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        if timeout.is_zero() {
            return Ok(None);
        }
        Ok(self.0.next())
    }
}

/// A terminal nobody types at: every call waits out its timeout and comes
/// back empty.
struct Idle;

impl InputSource for Idle {
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        std::thread::sleep(timeout);
        Ok(None)
    }
}

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(120, 40)).expect("a test terminal")
}

fn drive(input: &mut dyn InputSource, trace: &Trace) -> App {
    let mut app = two_tabs();
    let mut terminal = terminal();
    run_loop(&mut terminal, &mut app, input, trace).expect("the loop");
    app
}

#[test]
fn a_run_draws_the_layout_and_q_ends_it() {
    let mut app = two_tabs();
    let mut terminal = terminal();
    run_loop(
        &mut terminal,
        &mut app,
        &mut Keys::new(&["Ctrl-T", "q"]),
        &Trace::new(None),
    )
    .expect("the loop");
    assert!(app.shell.should_quit);
    assert_eq!(app.shell.active_tab, 1);
    let row: String = (0..120)
        .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
        .collect();
    assert_eq!(row.trim_end(), " 1 local-mssql ○  2 local-oracle ○");
    let footer: String = (0..120)
        .map(|x| terminal.backend().buffer()[(x, 39)].symbol())
        .collect();
    assert!(footer.trim_end().ends_with("○ disconnected"), "{footer}");
}

#[test]
fn the_loop_ends_when_the_input_is_exhausted_even_without_a_quit() {
    let app = drive(&mut Keys::new(&["Tab", "Tab"]), &Trace::new(None));
    assert!(!app.shell.should_quit);
    assert_eq!(app.shell.focus, crate::app::Focus::Results);
}

#[test]
fn an_idle_app_draws_one_frame_and_nothing_more() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("trace.tsv");
    let started = Instant::now();
    drive(&mut Idle, &Trace::new(Some(path.clone())));
    // Two empty waits, which is what exhaustion is, at one idle timeout each.
    assert!(
        started.elapsed() >= IDLE_TIMEOUT * 2,
        "the loop did not wait"
    );

    let written = std::fs::read_to_string(&path).expect("a trace file");
    let frames: Vec<&str> = written
        .lines()
        .filter(|line| line.split('\t').nth(1) == Some("frame"))
        .collect();
    assert_eq!(
        frames.len(),
        1,
        "one frame, and only the first one:\n{written}"
    );
    let draw = frames[0].split('\t').nth(2).expect("a draw_ms field");
    let (name, value) = draw.split_once('=').expect("k=v");
    assert_eq!(name, "draw_ms");
    value.parse::<u64>().expect("milliseconds");
}

#[test]
fn a_key_is_a_frame_and_a_traced_run_says_how_long_it_took_to_draw() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("trace.tsv");
    drive(
        &mut Keys::new(&["Tab", "Tab", "q"]),
        &Trace::new(Some(path.clone())),
    );
    let written = std::fs::read_to_string(&path).expect("a trace file");
    let frames = written
        .lines()
        .filter(|line| line.split('\t').nth(1) == Some("frame"))
        .count();
    // The first frame, and then one for each key: the quit is not drawn.
    assert_eq!(frames, 3, "{written}");
}
