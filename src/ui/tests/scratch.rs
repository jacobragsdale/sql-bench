//! The scratch pad pane: the gutter, the cursor, the selection, and how far
//! the view scrolls to keep the cursor on the screen.

use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Color;

use super::*;
use crate::app::Focus;

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

/// The pad's text starts here at 120x40: the border, the padding, and the
/// row the title is on.
const TEXT_X: u16 = LEFT + 2;
const TEXT_Y: u16 = 2;

/// Forty numbered lines, the cursor on the last, as a 120x40 frame shows
/// them: lines 28 to 40.
fn scrolled() -> App {
    let text: Vec<String> = (1..=40).map(|n| format!("select {n}")).collect();
    let mut app = typed(&text.join("\n"));
    for _ in 0..39 {
        app.tabs[0].scratch.handle(key("Down"));
    }
    app
}

/// The hits of the 120x40 frame `app` is showing.
fn shown(app: &App) -> Hits {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("a test terminal");
    let mut hits = Hits::default();
    terminal
        .draw(|frame| hits = render(frame, app, &Theme::new(false)))
        .expect("a frame");
    hits
}

/// One mouse event at `(x, y)` against the frame `app` is showing.
fn pointer(app: &mut App, kind: MouseEventKind, (x, y): (u16, u16), modifiers: KeyModifiers) {
    let hits = shown(app);
    let event = MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers,
    };
    app.pointer(event, Instant::now(), &hits);
}

fn click(app: &mut App, at: (u16, u16)) {
    click_with(app, at, KeyModifiers::NONE);
}

fn click_with(app: &mut App, at: (u16, u16), modifiers: KeyModifiers) {
    pointer(app, MouseEventKind::Down(MouseButton::Left), at, modifiers);
    pointer(app, MouseEventKind::Up(MouseButton::Left), at, modifiers);
}

fn drag(app: &mut App, at: (u16, u16)) {
    pointer(
        app,
        MouseEventKind::Drag(MouseButton::Left),
        at,
        KeyModifiers::NONE,
    );
}

/// Where character `column` of the row `row` rows down the pad is, with a
/// gutter `gutter` cells wide.
const fn cell(gutter: u16, column: u16, row: u16) -> (u16, u16) {
    (TEXT_X + gutter + column, TEXT_Y + row)
}

#[test]
fn a_click_on_the_first_line_of_a_scrolled_pad_puts_the_cursor_there_and_the_view_stays() {
    let mut app = scrolled();
    let before = frame(120, 40, &app);
    assert_eq!(pad_line(&before, 2), row("28 select 28"));

    click(&mut app, cell(3, 7, 0));
    assert_eq!(app.tabs[0].scratch.cursor(), (27, 7));
    let after = frame(120, 40, &app);
    assert_eq!(pad_line(&after, 2), row("28 select 28"));
    assert_eq!(pad_line(&after, 14), row("40 select 40"));
}

#[test]
fn a_key_that_moves_the_cursor_up_inside_the_view_leaves_the_view_where_it_is() {
    let mut app = scrolled();
    // The run loop hands every frame's window back, as it does after a draw.
    let hits = shown(&app);
    app.drawn(&hits);
    for _ in 0..5 {
        app.tabs[0].scratch.handle(key("Up"));
    }
    let terminal = frame(120, 40, &app);
    assert_eq!(pad_line(&terminal, 2), row("28 select 28"));
    assert_eq!(pad_line(&terminal, 14), row("40 select 40"));
}

#[test]
fn a_drag_selects_from_the_press_to_the_pointer_and_ctrl_c_copies_exactly_that() {
    let mut app = typed("select a, b\nfrom t\nwhere x = 1");
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        cell(2, 7, 0),
        KeyModifiers::NONE,
    );
    drag(&mut app, cell(2, 3, 1));
    drag(&mut app, cell(2, 5, 2));
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        cell(2, 5, 2),
        KeyModifiers::NONE,
    );
    assert_eq!(
        app.tabs[0].scratch.selection(),
        Some(((0, 7), (2, 5))),
        "the release is the end of the drag, not a click that drops it"
    );
    app.handle(Event::Key(key("Ctrl-C")));
    assert_eq!(app.shell.clipboard, "a, b\nfrom t\nwhere");
}

#[test]
fn a_drag_below_the_pad_scrolls_it_a_line_each_time_the_pointer_moves() {
    let text: Vec<String> = (1..=40).map(|n| format!("select {n}")).collect();
    let mut app = typed(&text.join("\n"));
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        cell(3, 0, 0),
        KeyModifiers::NONE,
    );
    drag(&mut app, (TEXT_X + 5, 20));
    assert_eq!(pad_line(&frame(120, 40, &app), 2), row(" 2 select 2"));
    drag(&mut app, (TEXT_X + 5, 20));
    assert_eq!(pad_line(&frame(120, 40, &app), 2), row(" 3 select 3"));
    let copied = app.tabs[0].scratch.selected_text().expect("a selection");
    assert!(copied.starts_with("select 1\nselect 2\n"), "{copied}");
}

#[test]
fn shift_click_takes_the_selection_to_the_click() {
    let mut app = typed("select a, b\nfrom t");
    click(&mut app, cell(2, 7, 0));
    click_with(&mut app, cell(2, 4, 1), KeyModifiers::SHIFT);
    assert_eq!(
        app.tabs[0].scratch.selected_text().as_deref(),
        Some("a, b\nfrom")
    );
}

#[test]
fn a_double_click_selects_the_word_and_on_a_space_only_places_the_cursor() {
    let mut app = typed("select order_id from t");
    click(&mut app, cell(2, 10, 0));
    click(&mut app, cell(2, 10, 0));
    assert_eq!(
        app.tabs[0].scratch.selected_text().as_deref(),
        Some("order_id")
    );

    let mut app = typed("select order_id from t");
    click(&mut app, cell(2, 6, 0));
    click(&mut app, cell(2, 6, 0));
    assert_eq!(app.tabs[0].scratch.selected_text(), None);
    assert_eq!(app.tabs[0].scratch.cursor(), (0, 6));
}

#[test]
fn a_click_in_the_gutter_or_past_the_end_of_a_line_lands_on_the_line() {
    let mut app = typed("select 1\nfrom dual");
    click(&mut app, (TEXT_X, TEXT_Y + 1));
    assert_eq!(
        app.tabs[0].scratch.cursor(),
        (1, 0),
        "the gutter is column 0"
    );
    click(&mut app, cell(2, 60, 0));
    assert_eq!(
        app.tabs[0].scratch.cursor(),
        (0, 8),
        "past the end is the end"
    );
    click(&mut app, cell(2, 3, 9));
    assert_eq!(
        app.tabs[0].scratch.cursor(),
        (1, 3),
        "under the last line is the last line"
    );
}

#[test]
fn a_click_on_the_pad_focuses_it_and_is_not_an_edit() {
    let mut app = typed("select 1\nfrom dual");
    app.shell.focus = Focus::Objects;
    let scratch = &mut app.tabs[0].scratch;
    scratch.saved();
    let now = Instant::now();
    scratch.settle(now);
    scratch.settle(now + crate::app::scratch::SETTLE);
    assert!(!scratch.settling());

    click(&mut app, cell(2, 3, 1));
    drag(&mut app, cell(2, 5, 0));
    assert_eq!(app.shell.focus, Focus::Scratch);
    let scratch = &app.tabs[0].scratch;
    assert!(
        !scratch.modified() && !scratch.settling(),
        "nothing to save"
    );
    assert!(pad_line(&frame(120, 40, &app), 1).starts_with("╭ Scratch ──"));
}

#[test]
fn the_wheel_scrolls_three_lines_and_pulls_the_cursor_along() {
    let text: Vec<String> = (1..=40).map(|n| format!("select {n}")).collect();
    let mut app = typed(&text.join("\n"));
    let over = cell(3, 0, 5);
    pointer(
        &mut app,
        MouseEventKind::ScrollDown,
        over,
        KeyModifiers::NONE,
    );
    let terminal = frame(120, 40, &app);
    assert_eq!(pad_line(&terminal, 2), row(" 4 select 4"));
    assert_eq!(app.tabs[0].scratch.cursor(), (3, 0), "pulled onto the view");

    pointer(&mut app, MouseEventKind::ScrollUp, over, KeyModifiers::NONE);
    assert_eq!(pad_line(&frame(120, 40, &app), 2), row(" 1 select 1"));
    assert_eq!(
        app.tabs[0].scratch.cursor(),
        (3, 0),
        "still on it, so left be"
    );

    for _ in 0..20 {
        pointer(
            &mut app,
            MouseEventKind::ScrollDown,
            over,
            KeyModifiers::NONE,
        );
    }
    let terminal = frame(120, 40, &app);
    assert_eq!(
        pad_line(&terminal, 14),
        row("40 select 40"),
        "no further than the last line"
    );
    assert_eq!(pad_line(&terminal, 2), row("28 select 28"));
}
