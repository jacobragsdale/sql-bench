//! The scratch pad pane: the gutter, the cursor, the selection, and how far
//! the view scrolls to keep the cursor on the screen.

use super::*;
use crate::app::Focus;
use ratatui::style::Color;

/// Where the pad's pane starts at 120 columns, and how wide it is.
const LEFT: u16 = 36;
const WIDTH: usize = 84;

/// An app with the pad focused and this text in it.
fn typed(text: &str) -> App {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    app.tabs[0].scratch.set_text(text);
    app
}

/// Row `y` of the pad's pane, border to border.
fn pad_line(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (LEFT..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

/// The same row as the pane paints it: a border, the padding, the body, and
/// the space left over.
fn row(body: &str) -> String {
    format!(
        "│ {body}{:fill$}│",
        "",
        fill = WIDTH - 3 - body.chars().count()
    )
}

#[test]
fn every_line_is_numbered_in_a_gutter_as_wide_as_the_last_number() {
    let terminal = frame(120, 40, &typed("select 1\nfrom dual"));
    assert_eq!(pad_line(&terminal, 2), row("1 select 1"));
    assert_eq!(pad_line(&terminal, 3), row("2 from dual"));

    // Ten lines are two digits wide, and the numbers stay right-aligned.
    let text: Vec<String> = (1..=10).map(|n| format!("select {n}")).collect();
    let terminal = frame(120, 40, &typed(&text.join("\n")));
    assert_eq!(pad_line(&terminal, 2), row(" 1 select 1"));
    assert_eq!(pad_line(&terminal, 11), row("10 select 10"));
}

#[test]
fn the_cursor_is_the_one_reversed_cell_and_only_while_the_pad_has_the_focus() {
    let theme = Theme::new(false);
    let mut app = typed("select 1");
    for _ in 0..3 {
        app.tabs[0].scratch.handle(key("Right"));
    }
    // The text starts after `1 `, so the cursor's cell is three in.
    // `painted` reads the cell back with the foreground it was left with,
    // which for text nobody styled is the terminal's own.
    let plain = Style::new().fg(Color::Reset);
    let terminal = frame(120, 40, &app);
    assert_eq!(
        painted(&terminal, LEFT + 2 + 2 + 3, 2),
        theme.cursor.fg(Color::Reset)
    );
    assert_eq!(painted(&terminal, LEFT + 2 + 2 + 2, 2), plain);

    app.shell.focus = Focus::Objects;
    let terminal = frame(120, 40, &app);
    assert_eq!(
        painted(&terminal, LEFT + 2 + 2 + 3, 2),
        plain,
        "a pad nobody is typing in shows no cursor"
    );
}

#[test]
fn a_selection_is_painted_with_the_accent_behind_it() {
    let mut app = typed("select 1");
    for _ in 0..3 {
        app.tabs[0].scratch.handle(key("Shift-Right"));
    }
    let terminal = frame(120, 40, &app);
    let buffer = terminal.backend().buffer();
    let background = |x: u16| buffer[(x, 2)].bg;
    assert_eq!(background(LEFT + 4), Color::Cyan, "the first selected cell");
    assert_eq!(background(LEFT + 6), Color::Cyan);
    assert_eq!(
        background(LEFT + 7),
        Color::Reset,
        "and the cursor's is not"
    );
}

#[test]
fn a_long_line_scrolls_sideways_so_that_the_cursor_stays_on_the_screen() {
    let long: String = (0..200)
        .map(|n| char::from_digit(n % 10, 10).expect("a digit"))
        .collect();
    let mut app = typed(&long);
    let terminal = frame(120, 40, &app);
    assert_eq!(
        pad_line(&terminal, 2),
        row(&format!("1 {}", &long[..WIDTH - 6])),
        "from the left while the cursor is there"
    );

    app.tabs[0].scratch.handle(key("End"));
    let terminal = frame(120, 40, &app);
    // The window is the text's width, and the cursor's cell is the last one.
    let visible = WIDTH - 6;
    assert_eq!(
        pad_line(&terminal, 2),
        row(&format!("1 {} ", &long[200 - visible + 1..])),
        "and from the right once it is not"
    );
}

#[test]
fn the_title_says_modified_until_the_pad_is_saved() {
    let mut app = typed("select 1");
    assert!(
        pad_line(&frame(120, 40, &app), 1).starts_with("╭ Scratch [modified] "),
        "a pad the disk has not caught up with says so"
    );

    app.tabs[0].scratch.saved();
    assert!(pad_line(&frame(120, 40, &app), 1).starts_with("╭ Scratch ──"));
}

#[test]
fn a_pad_taller_than_the_pane_scrolls_to_the_line_the_cursor_is_on() {
    let text: Vec<String> = (1..=40).map(|n| format!("select {n}")).collect();
    let mut app = typed(&text.join("\n"));
    for _ in 0..39 {
        app.tabs[0].scratch.handle(key("Down"));
    }
    let terminal = frame(120, 40, &app);
    // Thirteen rows inside the pane, so the fortieth line is the last of
    // them and the twenty-eighth is the first.
    assert_eq!(pad_line(&terminal, 2), row("28 select 28"));
    assert_eq!(pad_line(&terminal, 14), row("40 select 40"));
}
