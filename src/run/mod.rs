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
pub use runtime::{Runtime, startup_tabs};

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{Action, App, SPIN_EVERY};
use crate::cli::Cli;
use crate::config::Config;
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
}

/// The keyboard, through crossterm.
#[derive(Debug, Default)]
pub struct TerminalInput {
    idle: bool,
}

impl InputSource for TerminalInput {
    /// One poll, and then — having already come up empty once — however long
    /// it takes: a terminal nobody is typing at is idle, never exhausted, and
    /// two empty polls in a row would end the loop.
    ///
    /// The exception is a timeout shorter than the idle one, which is the
    /// loop saying it wants the turn back on time because something is
    /// animating; [`Driver::turn`] does not count those empties.
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        loop {
            if event::poll(timeout).context("waiting for a key")? {
                self.idle = false;
                return Ok(Some(event::read().context("reading a key")?));
            }
            if !std::mem::replace(&mut self.idle, true) || timeout < IDLE_TIMEOUT {
                return Ok(None);
            }
        }
    }
}

/// Opens the TUI on this terminal and gives it back when the app quits.
pub fn run(config: &Config, args: &Cli) -> Result<()> {
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
    driver.restore_scratch(&mut app);
    driver.connect_at_startup(startup);
    run_loop(
        &mut terminal,
        &mut app,
        &mut TerminalInput::default(),
        &Trace::from_env(),
        &mut driver,
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
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    while driver.turn(terminal, app, input, trace)? {}
    Ok(())
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
        for action in app.settle(Instant::now()) {
            self.act(terminal, app, action)?;
        }
        let spinning = app.connecting();
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
            if spinning || settling {
                // An app still working is not an exhausted one.
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
            // T5.2 runs these; until then the footer says why nothing did.
            Action::RunStatement { .. } | Action::RunAll { .. } => {
                app.shell.status = "no query runner yet".to_owned();
            }
            Action::OpenEditor { tab } => self.open_editor(terminal, app, tab)?,
            Action::SaveScratch { tab } => self.save_scratch(app, tab),
            Action::Copy(text) => self.copy(&text),
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

fn millis(elapsed: Duration) -> String {
    elapsed.as_millis().to_string()
}
