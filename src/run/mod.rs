//! The run: claiming the terminal, waiting for input, drawing when something
//! changed, carrying out what the app asked for, and the trace file. The only
//! module that blocks.

pub mod clipboard;
pub mod editor;
pub mod replay;
pub mod runtime;
pub mod state;

#[cfg(test)]
mod tests;

pub use replay::replay;
pub use runtime::{Pending, Runtime, startup_tabs};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::pointer::Hits;
use crate::app::results::grouped;
use crate::app::{Action, App, SPIN_EVERY};
use crate::cli::Cli;
use crate::config::Config;
use crate::export;
use crate::run::clipboard::Clipboard;
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

/// How long the loop waits for input while a paste is being read: the
/// answer is a few milliseconds away and is drawn as soon as it lands.
const PASTE_POLL: Duration = Duration::from_millis(10);

/// How long the input thread waits for the terminal before it looks again at
/// whether the editor wants it, which is how long Ctrl-E can take to start.
const READ_POLL: Duration = Duration::from_millis(50);

/// The most OSC 52 is asked to carry, encoded. Terminals cap the sequence at
/// about this (xterm and tmux at 100 000 bytes) and cut or drop a longer one,
/// and writing megabytes of it to a slow tty would stall the loop besides.
const OSC52_LIMIT: usize = 100_000;

/// Whether `$EDITOR` has the terminal, and the lock the input thread holds
/// while it reads it. Without them the thread reads keys meant for the
/// editor, which gets every other one. One of each per process: there is
/// one terminal.
static EDITING: AtomicBool = AtomicBool::new(false);
static READING: Mutex<()> = Mutex::new(());

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

    /// Whether the process was told to end — SIGTERM, or SIGHUP when the
    /// terminal went — which the loop takes as a quit, pads saved and all.
    fn terminated(&self) -> bool {
        false
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
    terminated: Arc<AtomicBool>,
}

impl Default for TerminalInput {
    fn default() -> Self {
        let (sender, events) = std::sync::mpsc::channel();
        let terminated = Arc::new(AtomicBool::new(false));
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            std::thread::spawn(move || read_terminal(&sender));
            watch_signals(Arc::clone(&terminated));
        }
        Self {
            events,
            idle: false,
            live: true,
            terminated,
        }
    }
}

/// The input thread. It ends on a read error, and on the loop hanging up —
/// which is the app quitting and the receiver going with it.
///
/// It reads only while it holds [`READING`], a poll at a time, and stands
/// aside while [`EDITING`] says the editor has the terminal. The flag is
/// looked at before the lock is taken so that a thread relocking the moment
/// it lets go cannot keep the editor waiting on it.
fn read_terminal(sender: &Sender<Event>) {
    loop {
        if EDITING.load(Ordering::SeqCst) {
            std::thread::sleep(READ_POLL);
            continue;
        }
        let reading = READING.lock().unwrap_or_else(PoisonError::into_inner);
        if EDITING.load(Ordering::SeqCst) {
            continue;
        }
        let event = match event::poll(READ_POLL) {
            Ok(false) => continue,
            Ok(true) => event::read(),
            Err(error) => Err(error),
        };
        drop(reading);
        let Ok(event) = event else {
            break;
        };
        if sender.send(event).is_err() {
            break;
        }
    }
}

/// The terminal kept from the input thread for as long as this lives.
struct Editing(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Editing {
    /// Waits out the read in progress, at most [`READ_POLL`].
    fn start() -> Self {
        EDITING.store(true, Ordering::SeqCst);
        Self(READING.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for Editing {
    // The flag goes before the lock does, so the thread waiting on the lock
    // finds it down.
    fn drop(&mut self) {
        EDITING.store(false, Ordering::SeqCst);
    }
}

/// SIGTERM and SIGHUP raise `terminated`, which the loop quits on. A second
/// one while the loop has still not let go gives the terminal back and ends
/// the process there, because a person sending it again has stopped waiting.
///
/// The signals are caught on a thread of their own with a current-thread
/// tokio runtime, which is the one signal API already among the
/// dependencies.
#[cfg(unix)]
fn watch_signals(terminated: Arc<AtomicBool>) {
    use tokio::signal::unix::{SignalKind, signal};
    let _ = std::thread::Builder::new()
        .name("signals".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let (Ok(mut term), Ok(mut hup)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::hangup()),
                ) else {
                    return;
                };
                loop {
                    futures_util::future::select(Box::pin(term.recv()), Box::pin(hup.recv())).await;
                    if terminated.swap(true, Ordering::SeqCst) {
                        release_terminal();
                        std::process::exit(1);
                    }
                }
            });
        });
}

#[cfg(not(unix))]
fn watch_signals(_terminated: Arc<AtomicBool>) {}

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
                    if !std::mem::replace(&mut self.idle, true)
                        || timeout < IDLE_TIMEOUT
                        || self.terminated()
                    {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn live(&self) -> bool {
        self.live
    }

    fn terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
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
    restore_only_from_main();
    let _restore = Restore;
    // A pasted block arrives as one `Event::Paste` rather than as however
    // many key presses, which is what makes it one undo and one redraw.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    let mut driver = Driver::new(Theme::from_env(), config);
    driver.clipboard =
        Clipboard::detect(|name| std::env::var(name).ok(), cfg!(target_os = "macos"));
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

/// Narrows the panic hook `try_init` installed to the loop's own thread.
///
/// A worker that panics is already an error the loop shows — the channel it
/// was answering on ends — so giving the terminal back for it would leave the
/// loop drawing over the shell. Its panic goes to the trace instead, because
/// a message printed over the screen is one nobody can read.
fn restore_only_from_main() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        match thread.name() {
            Some("main") => restoring(info),
            name => Trace::from_env().event(
                "panic",
                &[
                    ("thread", name.unwrap_or("unnamed")),
                    ("message", &info.to_string().replace(['\n', '\t'], " ")),
                ],
            ),
        }
    }));
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        release_terminal();
    }
}

/// Hands the terminal back to the shell: the editor gets it exactly as the
/// shell had it, and so does whatever ran sql-bench.
///
/// The mouse goes back before the alternate screen is left: a shell handed a
/// terminal that still reports the mouse gets escape codes typed at it every
/// time the pointer moves.
fn release_terminal() {
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
}

/// Takes it back, in the same order the startup takes it.
fn claim_terminal() -> Result<()> {
    enable_raw_mode().context("failed to take raw mode back")?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
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
/// idle timeout after its last key. A mouse event the drain held back is
/// handled before `input` is asked again, so the loop never ends with one
/// still owed.
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

    fn terminated(&self) -> bool {
        self.inner.terminated()
    }
}

/// A paste worker's answer: the tab that asked, and what the clipboard held.
type Pasted = (usize, Option<String>);

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
    /// Where the last frame drew what can be clicked.
    hits: Hits,
    /// A mouse event that arrived behind one that changed the screen. It is
    /// handled next turn, after the frame it was aimed at has been drawn.
    held: Option<Event>,
    /// The system clipboard's tool. Only a run on a real terminal looks
    /// for one; a replay has a fake and a test has none.
    clipboard: Clipboard,
    /// Where the paste workers answer, which tab asked, and what they read.
    pastes: (Sender<Pasted>, Receiver<Pasted>),
    /// How many of them are still reading, which keeps the loop turning.
    pasting: usize,
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
            hits: Hits::default(),
            held: None,
            clipboard: Clipboard::None,
            pastes: mpsc::channel(),
            pasting: 0,
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
        if input.terminated() {
            self.act(terminal, app, Action::Quit)?;
            return Ok(false);
        }
        self.dirty |= self.runtime.poll_connections(app);
        // Every batch that has arrived since the last turn, and then one
        // frame: a scan reporting five hundred rows at a time is not five
        // hundred redraws.
        self.dirty |= self.runtime.poll_queries(app, trace);
        self.dirty |= self.runtime.poll_catalog(app);
        while let Ok((tab, text)) = self.pastes.1.try_recv() {
            self.pasting = self.pasting.saturating_sub(1);
            app.pasted(tab, text);
            self.dirty = true;
        }
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
            terminal.draw(|frame| self.hits = ui::render(frame, app, &self.theme))?;
            app.drawn(&self.hits);
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
        // the save is due half a second after a key and not at the next one;
        // so does a paste still being read, whose answer nobody types for.
        let settling = app.settling() || self.pasting > 0;
        let timeout = match (spinning, settling) {
            (_, true) if self.pasting > 0 => PASTE_POLL,
            (true, _) => SPIN_EVERY,
            (false, true) => SETTLE_POLL,
            (false, false) => IDLE_TIMEOUT,
        };
        let first = match self.held.take() {
            Some(held) => Some(held),
            None => input.next(timeout)?,
        };
        let Some(first) = first else {
            if input.terminated() {
                self.act(terminal, app, Action::Quit)?;
                return Ok(false);
            }
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
        // again, so a burst of keys is one frame rather than one frame each —
        // except a click behind a key that changed the screen, which is held
        // until the screen it was aimed at is the one its targets come from.
        let handling = Instant::now();
        let mut event = Some(first);
        while let Some(this) = event {
            if self.dirty && matches!(this, Event::Mouse(_)) {
                self.held = Some(this);
                break;
            }
            self.dirty |= self.handle(terminal, app, this, trace)?;
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
    /// One event into the app, and whether the screen has to be drawn again.
    ///
    /// A key always changes something worth a frame. The mouse mostly does
    /// not: a press is only a press until it comes up, and the pointer moving
    /// is only a frame when it moves onto something else.
    fn handle<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut App,
        event: Event,
        trace: &Trace,
    ) -> Result<bool>
    where
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        // The app sorts inside the call and reads no clock, so the call is
        // what a `sort` line times: the sort is all of it worth timing.
        let at = trace.is_on().then(Instant::now);
        let (actions, changed) = match event {
            Event::Mouse(mouse) => {
                let before = app.shell.mouse.pointer;
                let split = app.shell.split;
                let seam = app.shell.mouse.seam().is_some();
                let actions = app.pointer(mouse, Instant::now(), &self.hits);
                let changed = match mouse.kind {
                    MouseEventKind::Down(_) => false,
                    // A held seam stays lit wherever the pointer goes, so
                    // only a move of the seam itself shows.
                    MouseEventKind::Drag(_) if seam => split != app.shell.split,
                    MouseEventKind::Moved => {
                        let spot = |at: Option<_>| at.and_then(|at| self.hits.spot(at));
                        spot(before) != spot(app.shell.mouse.pointer)
                    }
                    _ => true,
                };
                (actions, changed)
            }
            other => (app.handle(other), true),
        };
        for action in actions {
            if let (Action::Sorted { rows }, Some(at)) = (&action, at) {
                trace.event(
                    "sort",
                    &[("rows", &rows.to_string()), ("ms", &millis(at.elapsed()))],
                );
            }
            self.act(terminal, app, action)?;
        }
        Ok(changed)
    }

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
            Action::Copy(text) => self.copy(app, text),
            Action::ReadClipboard { tab } => self.read_clipboard(app, tab),
            Action::Export { tab, path } => self.export(app, tab, &path),
            Action::LoadObjects { tab, request } => self.runtime.load(app, tab, request),
            Action::Sorted { .. } => {}
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
        let editing = Editing::start();
        release_terminal();
        let edited = editor::round_trip(&std::env::temp_dir(), tab, &sql, &command);
        let taken = claim_terminal();
        drop(editing);
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

    /// The selection into the terminal's own clipboard through OSC 52,
    /// and into the system's through its tool when there is one. Both are a
    /// best effort: a terminal that ignores the escape and a machine with no
    /// tool leave the text in the app's clipboard and nobody any worse off.
    ///
    /// A selection too big for OSC 52 is not sent that way at all, and when
    /// there is no tool either, the footer says where the copy went.
    fn copy(&mut self, app: &mut App, text: String) {
        use std::io::Write as _;
        if self.terminal && text.len().div_ceil(3) * 4 <= OSC52_LIMIT {
            let mut out = std::io::stdout();
            let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
            let _ = out.flush();
        } else if self.terminal && matches!(self.clipboard, Clipboard::None) {
            app.shell
                .status
                .push_str(" (inside sql-bench only: too big for the terminal's clipboard)");
        }
        match &mut self.clipboard {
            Clipboard::None => {}
            Clipboard::Tool { copy, .. } => clipboard::write(copy.clone(), text),
            Clipboard::Fake(fake) => *fake = text,
        }
    }

    /// Ctrl-V: the system clipboard read on a worker, whose answer the next
    /// turns collect — a tool waiting on a display that is gone costs the
    /// loop nothing. A fake one is answered at once.
    fn read_clipboard(&mut self, app: &mut App, tab: usize) {
        let paste = match &self.clipboard {
            Clipboard::Tool { paste, .. } => paste.clone(),
            Clipboard::Fake(text) => return app.pasted(tab, Some(text.clone())),
            Clipboard::None => return app.pasted(tab, None),
        };
        let answer = self.pastes.0.clone();
        let spawned = std::thread::Builder::new()
            .name("paste".to_owned())
            .spawn(move || {
                let _ = answer.send((tab, clipboard::read(&paste, clipboard::TIMEOUT)));
            });
        match spawned {
            Ok(_) => self.pasting += 1,
            Err(_) => app.pasted(tab, None),
        }
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
