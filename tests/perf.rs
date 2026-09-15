//! The budgets `docs/DESIGN.md` sets, asserted rather than written down, so
//! a regression is a red test and not a number nobody re-read.
//!
//! `cargo test --release -- --ignored`. Ignored because they are timings:
//! they belong in a release build on a machine that is not doing something
//! else, and `cargo test` is neither. No database either — the rows are made
//! up here, which is the point: what is measured is the draw and the loop.
//!
//! Every assertion allows twice the budget. A machine under load is the
//! usual reason a timing test goes red, and a budget missed by a factor of
//! two is a regression rather than a busy runner.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use sql_bench::app::{App, Focus};
use sql_bench::config::{self, Config};
use sql_bench::db::model::{Cell, Column, QueryEvent};
use sql_bench::run::state::Store;
use sql_bench::run::{Driver, InputSource};
use sql_bench::trace::Trace;
use sql_bench::ui::theme::Theme;

/// Startup to the first frame.
const STARTUP: Duration = Duration::from_millis(50);
/// A key to the frame that answers it.
const KEY_TO_FRAME: Duration = Duration::from_millis(16);
/// How much dearer a draw of ten times the rows may be. The budget is that
/// it costs nothing, which is a ratio and not a duration.
const ROWS_ALLOWANCE: f64 = 0.20;
/// What every budget above is multiplied by before it is asserted.
const MARGIN: u32 = 2;

/// The size every frame in `docs/` is drawn at.
const SIZE: (u16, u16) = (120, 40);

/// The committed two-connection config, which is what a run of the real
/// thing starts from. Nothing here connects.
fn config() -> Config {
    config::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml"))
        .expect("the committed config.local.toml")
}

fn driver(config: &Config) -> Driver {
    let mut driver = Driver::new(Theme::new(true), config);
    // A test writes no scratch pad of its own and reads nobody else's.
    driver.keep_scratch_in(Store::new(None));
    driver.without_terminal();
    driver
}

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).expect("a test terminal")
}

/// One key per call, then nothing — the same bargain the replay's queue
/// strikes, so one turn is one key and one frame.
struct Keys(std::vec::IntoIter<Event>);

impl Keys {
    fn of(code: KeyCode, count: usize) -> Self {
        let event = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        Self(vec![event; count].into_iter())
    }
}

impl InputSource for Keys {
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        if timeout.is_zero() {
            return Ok(None);
        }
        Ok(self.0.next())
    }
}

/// An app of one tab with `rows` rows of the seeded `events` table's shape
/// on screen, which is what a draw is measured over.
fn app_with_rows(rows: usize) -> App {
    let mut app = App::new(&config());
    app.shell.focus = Focus::Results;
    let results = &mut app.tabs.first_mut().expect("a tab").results;
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(
        ["id", "customer_id", "kind", "amount", "at", "note"]
            .into_iter()
            .map(|name| Column {
                name: name.to_owned(),
                type_name: "nvarchar(64)".to_owned(),
            })
            .collect(),
    ));
    // In batches the size the drivers report in, so the widths are measured
    // the way a real scan measures them.
    for batch in 0..rows / 500 {
        results.apply(QueryEvent::Rows(
            (0..500)
                .map(|row| {
                    let id = batch * 500 + row;
                    vec![
                        Cell::Int(id as i64),
                        Cell::Int((id % 1000) as i64),
                        Cell::Text(format!("kind-{}", id % 7)),
                        Cell::Decimal(format!("{}.{:02}", id % 10_000, id % 100)),
                        Cell::DateTime("2026-09-15 12:00:00".to_owned()),
                        Cell::Text(format!("note for row {id}, long enough to be cut")),
                    ]
                })
                .collect(),
        ));
    }
    results.apply(QueryEvent::Done {
        rows,
        truncated: false,
        connect_ms: 0,
        first_row_ms: 0,
        total_ms: 0,
    });
    app
}

/// The middle of a sorted list of timings. A median rather than a mean
/// because one scheduling hiccup should not decide a budget.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// The sample at `ceil(p/100 * n)`, counting from one — the same
/// nearest-rank percentile `bench` and `scripts/perf.sh` report.
fn percentile(mut samples: Vec<Duration>, p: usize) -> Duration {
    samples.sort_unstable();
    let rank = (samples.len() * p).div_ceil(100).max(1);
    samples[rank - 1]
}

/// How long `count` draws of this app take, one each.
fn draws(app: &App, count: usize) -> Vec<Duration> {
    let theme = Theme::new(true);
    let mut terminal = terminal();
    (0..count)
        .map(|_| {
            let at = Instant::now();
            terminal
                .draw(|frame| sql_bench::ui::render(frame, app, &theme))
                .expect("a draw");
            at.elapsed()
        })
        .collect()
}

#[test]
#[ignore = "a timing: cargo test --release -- --ignored"]
fn startup_reaches_the_first_frame_inside_the_budget() {
    let config = config();
    // Everything `run` does between the terminal being taken and the first
    // frame being on it: the app, the pads, the driver and one turn.
    let at = Instant::now();
    let mut app = App::new(&config);
    let mut driver = driver(&config);
    driver.restore_scratch(&mut app);
    let mut terminal = terminal();
    driver
        .turn(
            &mut terminal,
            &mut app,
            &mut Keys::of(KeyCode::Char('q'), 1),
            &Trace::new(None),
        )
        .expect("a turn");
    let elapsed = at.elapsed();
    assert!(
        elapsed < STARTUP * MARGIN,
        "startup took {elapsed:?}, and the budget is {STARTUP:?}"
    );
}

#[test]
#[ignore = "a timing: cargo test --release -- --ignored"]
fn a_draw_costs_the_window_and_never_the_scan() {
    let ten_thousand = median(draws(&app_with_rows(10_000), 200));
    let hundred_thousand = median(draws(&app_with_rows(100_000), 200));
    assert!(
        hundred_thousand < KEY_TO_FRAME * MARGIN,
        "a draw of 100,000 rows took {hundred_thousand:?}, and the budget is {KEY_TO_FRAME:?}"
    );
    let allowed = ten_thousand.as_secs_f64() * (1.0 + ROWS_ALLOWANCE * f64::from(MARGIN));
    assert!(
        hundred_thousand.as_secs_f64() <= allowed,
        "10,000 rows drew in {ten_thousand:?} and 100,000 in {hundred_thousand:?}, \
         which is more than the {:.0}% a draw may cost for ten times the rows",
        ROWS_ALLOWANCE * 100.0
    );
}

#[test]
#[ignore = "a timing: cargo test --release -- --ignored"]
fn a_key_reaches_the_frame_inside_the_budget_with_a_full_grid() {
    let config = config();
    let mut app = app_with_rows(100_000);
    let mut driver = driver(&config);
    let mut terminal = terminal();
    let trace = Trace::new(None);
    // The grid's movement keys, which are the ones that move the window a
    // draw has to format: down one, a page down, and back to the top.
    let mut keys = Keys(
        [
            KeyCode::Char('j'),
            KeyCode::PageDown,
            KeyCode::Char('G'),
            KeyCode::Char('g'),
        ]
        .into_iter()
        .cycle()
        .take(200)
        .map(|code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .collect::<Vec<_>>()
        .into_iter(),
    );
    let mut turns = Vec::new();
    loop {
        let at = Instant::now();
        let going = driver
            .turn(&mut terminal, &mut app, &mut keys, &trace)
            .expect("a turn");
        turns.push(at.elapsed());
        if !going {
            break;
        }
    }
    assert!(turns.len() > 100, "only {} turns were taken", turns.len());
    let p95 = percentile(turns, 95);
    assert!(
        p95 < KEY_TO_FRAME * MARGIN,
        "key to frame p95 was {p95:?}, and the budget is {KEY_TO_FRAME:?}"
    );
}
