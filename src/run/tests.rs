//! The loop, driven on a `TestBackend` the way T3.2's replay will drive it.

use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::app::tests::{key, two_connections, two_tabs};

/// A driver with the two tabs' connections behind it, connected to none,
/// and nowhere to save a scratch pad: a test writes no state of its own.
fn driver() -> Driver {
    let mut driver = Driver::new(Theme::from_env(), &two_connections());
    driver.keep_scratch_in(crate::run::state::Store::new(None));
    driver
}

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
    run_loop(&mut terminal, &mut app, input, trace, &mut driver(), None).expect("the loop");
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
        &mut driver(),
        None,
    )
    .expect("the loop");
    assert!(app.shell.should_quit);
    assert_eq!(app.shell.active_tab, 1);
    let row: String = (0..120)
        .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
        .collect();
    assert_eq!(
        row.trim_end(),
        format!(" 1 local-mssql ○  2 local-oracle ○{:79}? Help", "")
    );
    let footer: String = (0..120)
        .map(|x| terminal.backend().buffer()[(x, 39)].symbol())
        .collect();
    assert!(footer.trim_end().ends_with("○ disconnected"), "{footer}");
}

#[test]
fn the_loop_ends_when_the_input_is_exhausted_even_without_a_quit() {
    let app = drive(
        &mut Keys::new(&["Shift-Tab", "Shift-Tab"]),
        &Trace::new(None),
    );
    assert!(!app.shell.should_quit);
    assert_eq!(app.shell.focus, crate::app::Focus::Scratch);
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
    // With the fraction: a draw takes well under a millisecond, and a number
    // rounded to whole ones cannot be held to a budget of five.
    assert!(
        value.contains('.'),
        "draw_ms={value} is a whole millisecond"
    );
    value.parse::<f64>().expect("milliseconds");
}

#[test]
fn a_key_is_a_frame_and_a_traced_run_says_how_long_it_took_to_draw() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("trace.tsv");
    drive(
        &mut Keys::new(&["Tab", "Shift-Tab", "q"]),
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

#[test]
fn osc_52_carries_the_selection_as_base64() {
    // The three lengths that matter: a multiple of three, and each of the
    // two paddings.
    assert_eq!(super::base64(b"sql"), "c3Fs");
    assert_eq!(super::base64(b"select 1"), "c2VsZWN0IDE=");
    assert_eq!(super::base64(b"select 12"), "c2VsZWN0IDEy");
    assert_eq!(super::base64(b"s"), "cw==");
    assert_eq!(super::base64(b""), "");
}

/// One export through the loop's own `act`, and what it wrote.
fn exported(app: &mut App, name: &str, directory: &std::path::Path) -> String {
    let path = directory.join(name);
    driver()
        .act(
            &mut terminal(),
            app,
            Action::Export {
                tab: 0,
                path: path.to_string_lossy().into_owned(),
            },
        )
        .expect("the export");
    std::fs::read_to_string(&path).expect("the exported file")
}

#[test]
fn an_export_writes_the_same_bytes_the_headless_formatters_do() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = two_tabs();
    app.tabs[0].results = crate::app::tests::filled(3, 4);
    let columns = app.tabs[0].results.columns().to_vec();
    let rows = app.tabs[0].results.rows().to_vec();

    assert_eq!(
        exported(&mut app, "rows.csv", directory.path()),
        crate::export::csv(&columns, &rows)
    );
    assert_eq!(app.shell.status, {
        let path = directory.path().join("rows.csv");
        format!("exported 3 rows to {}", path.display())
    });
    // The extension is what picks the format, whatever its case.
    assert_eq!(
        exported(&mut app, "rows.JSON", directory.path()),
        crate::export::json(&columns, &rows)
    );
    assert!(app.shell.error.is_none());
}

#[test]
fn an_export_that_cannot_be_written_says_so_in_the_footer() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = two_tabs();
    app.tabs[0].results = crate::app::tests::filled(1, 2);
    let missing = directory.path().join("no").join("such").join("out.csv");
    driver()
        .act(
            &mut terminal(),
            &mut app,
            Action::Export {
                tab: 0,
                path: missing.to_string_lossy().into_owned(),
            },
        )
        .expect("the export");
    let error = app.shell.error.expect("the failure in the footer");
    assert!(error.starts_with("export failed: "), "{error}");
    assert!(app.shell.status.is_empty());
}

#[test]
fn a_leading_tilde_is_the_home_directory_and_nothing_else_is_expanded() {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    assert_eq!(
        home.map(|home| home.join("sql-bench.csv")),
        Some(super::expand("~/sql-bench.csv"))
    );
    assert_eq!(
        super::expand("/tmp/a.csv"),
        std::path::Path::new("/tmp/a.csv")
    );
    assert_eq!(super::expand("out.csv"), std::path::Path::new("out.csv"));
    // Not a home directory: `~other` is somebody else's, and this is not a
    // shell.
    assert_eq!(
        super::expand("~other/a.csv"),
        std::path::Path::new("~other/a.csv")
    );
}

/// Everything already queued, as a terminal delivers a burst: the loop's
/// drain finds each event waiting behind the one before it.
struct Burst(std::collections::VecDeque<Event>);

impl InputSource for Burst {
    fn next(&mut self, _timeout: Duration) -> Result<Option<Event>> {
        Ok(self.0.pop_front())
    }
}

fn mouse(kind: crossterm::event::MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    })
}

/// How many frames a run of these events draws, the first one included.
fn frames_for(events: Vec<Event>) -> usize {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("trace.tsv");
    drive(&mut Burst(events.into()), &Trace::new(Some(path.clone())));
    std::fs::read_to_string(&path)
        .expect("a trace file")
        .lines()
        .filter(|line| line.split('\t').nth(1) == Some("frame"))
        .count()
}

#[test]
fn the_pointer_resting_on_one_cell_costs_one_frame_and_a_press_costs_none() {
    use crossterm::event::{MouseButton, MouseEventKind};
    // The second tab's label: its hover is the one frame.
    let resting = vec![mouse(MouseEventKind::Moved, 20, 0); 1000];
    assert_eq!(frames_for(resting), 2, "the first frame and the hover");
    let pressed = vec![mouse(MouseEventKind::Down(MouseButton::Left), 20, 0)];
    assert_eq!(
        frames_for(pressed),
        1,
        "a press is not a click until it is let go"
    );
}

#[test]
fn a_click_behind_a_key_that_changed_the_layout_lands_on_the_new_layout() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let left = MouseButton::Left;
    // `?` opens the help, and x 20 of the tab bar is the second tab on the
    // frame before it and outside the help on the frame after.
    let mut burst = Burst(
        vec![
            Event::Key(key("?")),
            mouse(MouseEventKind::Down(left), 20, 0),
            mouse(MouseEventKind::Up(left), 20, 0),
        ]
        .into(),
    );
    let app = drive(&mut burst, &Trace::new(None));
    assert!(!app.shell.help, "the click beside the help closed it");
    assert_eq!(app.shell.active_tab, 0, "and reached nothing under it");
}
