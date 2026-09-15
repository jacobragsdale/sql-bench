//! Rendering tests. Everything goes through `TestBackend` and asserts on the
//! text of a line, which is what rule 3 asks for: no screenshots.

mod layout;
mod overlays;
mod styles;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Style;

use super::*;
use crate::app::tests::{key, two_tabs};
use crossterm::event::Event;

/// One frame of `app`, in colour.
fn frame(width: u16, height: u16, app: &App) -> Terminal<TestBackend> {
    frame_with(width, height, app, &Theme::new(false))
}

fn frame_with(width: u16, height: u16, app: &App, theme: &Theme) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| render(frame, app, theme))
        .expect("a frame");
    terminal
}

/// Row `y`, with the padding every row is right-filled with taken off.
fn line(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    let height = terminal.backend().buffer().area.height;
    (0..height)
        .map(|y| line(terminal, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The colour and weight a cell was painted in, as a [`Style`] to compare
/// with a theme token.
fn painted(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Style {
    let cell = &terminal.backend().buffer()[(x, y)];
    Style::new().fg(cell.fg).add_modifier(cell.modifier)
}

/// The top-left corner of every pane frame, in the order they are drawn.
fn corners(terminal: &Terminal<TestBackend>) -> Vec<(u16, u16)> {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .filter(|(x, y)| buffer[(*x, *y)].symbol() == "╭")
        .collect()
}
