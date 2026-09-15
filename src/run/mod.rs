//! The run: claiming the terminal, waiting for input, drawing when something
//! changed, carrying out what the app asked for, and the trace file. The only
//! module that blocks.

pub mod replay;

#[cfg(test)]
mod tests;

pub use replay::replay;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{Action, App};
use crate::config::Config;
use crate::trace::Trace;
use crate::ui;
use crate::ui::theme::Theme;

/// How long queued input may be handled before the screen is painted again.
/// A key held down arrives about as fast as it can be handled, so without a
/// limit the drain never empties and nothing is redrawn until it comes up.
const DRAIN_LIMIT: Duration = Duration::from_millis(50);

/// How long the loop waits for input before taking a turn with none.
const IDLE_TIMEOUT: Duration = Duration::from_millis(250);

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
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        loop {
            if event::poll(timeout).context("waiting for a key")? {
                self.idle = false;
                return Ok(Some(event::read().context("reading a key")?));
            }
            if !std::mem::replace(&mut self.idle, true) {
                return Ok(None);
            }
        }
    }
}

/// Opens the TUI on this terminal and gives it back when the app quits.
pub fn run(config: &Config) -> Result<()> {
    let mut app = App::new(config);
    // `try_init` takes raw mode and the alternate screen and installs the
    // panic hook that gives both back; `Restore` is the same for every other
    // way out of this function.
    let mut terminal = ratatui::try_init()
        .inspect_err(|_| ratatui::restore())
        .context("failed to take the terminal")?;
    let _restore = Restore;
    run_loop(
        &mut terminal,
        &mut app,
        &mut TerminalInput::default(),
        &Trace::from_env(),
    )
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
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
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut driver = Driver::new(Theme::from_env());
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
}

impl Driver {
    /// A driver whose first turn draws, because nothing has been drawn yet.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            theme,
            dirty: true,
            empty: 0,
        }
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
        let mut drew = Duration::ZERO;
        if self.dirty {
            let at = Instant::now();
            terminal.draw(|frame| ui::render(frame, app, &self.theme))?;
            self.dirty = false;
            drew = at.elapsed();
            if trace.is_on() {
                trace.event("frame", &[("draw_ms", &millis(drew))]);
            }
        }
        let Some(first) = input.next(IDLE_TIMEOUT)? else {
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
                match action {
                    Action::Quit => app.shell.should_quit = true,
                }
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

fn millis(elapsed: Duration) -> String {
    elapsed.as_millis().to_string()
}
