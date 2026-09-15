//! The five styles the whole screen is painted with.
//!
//! This is the only module that names a [`Color`]: everything else asks for
//! `accent` or `error` and gets whatever this run's theme says that is. With
//! `NO_COLOR` set every one of them is plain, so the same screens render on a
//! terminal that was asked for no colour at all.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Theme {
    /// The focused pane's border, the tab showing, anything that is where
    /// the eye should go.
    pub accent: Style,
    /// Placeholders, key hints, the tabs not showing.
    pub dim: Style,
    /// The frame of a pane nothing is focused on.
    pub border: Style,
    pub error: Style,
    pub ok: Style,
}

impl Theme {
    /// Colour unless `NO_COLOR` is set to something.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()))
    }

    #[must_use]
    pub fn new(no_color: bool) -> Self {
        if no_color {
            return Self::default();
        }
        Self {
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            dim: Style::new().fg(Color::DarkGray),
            border: Style::new().fg(Color::DarkGray),
            error: Style::new().fg(Color::Red),
            ok: Style::new().fg(Color::Green),
        }
    }
}
