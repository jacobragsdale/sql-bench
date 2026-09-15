//! The run: claiming the terminal, waiting for input, drawing when something
//! changed, carrying out what the app asked for, and the trace file. The only
//! module that blocks.

pub mod editor;
pub mod replay;
pub mod runtime;
pub mod state;

#[cfg(test)]
mod tests;

pub use replay::replay;
pub use runtime::{Pending, Runtime, startup_tabs};

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::results::grouped;
use crate::app::{Action, App, SPIN_EVERY};
use crate::cli::Cli;
use crate::config::Config;
use crate::export;
use crate::run::state::Store;
use crate::trace::Trace;
use crate::ui;
use crate::ui::theme::Theme;

/// How long queued input may be handled before the screen is painted again.
/// A key held down arrives about as fast as it can be handled, so without a
/// limit the drain never empties and nothing is redrawn until it comes up.
const DRAIN_LIMIT: Duration = Duration::from_millis(50);

/// How long the loop waits for input before taking a turn with none. While
/// something is connecting the wait is [`SPIN_EVERY`] instead, because the
/// spinner has to move even when nobody is typing.
const IDLE_TIMEOUT: Duration = Duration::from_millis(250);

/// How long the loop waits for input while a pad is waiting to be saved.
/// Short enough that the save lands about when it was promised, long enough
/// that a run nobody is typing at costs a wake-up every tenth of a second
/// and only for the half second after the last key.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// A turn slower than this is worth a `turn` line in the trace. Anything
/// faster is the loop working as intended and not worth the write.
const SLOW_TURN: Duration = Duration::from_millis(30);

/// Where the loop's events come from. Two implementations exist by design:
/// the terminal, and T3.2's replay of a key file — which is the whole reason
/// the loop is generic over its backend as well.
pub trait InputSource {
    /// The next event, or `None` if `timeout` passed with nothing to report.
    ///
    /// Returning `None` on two consecutive calls means "exhausted" and ends
    /// the loop (see [`run_loop`]), so a source that is still live must not
    /// come up empty twice in a row.
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>>;

    /// Whether more events can still arrive. A source that says no is over,
    /// and the loop stops keeping it alive for a spinner nobody can see —
    /// otherwise a dead terminal under `--connect` spins the loop for ever.
    fn live(&self) -> bool {
        true
    }
}

/// The keyboard, through crossterm, read on a thread of its own.
///
/// The loop waits on the channel and never on crossterm, which is what keeps
/// a terminal it cannot read from becoming a terminal it cannot give back:
/// `event::read` waiting for bytes that never come blocks its own thread,
/// and a read that fails — the pty's other end closed, the window shut —
/// drops the sender and ends the loop cleanly instead of surfacing as an
/// error raised from inside a redraw.
///
/// A run whose own standard input is not a terminal gets no reader at all.
/// crossterm would quietly read `/dev/tty` instead, which is how `sql-bench
/// </dev/null` ends up holding raw mode and the alternate screen with no key
/// left that could quit it.
// ponytail: a pty that stays open and is never written to again is still
// indistinguishable from a terminal nobody is typing at, and both wait. A
// program in raw mode cannot tell them apart; only the input side closing,
// or this run's own stdin not being a terminal, is a fact rather than a
// guess.
#[derive(Debug)]
pub struct TerminalInput {
    events: std::sync::mpsc::Receiver<Event>,
    idle: bool,
    live: bool,
}

impl Default for TerminalInput {
    fn default() -> Self {
        let (sender, events) = std::sync::mpsc::channel();
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            std::thread::spawn(move || {
                // Ends on a read error, and on the loop hanging up — which
                // is the app quitting and the receiver going with it.
                while let Ok(event) = event::read() {
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            events,
            idle: false,
            live: true,
        }
    }
}

impl InputSource for TerminalInput {
    /// One wait, and then — having already come up empty once — however long
    /// it takes: a terminal nobody is typing at is idle, never exhausted, and
    /// two empty waits in a row would end the loop.
    ///
    /// The exception is a timeout shorter than the idle one, which is the
    /// loop saying it wants the turn back on time because something is
    /// animating; [`Driver::turn`] does not count those empties.
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        use std::sync::mpsc::RecvTimeoutError;
        loop {
            match self.events.recv_timeout(timeout) {
                Ok(event) => {
                    self.idle = false;
                    return Ok(Some(event));
                }
                // The reader is gone: no key will ever arrive again, so the
                // loop is over rather than idle.
                Err(RecvTimeoutError::Disconnected) => {
                    self.live = false;
                    return Ok(None);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !std::mem::replace(&mut self.idle, true) || timeout < IDLE_TIMEOUT {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn live(&self) -> bool {
        self.live
    }
}

/// Opens the TUI on this terminal and gives it back when the app quits.
///
/// `panic_after` is `--panic-after-ms`, which only a debug build has: it is
/// how QA gets a panic out of a loop that is holding the terminal.
pub fn run(config: &Config, args: &Cli, panic_after: Option<Duration>) -> Result<()> {
    let mut app = App::new(config);
    let startup = startup_tabs(config, args)?;
    // `try_init` takes raw mode and the alternate screen and installs the
    // panic hook that gives both back; `Restore` is the same for every other
    // way out of this function.
    let mut terminal = ratatui::try_init()
        .inspect_err(|_| ratatui::restore())
        .context("failed to take the terminal")?;
    let _restore = Restore;
    // A pasted block arrives as one `Event::Paste` rather than as however
    // many key presses, which is what makes it one undo and one redraw.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let mut driver = Driver::new(Theme::from_env(), config);
    driver.set_max_rows(args.max_rows);
    driver.restore_scratch(&mut app);
    driver.connect_at_startup(startup);
    run_loop(
        &mut terminal,
        &mut app,
        &mut TerminalInput::default(),
        &Trace::from_env(),
        &mut driver,
        panic_after,
    )
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        release_terminal();
    }
}

/// Hands the terminal back to the shell: the editor gets it exactly as the
/// shell had it, and so does whatever ran sql-bench.
fn release_terminal() {
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

/// Takes it back, in the same order the startup takes it.
fn claim_terminal() -> Result<()> {
    enable_raw_mode().context("failed to take raw mode back")?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )
    .context("failed to take the screen back")
}

/// The loop: draw when something changed, wait for input, handle everything
/// already queued, carry out what the app asked for.
///
/// It returns when the app asks to quit, or when `input` is exhausted —
/// defined as two consecutive calls to [`InputSource::next`] that return
/// `Ok(None)`, which is what a replay does once its keys have run out. The
/// drain's non-blocking poll counts as one such call, so a replay ends one
/// idle timeout after its last key.
pub fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    input: &mut dyn InputSource,
    trace: &Trace,
    driver: &mut Driver,
    panic_after: Option<Duration>,
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut panicking;
    let input = match panic_after {
        Some(after) => {
            panicking = PanicAfter {
                inner: input,
                at: Instant::now() + after,
            };
            &mut panicking as &mut dyn InputSource
        }
        None => input,
    };
    while driver.turn(terminal, app, input, trace)? {}
    Ok(())
}

/// What `--panic-after-ms` wraps the input in: a panic raised where the loop
/// is about to wait, rather than between turns.
///
/// It has to be here and not around the `while`, because a loop waiting for a
/// key that never comes never gets between two turns — which is exactly the
/// state QA panics it out of.
struct PanicAfter<'a> {
    inner: &'a mut dyn InputSource,
    at: Instant,
}

impl InputSource for PanicAfter<'_> {
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        assert!(
            Instant::now() < self.at,
            "--panic-after-ms: the deliberate panic QA checks the terminal is given back after"
        );
        self.inner.next(timeout)
    }

    fn live(&self) -> bool {
        self.inner.live()
    }
}

/// What one turn of the loop carries over to the next, so the loop can be
/// taken a turn at a time.
///
/// [`run_loop`] is this in a `while`, and that is all it is. [`replay`] takes
/// the turns itself because it has to look at the screen and the app between
/// keys — which the loop, being generic over its backend, cannot hand out.
/// Nothing else should: a caller that forgets to check [`Driver::turn`]'s
/// answer never stops.
#[derive(Debug)]
pub struct Driver {
    theme: Theme,
    dirty: bool,
    empty: u8,
    runtime: Runtime,
    /// The tabs `--connect` asked for, connected once the first frame is up
    /// so that a slow server is watched rather than waited for.
    startup: Vec<usize>,
    /// Where the scratch pads are kept between runs.
    store: Store,
    /// Whether this run owns a terminal it can hand to an editor. A replay
    /// draws into a buffer and owns nothing.
    terminal: bool,
}

impl Driver {
    /// A driver whose first turn draws, because nothing has been drawn yet.
    #[must_use]
    pub fn new(theme: Theme, config: &Config) -> Self {
        Self {
            theme,
            dirty: true,
            empty: 0,
            runtime: Runtime::new(config),
            startup: Vec::new(),
            store: Store::from_env(),
            terminal: true,
        }
    }

    /// The cap every query runs with, from `--max-rows`.
    pub fn set_max_rows(&mut self, max_rows: usize) {
        self.runtime.set_max_rows(max_rows);
    }

    /// Keep the pads somewhere other than `$SQL_BENCH_STATE_DIR` says, which
    /// is what a test does.
    pub fn keep_scratch_in(&mut self, store: Store) {
        self.store = store;
    }

    /// Say that this run has no terminal to hand over, so Ctrl-E says so
    /// instead of leaving a screen nobody can take back.
    pub fn without_terminal(&mut self) {
        self.terminal = false;
    }

    /// Fill every tab's pad from its file, before the first frame.
    pub fn restore_scratch(&self, app: &mut App) {
        if let Some(why) = self.store.restore(app) {
            app.shell.error = Some(format!("scratch not loaded: {why}"));
        }
    }

    /// Connect these tabs as soon as the first frame is on the screen.
    pub fn connect_at_startup(&mut self, tabs: Vec<usize>) {
        self.startup = tabs;
    }

    /// Draw if something changed, wait for input, handle everything already
    /// queued, carry out what the app asked for.
    ///
    /// `Ok(false)` means the loop is over: the app asked to quit, or `input`
    /// is exhausted. Taking further turns after that is allowed and does
    /// nothing surprising — it is how a replay's `wait` lets the app make
    /// progress — but a quit app is never handed another event.
    pub fn turn<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut App,
        input: &mut dyn InputSource,
        trace: &Trace,
    ) -> Result<bool>
    where
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        if app.shell.should_quit {
            return Ok(false);
        }
        self.dirty |= self.runtime.poll_connections(app);
        // Every batch that has arrived since the last turn, and then one
        // frame: a scan reporting five hundred rows at a time is not five
        // hundred redraws.
        self.dirty |= self.runtime.poll_queries(app, trace);
        self.dirty |= self.runtime.poll_catalog(app);
        for action in app.settle(Instant::now()) {
            self.act(terminal, app, action)?;
        }
        // A running query animates its title the way a connecting tab
        // animates its mark, so both keep the loop ticking.
        let spinning = app.busy();
        self.dirty |= app.shell.tick(Instant::now(), spinning);
        let mut drew = Duration::ZERO;
        if self.dirty {
            let at = Instant::now();
            terminal.draw(|frame| ui::render(frame, app, &self.theme))?;
            self.dirty = false;
            drew = at.elapsed();
            if trace.is_on() {
                trace.event("frame", &[("draw_ms", &millis(drew))]);
            }
            // After the first frame and never before: a connection takes up
            // to ten seconds and nobody should watch a blank terminal for it.
            for tab in std::mem::take(&mut self.startup) {
                self.runtime.connect(app, tab);
            }
        }
        // A pad that owes the disk a save keeps the loop turning too, because
        // the save is due half a second after a key and not at the next one.
        let settling = app.settling();
        let timeout = match (spinning, settling) {
            (true, _) => SPIN_EVERY,
            (false, true) => SETTLE_POLL,
            (false, false) => IDLE_TIMEOUT,
        };
        let Some(first) = input.next(timeout)? else {
            if (spinning || settling) && input.live() {
                // An app still working is not an exhausted one — unless
                // there is nobody left to watch it work.
                self.empty = 0;
                return Ok(true);
            }
            // Saturating because a replay keeps turning the loop through a
            // wait, long after it has counted to two.
            self.empty = self.empty.saturating_add(1);
            return Ok(self.empty < 2);
        };
        self.empty = 0;
        // Everything already queued is handled before the screen is painted
        // again, so a burst of keys is one frame rather than one frame each.
        let handling = Instant::now();
        let mut event = Some(first);
        while let Some(this) = event {
            self.dirty = true;
            for action in app.handle(this) {
                self.act(terminal, app, action)?;
            }
            if app.shell.should_quit || handling.elapsed() >= DRAIN_LIMIT {
                break;
            }
            event = input.next(Duration::ZERO)?;
            if event.is_none() {
                self.empty = self.empty.saturating_add(1);
            }
        }
        let input_ms = handling.elapsed();
        if trace.is_on() && drew + input_ms >= SLOW_TURN {
            trace.event(
                "turn",
                &[
                    ("total_ms", &millis(drew + input_ms)),
                    ("draw_ms", &millis(drew)),
                    ("input_ms", &millis(input_ms)),
                ],
            );
        }
        Ok(!app.shell.should_quit)
    }
}

/// What the app asked for, done.
impl Driver {
    fn act<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut App,
        action: Action,
    ) -> Result<()>
    where
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        match action {
            Action::Quit => {
                // Whatever was typed in the last half second is still owed to
                // the disk, and there is no next turn to owe it on.
                for tab in 0..app.tabs.len() {
                    self.save_scratch(app, tab);
                }
                app.shell.should_quit = true;
            }
            Action::Connect(tab) => self.runtime.connect(app, tab),
            Action::Disconnect(tab) => self.runtime.disconnect(app, tab),
            Action::RunStatement { tab, sql } => {
                self.runtime.run(app, tab, Pending::Statement(sql));
            }
            Action::RunAll { tab, statements } => {
                self.runtime.run(app, tab, Pending::All(statements));
            }
            Action::Cancel(tab) => self.runtime.cancel(tab),
            Action::MoreRows { tab } => self.runtime.more_rows(app, tab),
            Action::OpenEditor { tab } => self.open_editor(terminal, app, tab)?,
            Action::SaveScratch { tab } => self.save_scratch(app, tab),
            Action::Copy(text) => self.copy(&text),
            Action::Export { tab, path } => self.export(app, tab, &path),
            Action::LoadObjects { tab, request } => self.runtime.load(app, tab, request),
        }
        Ok(())
    }

    /// Write one pad, and tell the person if it could not be written — a pad
    /// that is silently not saved is a pad that is lost.
    fn save_scratch(&mut self, app: &mut App, tab: usize) {
        let Some(open) = app.tabs.get(tab) else {
            return;
        };
        if !open.scratch.modified() {
            return;
        }
        match self.store.save(open) {
            Ok(()) => {
                if let Some(open) = app.tabs.get_mut(tab) {
                    open.scratch.saved();
                }
                // The pane title stops saying `[modified]`, which is a frame.
                self.dirty = true;
            }
            Err(error) => app.shell.error = Some(format!("scratch not saved: {error:#}")),
        }
    }

    /// The Ctrl-E round trip: the terminal goes back to the shell, the editor
    /// gets the pad, and the screen is taken back and drawn again whatever
    /// the editor did.
    fn open_editor<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut App,
        tab: usize,
    ) -> Result<()>
    where
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        if !self.terminal {
            app.shell.status = "editor unavailable in replay".to_owned();
            return Ok(());
        }
        let Some(open) = app.tabs.get(tab) else {
            return Ok(());
        };
        let Some(command) = editor::command() else {
            app.shell.error = Some("no editor: set $VISUAL or $EDITOR".to_owned());
            return Ok(());
        };
        let sql = open.scratch.text();
        release_terminal();
        let edited = editor::round_trip(&std::env::temp_dir(), tab, &sql, &command);
        let taken = claim_terminal();
        // The editor drew over the screen and ratatui still believes its own
        // last frame is on it; a resize to the size it already has resets
        // both, and — unlike `clear` — asks the terminal nothing.
        let area = terminal.size()?;
        terminal.resize(area.into())?;
        self.dirty = true;
        if let Err(error) = taken {
            // Nothing can be said through a TUI that is not there.
            eprintln!("sql-bench: {error:#}");
            return Err(error);
        }
        match edited {
            Ok(text) => {
                if let Some(open) = app.tabs.get_mut(tab) {
                    open.scratch.set_text(&text);
                }
            }
            Err(error) => app.shell.error = Some(format!("editor: {error:#}")),
        }
        Ok(())
    }

    /// The result set on screen, written where the prompt said: JSON for a
    /// `.json` name and CSV for anything else, because a person who typed a
    /// file name has already said what they wanted.
    ///
    /// Every row that was fetched goes, not the ones on screen — and the cap
    /// that stopped the scan is still the cap, which is what the title says.
    fn export(&mut self, app: &mut App, tab: usize, path: &str) {
        let Some(open) = app.tabs.get(tab) else {
            return;
        };
        let results = &open.results;
        let path = expand(path);
        let json = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        let text = if json {
            export::json(results.columns(), results.rows())
        } else {
            export::csv(results.columns(), results.rows())
        };
        let rows = grouped(results.rows().len());
        match std::fs::write(&path, text) {
            Ok(()) => {
                app.shell.status = format!("exported {rows} rows to {}", path.display());
            }
            Err(error) => {
                app.shell.error = Some(format!("export failed: {error}"));
            }
        }
        self.dirty = true;
    }

    /// The selection into the terminal's own clipboard, through OSC 52. It
    /// is a best effort: a terminal that ignores the escape leaves the text
    /// in the app's clipboard and nobody any worse off.
    fn copy(&self, text: &str) {
        use std::io::Write as _;
        if !self.terminal {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.flush();
    }
}

/// A leading `~` is the one thing a person typing a path expects a program
/// to know. Nothing else is expanded: a shell did that before the argument
/// ever arrived, and a prompt is not a shell.
fn expand(path: &str) -> PathBuf {
    let rest = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest.trim_start_matches('/'),
        _ => return PathBuf::from(path),
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => PathBuf::from(path),
    }
}

/// Base64, because OSC 52 carries the selection that way and a crate for
/// sixteen lines would be a dependency for sixteen lines.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut block = [0u8; 3];
        block[..chunk.len()].copy_from_slice(chunk);
        let triple = u32::from_be_bytes([0, block[0], block[1], block[2]]);
        for index in 0..4 {
            text.push(if index <= chunk.len() {
                #[allow(clippy::cast_possible_truncation)]
                let sextet = ((triple >> (18 - index * 6)) & 0x3f) as usize;
                ALPHABET[sextet] as char
            } else {
                '='
            });
        }
    }
    text
}

/// Milliseconds with the fraction kept. A draw is a fraction of one, so
/// whole milliseconds cannot say whether a p95 is a tenth of the budget or
/// most of it — and a budget nothing can be measured against is not one.
fn millis(elapsed: Duration) -> String {
    format!("{:.3}", elapsed.as_secs_f64() * 1000.0)
}
