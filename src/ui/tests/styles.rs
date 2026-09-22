//! `NO_COLOR`: the same screens, painted in nothing.

use super::*;
use ratatui::style::Color;

#[test]
fn no_color_paints_nothing_but_the_cursor_and_the_selection() {
    use ratatui::style::Modifier;
    let mut app = two_tabs();
    app.shell.error = Some("could not connect".to_owned());
    app.shell.help = true;
    let terminal = frame_with(120, 40, &app, &Theme::new(true));
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y}) is painted"
            );
            assert!(
                (Modifier::REVERSED | Modifier::UNDERLINED).contains(cell.modifier),
                "({x}, {y}) is {:?}",
                cell.modifier
            );
        }
    }
}

#[test]
fn every_token_is_plain_without_colour_and_distinct_with_it() {
    use ratatui::style::Modifier;
    let plain = Theme::new(true);
    assert_eq!(
        Theme {
            cursor: Style::default(),
            selection: Style::default(),
            ..plain
        },
        Theme::default(),
        "only the cursor and the selection are anything"
    );
    assert_eq!(plain.cursor, Style::new().add_modifier(Modifier::REVERSED));
    assert_ne!(
        plain.cursor, plain.selection,
        "a cursor inside a range still shows"
    );
    let colour = Theme::new(false);
    assert_ne!(colour.accent, colour.border);
    assert_ne!(colour.error, colour.ok);
}
