//! `NO_COLOR`: the same screens, painted in nothing.

use super::*;
use ratatui::style::Color;

#[test]
fn no_color_leaves_every_cell_unpainted() {
    let mut app = two_tabs();
    app.shell.error = Some("could not connect".to_owned());
    app.shell.help = true;
    let terminal = frame_with(120, 40, &app, &Theme::new(true));
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg, cell.modifier),
                (
                    Color::Reset,
                    Color::Reset,
                    ratatui::style::Modifier::empty()
                ),
                "({x}, {y}) is painted"
            );
        }
    }
}

#[test]
fn every_token_is_plain_without_colour_and_distinct_with_it() {
    assert_eq!(Theme::new(true), Theme::default());
    let colour = Theme::new(false);
    assert_ne!(colour.accent, colour.border);
    assert_ne!(colour.error, colour.ok);
}
