//! What the frame says can be clicked, and what a click there does.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::Instant;

use super::*;
use crate::app::Action;
use crate::app::results::Inspector;
use crate::db::model::{Cell, Column, QueryEvent};

/// The hits of one 120x40 frame of `app`.
fn hits(app: &App) -> Hits {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("a test terminal");
    let mut hits = Hits::default();
    terminal
        .draw(|frame| hits = render(frame, app, &Theme::new(false)))
        .expect("a frame");
    hits
}

/// One mouse event at `(x, y)`, against the frame `app` is showing.
fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> Vec<Action> {
    let hits = hits(app);
    let event = MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.pointer(event, Instant::now(), &hits)
}

fn click(app: &mut App, x: u16, y: u16) -> Vec<Action> {
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y)
}

const OBJECTS: (u16, u16) = (5, 10);
const SCRATCH: (u16, u16) = (60, 5);
const RESULTS: (u16, u16) = (60, 30);

#[test]
fn every_tab_label_is_a_target_the_width_it_is_drawn() {
    let app = two_tabs();
    let terminal = frame(120, 40, &app);
    assert_eq!(line(&terminal, 0), " 1 local-mssql ○  2 local-oracle ○");
    let hits = hits(&app);
    assert_eq!(
        hits.at(Position::new(1, 0)),
        Some((Rect::new(1, 0, 15, 1), Target::Tab(0)))
    );
    assert_eq!(
        hits.at(Position::new(33, 0)),
        Some((Rect::new(18, 0, 16, 1), Target::Tab(1)))
    );
    assert_eq!(hits.at(Position::new(17, 0)), None, "the gap between them");
    assert_eq!(hits.at(Position::new(34, 0)), None, "past the last one");
}

#[test]
fn a_click_on_a_tab_shows_it_and_a_click_in_a_pane_focuses_it() {
    let mut app = two_tabs();
    assert!(click(&mut app, 20, 0).is_empty());
    assert_eq!(app.shell.active_tab, 1);
    for (focus, (x, y)) in [
        (Focus::Scratch, SCRATCH),
        (Focus::Results, RESULTS),
        (Focus::Objects, OBJECTS),
    ] {
        click(&mut app, x, y);
        assert_eq!(app.shell.focus, focus);
    }
    assert_eq!(app.shell.active_tab, 1, "no pane is a tab");
}

#[test]
fn a_right_click_does_what_a_left_click_does() {
    let mut app = two_tabs();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Right), 20, 0);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Right), 20, 0);
    assert_eq!(app.shell.active_tab, 1);
}

#[test]
fn a_press_that_slides_off_its_target_is_taken_back() {
    let mut app = two_tabs();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 20, 0);
    assert_eq!(app.shell.active_tab, 0, "a press alone does nothing");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 5, 0);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 20, 0);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 20, 0);
    assert_eq!(app.shell.active_tab, 0, "it left the tab on the way");

    let (x, y) = SCRATCH;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 20, 0);
    assert_eq!(app.shell.focus, Focus::Objects, "released somewhere else");
    assert_eq!(app.shell.active_tab, 0);
}

#[test]
fn a_click_beside_the_help_closes_it_and_reaches_nothing_under_it() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("?")));
    let (x, y) = SCRATCH;
    assert_eq!(
        hits(&app).at(Position::new(x, y)).map(|(_, target)| target),
        Some(Target::Outside),
    );
    click(&mut app, x, y);
    assert!(!app.shell.help);
    assert_eq!(
        app.shell.focus,
        Focus::Objects,
        "the focus stayed where it was"
    );

    app.handle(Event::Key(key("?")));
    click(&mut app, 20, 0);
    assert!(!app.shell.help);
    assert_eq!(app.shell.active_tab, 0, "the tab under it was not clicked");
}

#[test]
fn a_click_inside_the_help_leaves_it_open() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("?")));
    let terminal = frame(120, 40, &app);
    let top = (0..40)
        .find(|y| line(&terminal, *y).contains("╭ Help"))
        .expect("the help's top border");
    click(&mut app, 60, top + 1);
    assert!(app.shell.help);
}

#[test]
fn the_wheel_scrolls_the_help_under_it_and_nothing_else() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("?")));
    let terminal = frame(120, 40, &app);
    let top = (0..40)
        .find(|y| line(&terminal, *y).contains("╭ Help"))
        .expect("the help's top border");
    mouse(&mut app, MouseEventKind::ScrollDown, 60, top + 1);
    assert_eq!(app.shell.help_scroll, 3);
    mouse(&mut app, MouseEventKind::ScrollUp, 60, top + 1);
    mouse(&mut app, MouseEventKind::ScrollUp, 60, top + 1);
    assert_eq!(app.shell.help_scroll, 0);
    mouse(&mut app, MouseEventKind::ScrollDown, 1, 20);
    assert_eq!(app.shell.help_scroll, 0, "beside it");
    assert!(app.shell.help);
}

/// An app with the inspector open on a cell sixty lines long.
fn inspecting() -> App {
    let mut app = two_tabs();
    let results = &mut app.tabs[0].results;
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(vec![Column {
        name: "body".to_owned(),
        type_name: "nvarchar".to_owned(),
    }]));
    results.apply(QueryEvent::Rows(vec![vec![Cell::Text(
        "a line\n".repeat(60),
    )]]));
    app.shell.focus = Focus::Results;
    app.shell.inspector = Some(Inspector::default());
    app
}

#[test]
fn the_wheel_scrolls_the_inspector_and_a_click_beside_it_closes_it() {
    let mut app = inspecting();
    mouse(&mut app, MouseEventKind::ScrollDown, 60, 20);
    assert_eq!(app.shell.inspector, Some(Inspector { scroll: 3 }));
    assert_eq!(app.shell.focus, Focus::Results);
    click(&mut app, OBJECTS.0, OBJECTS.1);
    assert_eq!(app.shell.inspector, None);
    assert_eq!(
        app.shell.focus,
        Focus::Results,
        "the pane under it was not clicked"
    );
}

#[test]
fn a_click_beside_the_export_prompt_gives_it_up() {
    let mut app = inspecting();
    app.shell.inspector = None;
    app.handle(Event::Key(key("e")));
    assert!(app.shell.prompt.is_some());
    click(&mut app, 20, 0);
    assert_eq!(app.shell.prompt, None);
    assert_eq!(app.shell.active_tab, 0, "and nothing under it was clicked");
}

#[test]
fn the_pointer_lights_up_a_tab_and_nothing_under_an_overlay() {
    let theme = Theme::new(false);
    let mut app = two_tabs();
    app.shell.mouse.pointer = Some(Position::new(20, 0));
    let terminal = frame(120, 40, &app);
    let lit = |terminal: &Terminal<TestBackend>, x: u16| {
        let cell = &terminal.backend().buffer()[(x, 0)];
        Style::new().fg(cell.fg).bg(cell.bg) == theme.hover
    };
    assert!((18..34).all(|x| lit(&terminal, x)), "the whole label");
    assert!(!lit(&terminal, 17) && !lit(&terminal, 34) && !lit(&terminal, 1));
    assert_eq!(line(&terminal, 0), " 1 local-mssql ○  2 local-oracle ○");

    app.shell.help = true;
    let terminal = frame(120, 40, &app);
    assert!(!(18..34).any(|x| lit(&terminal, x)), "the help is over it");

    app.shell.help = false;
    app.shell.mouse.pointer = Some(Position::new(SCRATCH.0, SCRATCH.1));
    let terminal = frame(120, 40, &app);
    let buffer = terminal.backend().buffer();
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| Some(cell.bg) != theme.hover.bg),
        "a pane is not a thing to light up"
    );
}

#[test]
fn no_color_lights_nothing_up() {
    let plain = Theme::new(true);
    let mut app = two_tabs();
    let away = frame_with(120, 40, &app, &plain);
    app.shell.mouse.pointer = Some(Position::new(20, 0));
    let over = frame_with(120, 40, &app, &plain);
    assert_eq!(over.backend().buffer(), away.backend().buffer());
}
