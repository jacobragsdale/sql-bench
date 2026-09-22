//! What the frame says can be clicked, and what a click there does.

use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::Instant;

use super::*;
use crate::app::pointer::{Menu, Mouse, Seam, Split, menu};
use crate::app::results::{Inspector, Results};
use crate::app::tests::browsed;
use crate::app::{Action, RuntimeEvent};
use crate::config::Kind;
use crate::db::catalog::{CatalogAnswer, CatalogRequest, ObjectKind};
use crate::db::model::{Cell, Column, QueryEvent};

/// The hits of one 120x40 frame of `app`.
fn hits(app: &App) -> Hits {
    drawn(app, 120, 40).0
}

/// One mouse event at `(x, y)`, against the frame `app` is showing.
fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> Vec<Action> {
    mouse_at(app, kind, (x, y), Instant::now())
}

fn mouse_at(app: &mut App, kind: MouseEventKind, (x, y): (u16, u16), now: Instant) -> Vec<Action> {
    let hits = hits(app);
    let event = MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.pointer(event, now, &hits)
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
    assert_eq!(
        line(&terminal, 0),
        bar(&terminal, " 1 local-mssql ○  2 local-oracle ○")
    );
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
    // Its title: the rows are buttons of their own.
    click(&mut app, 60, top);
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
    assert_eq!(
        line(&terminal, 0),
        bar(&terminal, " 1 local-mssql ○  2 local-oracle ○")
    );

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

/// One button as a frame drew it: where, what it presses, and what it says.
type Drawn = (Rect, Focus, KeyEvent, String);

/// The hits of one `width`x`height` frame of `app`, and every button on it
/// that a click can reach: not one under an overlay.
fn drawn(app: &App, width: u16, height: u16) -> (Hits, Vec<Drawn>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    let mut hits = Hits::default();
    terminal
        .draw(|frame| hits = render(frame, app, &Theme::new(false)))
        .expect("a frame");
    let buffer = terminal.backend().buffer();
    let buttons = hits
        .regions()
        .filter(|(rect, target)| hits.at(rect.as_position()) == Some((*rect, *target)))
        .filter_map(|(rect, target)| match target {
            Target::Button { pane, key } => {
                let label: String = (rect.x..rect.right())
                    .map(|x| buffer[(x, rect.y)].symbol())
                    .collect();
                Some((rect, pane, key, label.trim().to_owned()))
            }
            _ => None,
        })
        .collect();
    (hits, buttons)
}

/// What each button on a 120x40 frame of `app` says, in paint order.
fn labels(app: &App) -> Vec<String> {
    drawn(app, 120, 40)
        .1
        .into_iter()
        .map(|(.., label)| label)
        .collect()
}

/// A connected tab with a tree to move about, a statement in the pad and
/// nothing run yet.
fn idle() -> App {
    let mut app = two_tabs();
    app.tabs[0].state = TabState::Connected;
    app.tabs[0].objects = browsed(Kind::Mssql);
    app.tabs[0].scratch.set_text("select 1 as one");
    app
}

/// A result set of `rows` rows of one column, from a statement that has
/// started and not finished.
fn rows(results: &mut Results, rows: i64) {
    results.apply(QueryEvent::Columns(vec![Column {
        name: "n".to_owned(),
        type_name: "int".to_owned(),
    }]));
    results.apply(QueryEvent::Rows(
        (0..rows).map(|row| vec![Cell::Int(row)]).collect(),
    ));
}

fn done(results: &mut Results, truncated: bool) {
    results.apply(QueryEvent::Done {
        rows: 3,
        truncated,
        connect_ms: 1,
        first_row_ms: 2,
        total_ms: 3,
    });
}

fn running() -> App {
    let mut app = idle();
    app.apply(RuntimeEvent::QueryStarted {
        tab: 0,
        at: Instant::now(),
        statement: 0,
        of: 1,
        keep_view: false,
    });
    rows(&mut app.tabs[0].results, 3);
    app
}

fn truncated() -> App {
    let mut app = running();
    done(&mut app.tabs[0].results, true);
    app.shell.status.clear();
    app
}

fn multi_set() -> App {
    let mut app = running();
    rows(&mut app.tabs[0].results, 2);
    done(&mut app.tabs[0].results, false);
    app.shell.status.clear();
    app
}

/// A filter nothing matches, typed and committed.
fn filtered() -> App {
    let mut app = idle();
    for spec in ["/", "z", "z", "z", "Enter"] {
        app.tabs[0].objects.key(key(spec));
    }
    app
}

fn failed() -> App {
    let mut app = two_tabs();
    app.tabs[0].state = TabState::Failed("localhost:1433: refused".to_owned());
    app.shell.error = Some("could not connect to local-mssql".to_owned());
    app
}

fn source_view() -> App {
    let mut app = multi_set();
    app.tabs[0]
        .results
        .show_source("bench.p".to_owned(), "create procedure p\nas\nselect 1");
    app
}

fn prompting() -> App {
    let mut app = truncated();
    app.shell.focus = Focus::Results;
    app.shell.prompt = Some(crate::app::prompt::Prompt::new("~/out.csv".to_owned()));
    app
}

/// The app with its mouse state forgotten, and the export prompt's
/// prefill without its stamp: it is the clock to the second, and the two
/// copies a parity check compares may straddle one.
fn settled(mut app: App) -> App {
    app.shell.mouse = Mouse::default();
    if let Some(prompt) = app.shell.prompt.as_mut() {
        prompt.text = prompt.text.replace(|c: char| c.is_ascii_digit(), "0");
    }
    app
}

/// Every key with somewhere to go and something to act on: a pad with an
/// edit to undo and a selection to copy, and a grid cursor with room on
/// every side. A frame wide enough shows every footer hint, and none of them
/// may be dead.
fn roomy() -> App {
    let mut app = idle();
    let scratch = &mut app.tabs[0].scratch;
    for spec in [
        "Right",
        "Right",
        "Right",
        "Right",
        "Right",
        "Right",
        "Right",
        "x",
        "Shift-Left",
    ] {
        scratch.handle(key(spec));
    }
    let results = &mut app.tabs[0].results;
    *results = crate::app::tests::filled(50, 4);
    // A second set for `[` and `]`, and back to the first, which has the
    // rows to move about in.
    rows(results, 2);
    done(results, true);
    for spec in ["[", "PageDown", "PageDown", "l"] {
        results.key(key(spec));
    }
    app
}

/// Click each button `app` draws at `width`x`height` on one copy and press
/// its key on another, and say what was wrong with each one that differed
/// or did nothing.
fn parity(app: &App, (width, height): (u16, u16), state: &str) -> (usize, Vec<String>) {
    let (hits, buttons) = drawn(app, width, height);
    let mut wrong = Vec::new();
    for (rect, pane, key, label) in &buttons {
        let at = |kind| MouseEvent {
            kind,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        let mut clicked = app.clone();
        let now = Instant::now();
        clicked.pointer(at(MouseEventKind::Down(MouseButton::Left)), now, &hits);
        let by_mouse = clicked.pointer(at(MouseEventKind::Up(MouseButton::Left)), now, &hits);
        let mut pressed = app.clone();
        pressed.shell.focus = *pane;
        let by_key = pressed.handle(Event::Key(*key));
        let mut before = app.clone();
        before.shell.focus = *pane;
        let (clicked, pressed, before) = (settled(clicked), settled(pressed), settled(before));
        let what = format!("{label} in {state}, {:?} focused", app.shell.focus);
        if clicked != pressed || by_mouse != by_key {
            wrong.push(format!("{what}: not its key"));
        } else if by_mouse.is_empty() && clicked == before {
            wrong.push(format!("{what}: does nothing"));
        }
    }
    (buttons.len(), wrong)
}

/// Sixty tables in `dbo`, more than the tree has rows for.
fn tall_tree(app: &mut App) {
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::Table,
    };
    let tables = (0..60)
        .map(|n| crate::app::tests::object("dbo", &format!("t{n:03}"), ObjectKind::Table))
        .collect();
    app.apply(RuntimeEvent::Catalog {
        tab: 0,
        request,
        result: Ok(CatalogAnswer::Objects(tables)),
    });
}

/// The tree, the grid and the pad each longer than their pane and scrolled
/// part way, so each has track above its thumb and below it.
fn scrolled() -> App {
    let mut app = deep();
    tall_tree(&mut app);
    for _ in 0..3 {
        app.tabs[0].objects.key(key("PageDown"));
    }
    let text: Vec<String> = (1..=40).map(|n| format!("select {n}")).collect();
    let scratch = &mut app.tabs[0].scratch;
    scratch.set_text(&text.join("\n"));
    for _ in 0..20 {
        scratch.handle(key("Down"));
    }
    app
}

/// An object's source a hundred lines long, scrolled part way.
fn long_source() -> App {
    let mut app = idle();
    let text: Vec<String> = (1..=100).map(|n| format!("-- line {n}")).collect();
    let results = &mut app.tabs[0].results;
    results.show_source("bench.p".to_owned(), &text.join("\n"));
    for _ in 0..4 {
        results.key(key("PageDown"));
    }
    app
}

/// A state the parity test is run in, and how to get an app into it.
type State = (&'static str, fn() -> App);

#[test]
fn every_button_drawn_is_exactly_its_key_and_does_something() {
    let states: [State; 11] = [
        ("idle", idle),
        ("running", running),
        ("truncated", truncated),
        ("multi-set", multi_set),
        ("filtered", filtered),
        ("failed", failed),
        ("source view", source_view),
        ("prompt", prompting),
        ("roomy", roomy),
        ("scrolled", scrolled),
        ("long source", long_source),
    ];
    let mut checked = 0;
    let mut wrong = Vec::new();
    for (state, make) in states {
        for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
            let mut app = make();
            if app.shell.prompt.is_none() {
                app.shell.focus = focus;
            }
            // Only the roomy app has something for every footer hint to do.
            let size = if state == "roomy" {
                (600, 60)
            } else {
                (120, 40)
            };
            let (count, mut found) = parity(&app, size, state);
            checked += count;
            wrong.append(&mut found);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(checked > 150, "only {checked} buttons");
}

/// The first row of a frame of `app` that has `text` in it.
fn row_with(app: &App, (width, height): (u16, u16), text: &str) -> String {
    let terminal = frame(width, height, app);
    (0..height)
        .map(|y| line(&terminal, y))
        .find(|row| row.contains(text))
        .unwrap_or_else(|| panic!("no {text} in\n{}", super::text(&terminal)))
}

/// Where the button that says `label` is on a 120x40 frame of `app`.
fn place(app: &App, label: &str) -> (u16, u16) {
    let (_, buttons) = drawn(app, 120, 40);
    let (rect, ..) = buttons
        .into_iter()
        .find(|(.., said)| said == label)
        .unwrap_or_else(|| panic!("no {label} button"));
    (rect.x, rect.y)
}

const WIDE: (u16, u16) = (120, 40);
const SMALL: (u16, u16) = (60, 15);

#[test]
fn each_pane_draws_a_button_only_where_its_key_does_what_it_says() {
    assert_eq!(
        row_with(&idle(), WIDE, "╭ Objects"),
        format!(
            "╭ Objects ─── ⟳ Reload ─ / Filter ─╮╭ Scratch [modified] {} ✎ Editor ─ ▶▶ All ─ ▶ Run ─╮",
            "─".repeat(34)
        )
    );
    // Esc is Stop and Cancel while a query runs, so it closes no error: the
    // title's clock is the one thing here not asserted.
    let mut running = running();
    running.shell.error = Some("boom".to_owned());
    assert_eq!(
        row_with(&running, WIDE, "╭ Objects"),
        format!(
            "╭ Objects ─── ⟳ Reload ─ / Filter ─╮╭ Scratch [modified] {} ✎ Editor ─ ▶▶ All ─ ▶ Run ─ ■ Stop ─╮",
            "─".repeat(25)
        )
    );
    assert!(
        row_with(&running, WIDE, "╭ Running")
            .ends_with(&format!(" 3 rows {} Export ─ ■ Cancel ─╮", "─".repeat(36)))
    );
    assert_eq!(
        row_with(&running, WIDE, " boom"),
        format!(" boom{}● connected", " ".repeat(103))
    );
    assert_eq!(
        row_with(&multi_set(), WIDE, "╭ Results"),
        format!(
            "│{:34}│╭ Results · set 2/2 · 3 rows · 3 ms {} Export ─ ◀ ─ ▶ ─╮",
            "",
            "─".repeat(30)
        )
    );
    assert_eq!(
        row_with(&truncated(), WIDE, "╭ Results"),
        format!(
            "│{:34}│╭ Results · 3 rows (truncated) · 3 ms {} Export ─ +10k ─╮",
            "",
            "─".repeat(29)
        )
    );
    // [ ] m and e do nothing over an object's source.
    assert_eq!(
        row_with(&source_view(), WIDE, "╭ Source"),
        format!(
            "│{:34}│╭ Source · bench.p · 3 lines {}╮",
            "",
            "─".repeat(54)
        )
    );

    let filtered = filtered();
    assert_eq!(
        row_with(&filtered, WIDE, "╭ Objects")
            .chars()
            .take(36)
            .collect::<String>(),
        "╭ Objects /zzz ───── / Filter ─ × ─╮"
    );
    assert_eq!(
        row_with(&filtered, WIDE, "[ Clear filter ]")
            .chars()
            .take(36)
            .collect::<String>(),
        format!("│ {:32} │", "[ Clear filter ]")
    );
    // Esc would clear the filter rather than the error, and while a query
    // runs it would cancel it rather than either.
    let mut both = filtered.clone();
    both.shell.error = Some("boom".to_owned());
    let crosses = |app: &App| labels(app).iter().filter(|label| *label == "×").count();
    assert_eq!(crosses(&both), 1, "the filter's, and not the error's");
    both.apply(RuntimeEvent::QueryStarted {
        tab: 0,
        at: Instant::now(),
        statement: 0,
        of: 1,
        keep_view: false,
    });
    assert_eq!(crosses(&both), 0);
    assert!(!labels(&both).contains(&"[ Clear filter ]".to_owned()));

    assert_eq!(
        row_with(&failed(), WIDE, " could not connect"),
        format!(
            " could not connect to local-mssql ×{}✗ failed",
            " ".repeat(76)
        )
    );
    assert_eq!(
        row_with(&prompting(), WIDE, " Export to"),
        format!(
            " Export to: ~/out.csv   Export   Cancel{}● connected",
            " ".repeat(69)
        )
    );
}

#[test]
fn at_60x15_every_title_is_whole_and_what_does_not_fit_is_dropped() {
    let mut running = running();
    running.shell.error = Some("boom".to_owned());
    assert_eq!(
        row_with(&running, SMALL, "╭ Objects"),
        format!(
            "╭ Objects {}╮╭ Scratch [modified] ─ ▶ Run ─ ■ Stop ─╮",
            "─".repeat(9)
        )
    );
    assert!(row_with(&running, SMALL, "╭ Running").ends_with(" 3 rows ─ ■ Cancel ─╮"));
    assert_eq!(
        row_with(&multi_set(), SMALL, "╭ Results"),
        "│   ▸ Views        │╭ Results · set 2/2 · 3 rows · 3 ms ───╮"
    );
    assert_eq!(
        row_with(&filtered(), SMALL, "╭ Objects"),
        "╭ Objects /zzz ────╮╭ Scratch [modified] ─ ▶▶ All ─ ▶ Run ─╮"
    );
    assert_eq!(
        row_with(&failed(), SMALL, "╭ Objects"),
        format!(
            "╭ Objects {}╮╭ Scratch ─ ✎ Editor ─ ▶▶ All ─ ▶ Run ─╮",
            "─".repeat(9)
        )
    );
    assert_eq!(
        row_with(&failed(), SMALL, " could not"),
        " could not connect to local-mssql ×                ✗ failed"
    );
}

#[test]
fn a_click_in_objects_is_done_typing_the_filter_before_it_presses_anything() {
    let mut app = idle();
    for spec in ["/", "c"] {
        app.handle(Event::Key(key(spec)));
    }
    let (x, y) = place(&app, "⟳ Reload");
    let actions = click(&mut app, x, y);
    let objects = &app.tabs[0].objects;
    assert_eq!((objects.filter(), objects.filtering()), ("c", false));
    assert!(
        !actions.is_empty() || !app.shell.status.is_empty(),
        "r reloaded"
    );
}

#[test]
fn a_help_row_closes_the_help_and_then_presses_its_key() {
    let mut app = two_tabs();
    app.shell.error = Some("boom".to_owned());
    app.handle(Event::Key(key("?")));
    let terminal = frame(120, 40, &app);
    let y = (0..40)
        .find(|y| line(&terminal, *y).contains("│ Tab        next pane"))
        .expect("the Tab row");
    click(&mut app, 60, y);
    assert!(!app.shell.help);
    assert_eq!(app.shell.focus, Focus::Scratch);

    // Its close button is Esc, which with the help open closes only that.
    app.handle(Event::Key(key("?")));
    let (x, y) = place(&app, "×");
    click(&mut app, x, y);
    assert!(!app.shell.help);
    assert_eq!(app.shell.error.as_deref(), Some("boom"));
}

#[test]
fn the_inspector_closes_from_its_button() {
    let mut app = inspecting();
    let (x, y) = place(&app, "×");
    click(&mut app, x, y);
    assert_eq!(app.shell.inspector, None);
    assert_eq!(app.shell.focus, Focus::Results);
}

#[test]
fn a_click_on_the_prompt_text_puts_the_cursor_there() {
    let mut app = prompting();
    // ` Export to: ` is twelve cells, and the `o` of `~/out.csv` two more.
    click(&mut app, 14, 39);
    assert_eq!(
        app.shell.prompt.as_ref().map(|prompt| prompt.cursor),
        Some(2)
    );
    click(&mut app, 30, 39);
    assert_eq!(
        app.shell.prompt.as_ref().map(|prompt| prompt.cursor),
        None,
        "beside it gives up"
    );

    let mut app = prompting();
    let (x, y) = place(&app, "Cancel");
    assert!(click(&mut app, x, y).is_empty());
    assert_eq!(app.shell.prompt, None);
}

#[test]
fn the_pointer_lights_up_a_button_and_nothing_under_an_overlay() {
    let hover = Theme::new(false).hover;
    let mut app = idle();
    let (x, y) = place(&app, "▶ Run");
    app.shell.mouse.pointer = Some(Position::new(x + 2, y));
    let terminal = frame(120, 40, &app);
    assert!(
        (x..x + 7).all(|x| painted(&terminal, x, y) == hover),
        "the whole of ` ▶ Run `"
    );
    assert_ne!(
        painted(&terminal, x - 1, y),
        hover,
        "not the border beside it"
    );

    app.shell.help = true;
    let terminal = frame(120, 40, &app);
    assert!(
        (x..x + 7).all(|x| painted(&terminal, x, y) != hover),
        "the help is over it"
    );
}

fn click_at(app: &mut App, at: (u16, u16), now: Instant) -> Vec<Action> {
    mouse_at(app, MouseEventKind::Down(MouseButton::Left), at, now);
    mouse_at(app, MouseEventKind::Up(MouseButton::Left), at, now)
}

/// Two clicks at one instant, and what the second one asked for.
fn double_click(app: &mut App, x: u16, y: u16) -> Vec<Action> {
    let now = Instant::now();
    click_at(app, (x, y), now);
    click_at(app, (x, y), now)
}

/// Row `y` of a 120x40 frame, where the Objects pane is empty: the right
/// hand pane's `text`, padded to the inside of it.
fn right(text: &str) -> String {
    format!("│{:34}││ {text:<80} │", "")
}

/// The rows of the grid's pane at 120x40: its two header rows, then the
/// data from row 19.
const GRID: std::ops::Range<u16> = 17..38;

fn lines(terminal: &Terminal<TestBackend>, rows: std::ops::Range<u16>) -> Vec<String> {
    rows.map(|y| line(terminal, y)).collect()
}

/// A focused grid of 500 rows with rows 31 to 49 showing — all 19 that
/// fit at 120x40 — and the cursor on the top one.
fn deep() -> App {
    let mut app = idle();
    app.tabs[0].results = crate::app::tests::filled(500, 4);
    app.shell.focus = Focus::Results;
    for spec in ["j"; 50].into_iter().chain(["k"; 19]) {
        app.handle(Event::Key(key(spec)));
    }
    app
}

/// A row of `deep()`'s grid, drawn: the long text cut to its forty columns.
fn grid_row(row: usize) -> String {
    let long: String = format!("row {row} of a value far too long for one column")
        .chars()
        .take(39)
        .collect();
    right(&format!("{row:>8}  {long}…  NULL         c3r{row}"))
}

#[test]
fn clicking_the_bottom_row_of_the_grid_leaves_the_view_where_it_was() {
    let mut app = deep();
    let before = frame(120, 40, &app);
    assert_eq!(line(&before, 19), grid_row(31));
    assert_eq!(line(&before, 37), grid_row(49));

    assert!(click(&mut app, 40, 37).is_empty());
    assert_eq!(app.tabs[0].results.selected(), (49, 0));
    let after = frame(120, 40, &app);
    assert_eq!(lines(&after, GRID), lines(&before, GRID));

    // column_2, eleven rows down.
    click(&mut app, 95, 30);
    assert_eq!(app.tabs[0].results.selected(), (42, 2));
    assert_eq!(lines(&frame(120, 40, &app), GRID), lines(&before, GRID));
}

#[test]
fn clicking_a_column_the_cursor_went_past_leaves_the_columns_where_they_were() {
    let mut app = idle();
    app.tabs[0].results = crate::app::tests::filled(50, 20);
    app.shell.focus = Focus::Results;
    // `l` keeps its column hint at 0 and lets the window work out the rest,
    // so it is behind what is drawn.
    for _ in 0..10 {
        app.handle(Event::Key(key("l")));
    }
    let before = frame(120, 40, &app);
    let header = right("column_5     column_6  column_7     column_8     column_9  column_10");
    assert_eq!(line(&before, 17), header);

    // column_6, on row 6.
    click(&mut app, 52, 25);
    assert_eq!(app.tabs[0].results.selected(), (6, 6));
    assert_eq!(lines(&frame(120, 40, &app), GRID), lines(&before, GRID));

    // Its header selects the column, and sorts by it, with the columns
    // where they were.
    click(&mut app, 40, 17);
    assert_eq!(app.tabs[0].results.selected(), (6, 5));
    assert_eq!(
        line(&frame(120, 40, &app), 17),
        right("column_5  ▲  column_6  column_7     column_8     column_9  column_10")
    );
}

#[test]
fn a_click_on_a_header_is_o_on_its_column() {
    let mut clicked = idle();
    clicked.tabs[0].results = crate::app::tests::filled(50, 4);
    let mut pressed = clicked.clone();
    pressed.shell.focus = Focus::Results;
    pressed.handle(Event::Key(key("l")));
    // Up, down, and back: column_1 is text, so row 10 comes after row 1.
    let firsts = ["row 0 of", "row 9 of", "row 0 of"];
    for first in firsts {
        let by_mouse = click(&mut clicked, 50, 17);
        let by_key = pressed.handle(Event::Key(key("o")));
        assert_eq!(by_mouse, vec![Action::Sorted { rows: 50 }]);
        assert_eq!(by_mouse, by_key);
        assert_eq!(settled(clicked.clone()), settled(pressed.clone()));
        let cell = clicked.tabs[0].results.rows()[0][1].display().into_owned();
        assert!(cell.starts_with(first), "{cell}");
    }
}

#[test]
fn a_click_below_the_last_row_selects_nothing() {
    let mut app = idle();
    app.tabs[0].results = crate::app::tests::filled(3, 2);
    app.tabs[0].objects.key(key("k"));
    click(&mut app, 40, 30);
    assert_eq!(app.shell.focus, Focus::Results, "it is still the pane");
    assert_eq!(app.tabs[0].results.selected(), (0, 0));
    let cursor = app.tabs[0].objects.cursor();
    click(&mut app, 8, 30);
    assert_eq!(app.tabs[0].objects.cursor(), cursor);
}

#[test]
fn a_double_click_on_a_cell_inspects_it() {
    let mut app = deep();
    assert!(double_click(&mut app, 40, 20).is_empty());
    assert_eq!(app.tabs[0].results.selected(), (32, 0));
    assert_eq!(app.shell.inspector, Some(Inspector::default()));
}

#[test]
fn the_wheel_scrolls_the_grid_and_pulls_the_cursor_along() {
    let mut app = deep();
    mouse(&mut app, MouseEventKind::ScrollDown, 40, 25);
    let terminal = frame(120, 40, &app);
    assert_eq!(line(&terminal, 19), grid_row(34));
    assert_eq!(line(&terminal, 37), grid_row(52));
    assert_eq!(app.tabs[0].results.selected(), (34, 0));

    // At the end the stored hint is past what is drawn, so the wheel starts
    // from what is drawn or it would scroll into the clamp and not move.
    app.handle(Event::Key(key("G")));
    assert_eq!(line(&frame(120, 40, &app), 19), grid_row(481));
    mouse(&mut app, MouseEventKind::ScrollUp, 40, 25);
    let terminal = frame(120, 40, &app);
    assert_eq!(line(&terminal, 19), grid_row(478));
    assert_eq!(line(&terminal, 37), thumbed(&grid_row(496)));
    assert_eq!(app.tabs[0].results.selected(), (496, 0));
}

#[test]
fn the_sideways_wheel_scrolls_the_columns_a_column_a_notch() {
    let mut app = idle();
    app.tabs[0].results = crate::app::tests::filled(50, 20);
    app.shell.focus = Focus::Results;
    mouse(&mut app, MouseEventKind::ScrollRight, 40, 25);
    assert_eq!(
        line(&frame(120, 40, &app), 17),
        right(&format!(
            "{:42}{:13}{:10}column_4",
            "column_1", "column_2", "column_3"
        ))
    );
    assert_eq!(app.tabs[0].results.selected(), (0, 1));
    mouse(&mut app, MouseEventKind::ScrollLeft, 40, 17);
    assert_eq!(
        line(&frame(120, 40, &app), 17),
        right(&format!(
            "{:10}{:42}{:13}column_3",
            "column_0", "column_1", "column_2"
        ))
    );
    assert_eq!(app.tabs[0].results.selected(), (0, 0));
}

#[test]
fn the_wheel_scrolls_an_objects_source() {
    let mut app = idle();
    let text: Vec<String> = (1..=100).map(|n| format!("line {n}")).collect();
    app.tabs[0]
        .results
        .show_source("dbo.p".to_owned(), &text.join("\n"));
    mouse(&mut app, MouseEventKind::ScrollDown, 60, 25);
    mouse(&mut app, MouseEventKind::ScrollDown, 60, 25);
    assert_eq!(line(&frame(120, 40, &app), 17), right("  7 line 7"));
    assert_eq!(app.shell.focus, Focus::Objects, "the wheel moves no focus");
}

#[test]
fn a_click_on_a_row_of_the_tree_moves_the_cursor_and_on_its_glyph_opens_it() {
    let mut app = idle();
    assert!(click(&mut app, 9, 5).is_empty());
    let objects = &app.tabs[0].objects;
    assert_eq!(objects.nodes()[objects.cursor()].item.name(), "orders");
    assert_eq!(
        line(&frame(120, 40, &app), 5)
            .chars()
            .take(36)
            .collect::<String>(),
        format!("│     ▸ orders{:21}│", "")
    );

    // customers' ▸: its columns are loaded.
    assert_eq!(
        click(&mut app, 6, 4),
        vec![Action::LoadObjects {
            tab: 0,
            request: CatalogRequest::Columns {
                schema: "dbo".to_owned(),
                table: "customers".to_owned(),
                show: false,
            },
        }]
    );
    // Tables' ▾ closes it.
    click(&mut app, 5, 3);
    let terminal = frame(120, 40, &app);
    assert_eq!(
        lines(&terminal, 2..6)
            .iter()
            .map(|row| row.chars().take(36).collect::<String>())
            .collect::<Vec<_>>(),
        [
            format!("│ ▾ dbo{:28}│", ""),
            format!("│   ▸ Tables{:23}│", ""),
            format!("│   ▸ Views{:24}│", ""),
            format!("│   ▸ Procedures{:19}│", ""),
        ]
    );
}

#[test]
fn a_double_click_on_a_table_drops_its_select_into_the_pad_as_enter_does() {
    let mut app = idle();
    let mut pressed = app.clone();
    pressed.shell.focus = Focus::Objects;
    let by_key: Vec<Action> = ["j", "Enter"]
        .into_iter()
        .flat_map(|spec| pressed.handle(Event::Key(key(spec))))
        .collect();
    assert_eq!(double_click(&mut app, 9, 5), by_key);
    assert_eq!(settled(app.clone()), settled(pressed));
    assert_eq!(app.shell.focus, Focus::Scratch);
    assert!(
        line(&frame(120, 40, &app), 2).starts_with(
            "│ ▾ dbo                            ││ 1 select top 100 * from dbo.orders "
        ),
    );
}

#[test]
fn a_double_click_on_a_procedure_shows_its_source_as_enter_does() {
    let mut app = idle();
    // A branch opens on a double-click, the way Enter opens it.
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::Procedure,
    };
    assert_eq!(
        double_click(&mut app, 9, 7),
        vec![Action::LoadObjects {
            tab: 0,
            request: request.clone(),
        }]
    );
    app.apply(RuntimeEvent::Catalog {
        tab: 0,
        request,
        result: Ok(CatalogAnswer::Objects(vec![crate::app::tests::object(
            "dbo",
            "refresh",
            ObjectKind::Procedure,
        )])),
    });
    let mut pressed = app.clone();
    pressed.handle(Event::Key(key("j")));
    let by_key = pressed.handle(Event::Key(key("Enter")));
    let actions = double_click(&mut app, 9, 8);
    assert_eq!(actions, by_key);
    assert_eq!(settled(app.clone()), settled(pressed));
    let Some(Action::LoadObjects { request, .. }) = actions.into_iter().next() else {
        panic!("no load");
    };
    app.apply(RuntimeEvent::Catalog {
        tab: 0,
        request,
        result: Ok(CatalogAnswer::Source(
            "create procedure dbo.refresh".to_owned(),
        )),
    });
    assert_eq!(
        row_with(&app, WIDE, "╭ Source"),
        format!(
            "│{:34}│╭ Source · dbo.refresh · 1 lines {}╮",
            "",
            "─".repeat(50)
        )
    );
    assert_eq!(
        line(&frame(120, 40, &app), 17),
        right("1 create procedure dbo.refresh")
    );
}

#[test]
fn the_wheel_scrolls_the_tree_and_pulls_the_cursor_along() {
    let mut app = idle();
    tall_tree(&mut app);
    let name = |app: &App| {
        let objects = &app.tabs[0].objects;
        objects.nodes()[objects.cursor()].item.name()
    };
    assert_eq!(name(&app), "Tables");
    mouse(&mut app, MouseEventKind::ScrollDown, 8, 10);
    let first = |app: &App| {
        line(&frame(120, 40, app), 2)
            .chars()
            .take(36)
            .collect::<String>()
    };
    assert_eq!(first(&app), format!("│     ▸ t001{:23}│", ""));
    assert_eq!(name(&app), "t001");
    mouse(&mut app, MouseEventKind::ScrollUp, 8, 10);
    mouse(&mut app, MouseEventKind::ScrollUp, 8, 10);
    assert_eq!(first(&app), format!("│ ▾ dbo{:28}┃", ""));
    assert_eq!(name(&app), "t001", "still showing, so it stays");
    assert_eq!(app.shell.focus, Focus::Objects);
}

/// Each scrollbar's track buttons on a 120x40 frame of `app`: where, which
/// pane, and the key, top to bottom.
fn tracks(app: &App) -> Vec<(Rect, Focus, String)> {
    drawn(app, 120, 40)
        .1
        .into_iter()
        .filter(|(.., label)| label == "│")
        .map(|(rect, pane, key, _)| (rect, pane, format!("{:?}", key.code)))
        .collect()
}

/// Where the thumb of `pane`'s scrollbar is on a 120x40 frame of `app`.
fn thumb_of(app: &App, pane: Focus) -> Rect {
    hits(app)
        .regions()
        .find_map(|(rect, target)| match target {
            Target::Thumb { pane: of, .. } if of == pane => Some(rect),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {pane:?} thumb"))
}

#[test]
fn every_scrollbar_is_a_page_up_and_a_page_down_either_side_of_its_thumb() {
    let theirs = |app: &App| {
        tracks(app)
            .into_iter()
            .map(|(rect, pane, key)| (rect.x, pane, key))
            .collect::<Vec<_>>()
    };
    let page = |x, pane, key: &str| (x, pane, key.to_owned());
    let wanted = vec![
        page(35, Focus::Objects, "PageUp"),
        page(35, Focus::Objects, "PageDown"),
        page(119, Focus::Scratch, "PageUp"),
        page(119, Focus::Scratch, "PageDown"),
        page(119, Focus::Results, "PageUp"),
        page(119, Focus::Results, "PageDown"),
    ];
    assert_eq!(theirs(&scrolled()), wanted);
    assert_eq!(
        theirs(&long_source()),
        vec![
            page(119, Focus::Results, "PageUp"),
            page(119, Focus::Results, "PageDown"),
        ]
    );
}

#[test]
fn a_click_on_the_track_is_page_down() {
    let mut clicked = scrolled();
    let thumb = thumb_of(&clicked, Focus::Results);
    let mut pressed = clicked.clone();
    click(&mut clicked, thumb.x, thumb.bottom() + 2);
    pressed.handle(Event::Key(key("PageDown")));
    assert_eq!(settled(clicked.clone()), settled(pressed));
    assert_eq!(clicked.tabs[0].results.selected(), (41, 0));
}

#[test]
fn there_is_no_scrollbar_where_everything_fits() {
    for app in [idle(), multi_set(), source_view()] {
        let terminal = frame(120, 40, &app);
        assert!(!text(&terminal).contains('┃'), "{}", text(&terminal));
        assert!(
            !hits(&app)
                .regions()
                .any(|(_, target)| matches!(target, Target::Thumb { .. }))
        );
    }
}

#[test]
fn the_grid_scrollbar_spans_its_rows_and_the_thumb_follows_them() {
    // Rows 31 to 49 of 500, nineteen showing on nineteen cells: a one-cell
    // thumb, one cell down.
    let mut app = deep();
    let terminal = frame(120, 40, &app);
    assert_eq!(thumb_of(&app, Focus::Results), Rect::new(119, 20, 1, 1));
    assert_eq!(line(&terminal, 19), grid_row(31));
    assert_eq!(line(&terminal, 20), thumbed(&grid_row(32)));
    // The two header rows above the first row are not track.
    let track = tracks(&app);
    assert_eq!(track[0].0, Rect::new(119, 19, 1, 1));
    assert_eq!(track[1].0, Rect::new(119, 21, 1, 17));
    for (spec, y) in [("g", 19), ("G", 37)] {
        app.handle(Event::Key(key(spec)));
        assert_eq!(thumb_of(&app, Focus::Results), Rect::new(119, y, 1, 1));
    }
}

#[test]
fn dragging_the_thumb_to_the_bottom_shows_the_last_row() {
    let mut app = deep();
    let thumb = thumb_of(&app, Focus::Results);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        thumb.x,
        thumb.y,
    );
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 100, 39);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 100, 39);
    let terminal = frame(120, 40, &app);
    assert_eq!(line(&terminal, 19), grid_row(481));
    assert_eq!(line(&terminal, 37), thumbed(&grid_row(499)));
    assert_eq!(
        app.tabs[0].results.selected(),
        (481, 0),
        "pulled along into the view"
    );

    // And back up to the middle, by the cell it was grabbed on.
    let thumb = thumb_of(&app, Focus::Results);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        thumb.x,
        thumb.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        thumb.x,
        28,
    );
    assert_eq!(thumb_of(&app, Focus::Results), Rect::new(119, 28, 1, 1));
    assert_eq!(app.shell.focus, Focus::Results);
}

#[test]
fn dragging_a_thumb_scrolls_each_pane_and_keeps_its_cursor_in_view() {
    let mut app = scrolled();
    app.shell.focus = Focus::Results;
    for pane in [Focus::Objects, Focus::Scratch] {
        let thumb = thumb_of(&app, pane);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            thumb.x,
            thumb.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            thumb.x,
            1,
        );
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), thumb.x, 1);
    }
    let terminal = frame(120, 40, &app);
    assert_eq!(
        line(&terminal, 2),
        format!("│ ▾ dbo{:28}┃│  1 select 1{:70}┃", "", "")
    );
    assert_eq!(app.tabs[0].scratch.cursor().0, 12, "the pad's last row");
    assert_eq!(app.shell.focus, Focus::Results, "the drag is the wheel's");

    let mut app = long_source();
    let thumb = thumb_of(&app, Focus::Results);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        thumb.x,
        thumb.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        thumb.x,
        39,
    );
    assert_eq!(
        line(&frame(120, 40, &app), 37),
        thumbed(&right("100 -- line 100"))
    );
}

#[test]
fn a_thumb_pressed_and_let_go_does_nothing() {
    let mut app = scrolled();
    let before = app.clone();
    for pane in [Focus::Objects, Focus::Scratch, Focus::Results] {
        let thumb = thumb_of(&app, pane);
        click(&mut app, thumb.x, thumb.y);
    }
    assert_eq!(settled(app), settled(before));
}

#[test]
fn the_pointer_lights_up_the_thumb_it_rests_on() {
    let hover = Theme::new(false).hover;
    let mut app = deep();
    let thumb = thumb_of(&app, Focus::Results);
    app.shell.mouse.pointer = Some(thumb.as_position());
    let terminal = frame(120, 40, &app);
    let lit = |y| {
        let cell = &terminal.backend().buffer()[(thumb.x, y)];
        Style::new().fg(cell.fg).bg(cell.bg) == hover
    };
    assert!(lit(thumb.y));
    assert!(!lit(thumb.y + 1), "the track under it is not the thumb");
}

#[test]
fn at_60x15_the_scrollbars_leave_the_titles_and_their_buttons_alone() {
    let app = scrolled();
    let terminal = frame(60, 15, &app);
    assert_eq!(
        line(&terminal, 1),
        "╭ Objects ─────────╮╭ Scratch [modified] ─ ▶▶ All ─ ▶ Run ─╮"
    );
    assert_eq!(
        line(&terminal, 3),
        format!("│     ▸ t021       ││ 20 select 20{:25}┃", "")
    );
    assert_eq!(
        line(&terminal, 5),
        format!("│     ▸ t023       │╰{}╯", "─".repeat(38))
    );
    assert_eq!(
        line(&terminal, 6),
        "│     ▸ t024       ┃╭ Results · 500 rows · 42 ms ─ Export ─╮"
    );
    assert_eq!(
        line(&terminal, 13),
        format!("╰{}╯╰{}╯", "─".repeat(18), "─".repeat(38))
    );
}

/// One mouse event at `at`, against a `width`x`height` frame of `app`.
fn mouse_on(app: &mut App, (width, height): (u16, u16), kind: MouseEventKind, at: (u16, u16)) {
    let hits = drawn(app, width, height).0;
    let event = MouseEvent {
        kind,
        column: at.0,
        row: at.1,
        modifiers: KeyModifiers::NONE,
    };
    app.pointer(event, Instant::now(), &hits);
}

/// `seam` pressed half way along, dragged to `to` and let go there.
fn drag(app: &mut App, size: (u16, u16), seam: Seam, to: (u16, u16)) {
    let from = seam_of(app, size, seam);
    mouse_on(app, size, MouseEventKind::Down(MouseButton::Left), from);
    mouse_on(app, size, MouseEventKind::Drag(MouseButton::Left), to);
    mouse_on(app, size, MouseEventKind::Up(MouseButton::Left), to);
}

/// A cell of `seam` on a `width`x`height` frame of `app`, half way along.
fn seam_of(app: &App, (width, height): (u16, u16), seam: Seam) -> (u16, u16) {
    let (rect, _) = drawn(app, width, height)
        .0
        .regions()
        .find(|(_, target)| matches!(target, Target::Seam { seam: of, .. } if *of == seam))
        .unwrap_or_else(|| panic!("no {seam:?} seam"));
    (rect.x + rect.width / 2, rect.y + rect.height / 2)
}

/// Objects, Scratch and Results at 120x40 before any seam is moved.
const SPLIT: [(u16, u16); 3] = [(0, 1), (36, 1), (36, 16)];

#[test]
fn dragging_each_seam_moves_the_borders_either_side_of_it() {
    let mut app = idle();
    assert_eq!(corners(&frame(120, 40, &app)), SPLIT);
    drag(&mut app, WIDE, Seam::Objects, (50, 10));
    assert_eq!(corners(&frame(120, 40, &app)), [(0, 1), (50, 1), (50, 16)]);
    drag(&mut app, WIDE, Seam::Scratch, (80, 25));
    let terminal = frame(120, 40, &app);
    assert_eq!(corners(&terminal), [(0, 1), (50, 1), (50, 26)]);
    assert!(line(&terminal, 25).ends_with(&format!("╰{}╯", "─".repeat(68))));
    assert_eq!(app.shell.focus, Focus::Objects, "a drag focuses nothing");
}

#[test]
fn every_pane_keeps_its_least_however_far_a_seam_goes_and_whatever_the_size() {
    for (width, height) in [SMALL, (200, 60)] {
        let size = (width, height);
        let mut app = idle();
        drag(&mut app, size, Seam::Objects, (0, 5));
        drag(&mut app, size, Seam::Scratch, (30, 0));
        assert_eq!(
            corners(&frame(width, height, &app)),
            [(0, 1), (20, 1), (20, 4)],
            "Objects 20 wide and Scratch 3 high at {width}x{height}"
        );
        drag(&mut app, size, Seam::Objects, (width - 1, 5));
        drag(&mut app, size, Seam::Scratch, (50, height - 1));
        let right = width - 20;
        assert_eq!(
            corners(&frame(width, height, &app)),
            [(0, 1), (right, 1), (right, height - 4)],
            "the right column 20 wide and Results 3 high at {width}x{height}"
        );
    }
    // A split made on a big screen still holds on a small one.
    let mut app = idle();
    let big = (200, 60);
    drag(&mut app, big, Seam::Objects, (199, 5));
    drag(&mut app, big, Seam::Scratch, (190, 59));
    assert_eq!(corners(&frame(60, 15, &app)), [(0, 1), (40, 1), (40, 11)]);
}

#[test]
fn a_double_click_on_a_seam_puts_the_split_back() {
    let mut app = idle();
    drag(&mut app, WIDE, Seam::Objects, (80, 10));
    drag(&mut app, WIDE, Seam::Scratch, (100, 30));
    for seam in [Seam::Objects, Seam::Scratch] {
        let (x, y) = seam_of(&app, WIDE, seam);
        double_click(&mut app, x, y);
    }
    assert_eq!(app.shell.split, Split::default());
    assert_eq!(corners(&frame(120, 40, &app)), SPLIT);
}

#[test]
fn a_seam_pressed_and_let_go_does_nothing_and_focuses_neither_side() {
    let mut app = idle();
    let before = app.clone();
    for seam in [Seam::Objects, Seam::Scratch] {
        let (x, y) = seam_of(&app, WIDE, seam);
        click(&mut app, x, y);
    }
    assert_eq!(settled(app), settled(before));
}

#[test]
fn the_seams_are_under_the_scrollbars_and_the_title_buttons() {
    let app = scrolled();
    let hits = hits(&app);
    let at = |x, y| hits.at(Position::new(x, y)).map(|(_, target)| target);
    for y in 2..38 {
        assert!(
            matches!(
                at(35, y),
                Some(Target::Thumb { .. } | Target::Button { .. })
            ),
            "Objects' right border is its scrollbar at row {y}"
        );
    }
    let objects = |y| {
        matches!(
            at(36, y),
            Some(Target::Seam {
                seam: Seam::Objects,
                ..
            })
        )
    };
    assert!((1..15).chain(16..39).all(objects));
    let scratch = |x| {
        matches!(
            at(x, 15),
            Some(Target::Seam {
                seam: Seam::Scratch,
                ..
            })
        )
    };
    assert!((36..120).all(scratch), "the corner is the one across");
    let (x, y) = place(&app, "Export");
    assert!(matches!(at(x, y), Some(Target::Button { .. })));
    assert_eq!(y, 16, "on Results' top border, the row under the seam");
}

#[test]
fn a_seam_is_lit_under_the_pointer_and_while_it_is_held() {
    let theme = Theme::new(false);
    let lit = theme.hover.patch(theme.accent);
    let mut app = idle();
    app.shell.mouse.pointer = Some(Position::new(36, 25));
    let terminal = frame(120, 40, &app);
    assert!(
        (1..39).all(|y| painted(&terminal, 36, y) == lit),
        "the whole seam"
    );
    assert_ne!(painted(&terminal, 35, 25), lit);

    // Held and dragged past where it stops: the pointer is over Objects and
    // the seam it is holding is the one lit.
    mouse_on(
        &mut app,
        WIDE,
        MouseEventKind::Down(MouseButton::Left),
        (36, 25),
    );
    mouse_on(
        &mut app,
        WIDE,
        MouseEventKind::Drag(MouseButton::Left),
        (2, 25),
    );
    let terminal = frame(120, 40, &app);
    assert!((1..39).all(|y| painted(&terminal, 20, y) == lit));
    assert_ne!(painted(&terminal, 2, 25), lit);
    mouse_on(
        &mut app,
        WIDE,
        MouseEventKind::Up(MouseButton::Left),
        (2, 25),
    );
    assert_ne!(painted(&frame(120, 40, &app), 20, 25), lit, "let go");
}

/// A right-click at `at` on a `width`x`height` frame of `app`.
fn right_click(app: &mut App, size: (u16, u16), at: (u16, u16)) {
    mouse_on(app, size, MouseEventKind::Down(MouseButton::Right), at);
    mouse_on(app, size, MouseEventKind::Up(MouseButton::Right), at);
}

/// The first cell of `pane` on a 120x40 frame of `app` that nothing more
/// particular was drawn over: a border, or a pane with nothing in it.
fn in_pane(app: &App, pane: Focus) -> (u16, u16) {
    let hits = hits(app);
    (0..40)
        .flat_map(|y| (0..120).map(move |x| (x, y)))
        .find(|(x, y)| {
            hits.at(Position::new(*x, *y))
                .is_some_and(|(_, target)| target == Target::Pane(pane))
        })
        .unwrap_or_else(|| panic!("nowhere in {pane:?} is bare"))
}

/// `pane`'s menu, opened with a right-click where nothing in it moves.
fn open_in(app: &mut App, pane: Focus) {
    let at = in_pane(app, pane);
    right_click(app, WIDE, at);
}

/// Where each entry of the open menu is on a 120x40 frame of `app`.
fn entries(app: &App) -> Vec<(u16, u16)> {
    hits(app)
        .regions()
        .filter(|(_, target)| matches!(target, Target::MenuItem(_)))
        .map(|(rect, _)| (rect.x, rect.y))
        .collect()
}

#[test]
fn picking_each_entry_is_exactly_its_key() {
    let states: [State; 6] = [
        ("idle", idle),
        ("running", running),
        ("truncated", truncated),
        ("filtered", filtered),
        ("source view", source_view),
        ("roomy", roomy),
    ];
    let mut checked = 0;
    let mut wrong = Vec::new();
    for (state, make) in states {
        for pane in [Focus::Objects, Focus::Scratch, Focus::Results] {
            let mut opened = make();
            open_in(&mut opened, pane);
            let mut closed = opened.clone();
            closed.shell.mouse.menu = None;
            let places = entries(&opened);
            assert_eq!(places.len(), menu(pane).len(), "{pane:?} in {state}");
            for (item, ((name, does), (x, y))) in menu(pane).into_iter().zip(places).enumerate() {
                let mut clicked = opened.clone();
                let by_mouse = click(&mut clicked, x, y);
                let mut picked = opened.clone();
                for _ in 0..item {
                    picked.handle(Event::Key(key("j")));
                }
                let by_enter = picked.handle(Event::Key(key("Enter")));
                let mut pressed = closed.clone();
                let by_key = pressed.handle(Event::Key(key(name)));
                let (clicked, picked, pressed) =
                    (settled(clicked), settled(picked), settled(pressed));
                let what = format!("{does} in {pane:?} in {state}");
                if clicked != pressed
                    || picked != pressed
                    || by_mouse != by_key
                    || by_enter != by_key
                {
                    wrong.push(format!("{what}: not its key"));
                } else if state == "roomy"
                    && by_key.is_empty()
                    && pressed == settled(closed.clone())
                {
                    // Elsewhere an entry may have nothing to act on, the way
                    // its key may: the footer says why, as it does for the key.
                    wrong.push(format!("{what}: does nothing"));
                }
                checked += 1;
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(checked, 6 * (7 + 5 + 8));
}

#[test]
fn a_right_click_selects_what_a_left_click_would_then_opens_the_menu_there() {
    // A cell of the grid, and a row of the tree off its glyph.
    for (x, y) in [(60, 25), (12, 5)] {
        let mut left = deep();
        click(&mut left, x, y);
        let mut right = deep();
        right_click(&mut right, WIDE, (x, y));
        assert_ne!(right.tabs, deep().tabs, "it moved something");
        assert_eq!(right.tabs, left.tabs);
        assert_eq!(right.shell.focus, left.shell.focus);
        assert_eq!(
            right.shell.mouse.menu,
            Some(Menu {
                pane: left.shell.focus,
                at: Position::new(x, y),
                item: 0
            })
        );
    }
    // A pane's border opens its menu and moves nothing.
    let mut app = deep();
    open_in(&mut app, Focus::Objects);
    assert_eq!(app.tabs, deep().tabs);
    assert_eq!(app.shell.focus, Focus::Objects);
    assert_eq!(
        app.shell.mouse.menu.map(|open| open.pane),
        Some(Focus::Objects)
    );
    // A tab is not a pane: it is a left click there, and no menu.
    let mut app = two_tabs();
    right_click(&mut app, WIDE, (20, 0));
    assert_eq!(app.shell.active_tab, 1);
    assert_eq!(app.shell.mouse.menu, None);
}

#[test]
fn a_right_click_in_the_pads_selection_keeps_it_and_beside_it_moves_the_cursor() {
    let mut app = roomy();
    app.shell.focus = Focus::Scratch;
    let (rect, gutter) = hits(&app)
        .regions()
        .find_map(|(rect, target)| match target {
            Target::Pad { gutter, .. } => Some((rect, gutter)),
            _ => None,
        })
        .expect("the pad");
    let selection = app.tabs[0].scratch.selection().expect("a selection");
    let ((line, column), _) = selection;
    let at = |column: usize| {
        (
            rect.x + u16::try_from(gutter + column).expect("on screen"),
            rect.y + u16::try_from(line).expect("on screen"),
        )
    };
    right_click(&mut app, WIDE, at(column));
    assert_eq!(app.tabs[0].scratch.selection(), Some(selection));
    let text = app.tabs[0].scratch.selected_text().expect("selected");
    let (x, y) = entries(&app)[2];
    assert_eq!(click(&mut app, x, y), vec![Action::Copy(text)], "Ctrl-C");

    right_click(&mut app, WIDE, at(column + 3));
    assert_eq!(app.tabs[0].scratch.selection(), None);
    assert_eq!(app.tabs[0].scratch.cursor(), (line, column + 3));
    assert_eq!(
        app.shell.mouse.menu.map(|open| open.pane),
        Some(Focus::Scratch)
    );
}

/// The menu for Results, as it is drawn at the right end of a frame.
const RESULTS_MENU: [&str; 10] = [
    "╭ Results ──────────────────────╮",
    "│ inspect the cell        Enter │",
    "│ copy the cell               y │",
    "│ copy the row                Y │",
    "│ sort by the column          o │",
    "│ export the result set       e │",
    "│ 10,000 more rows            m │",
    "│ previous result set         [ │",
    "│ next result set             ] │",
    "╰───────────────────────────────╯",
];

#[test]
fn a_menu_opened_near_the_bottom_right_corner_stays_on_the_screen() {
    // Two rows up from the footer and one in from the edge, it turns left
    // and up and ends at the pointer.
    let mut app = idle();
    right_click(&mut app, WIDE, (118, 37));
    let terminal = frame(120, 40, &app);
    let expected: Vec<String> = RESULTS_MENU
        .iter()
        .map(|row| format!("│{:34}││{:49}{row}│", "", ""))
        .collect();
    assert_eq!(lines(&terminal, 28..38), expected);
    assert_eq!(line(&terminal, 27), right(""));

    // At the smallest size there is not the room below it either, so it
    // goes up from the pointer too, and over the panes' borders.
    let mut app = idle();
    right_click(&mut app, SMALL, (58, 12));
    let terminal = frame(60, 15, &app);
    assert_eq!(
        lines(&terminal, 3..13),
        [
            "│   ▾ Tables       ││     ╭ Results ──────────────────────╮│",
            "│     ▸ customers  ││     │ inspect the cell        Enter ││",
            "│     ▸ orders     │╰─────│ copy the cell               y │╯",
            "│   ▸ Views        │╭ Resu│ copy the row                Y │╮",
            "│   ▸ Procedures   ││ noth│ sort by the column          o ││",
            "│   ▸ Functions    ││     │ export the result set       e ││",
            "│   ▸ Sequences    ││     │ 10,000 more rows            m ││",
            "│ ▸ bench          ││     │ previous result set         [ ││",
            "│                  ││     │ next result set             ] ││",
            "│                  ││     ╰───────────────────────────────╯│",
        ]
    );
}

#[test]
fn the_entry_enter_would_pick_is_painted_like_the_cursor_and_the_pointer_lights_its_own() {
    let theme = Theme::new(false);
    let mut app = idle();
    right_click(&mut app, WIDE, (118, 37));
    app.handle(Event::Key(key("j")));
    app.shell.mouse.pointer = Some(Position::new(100, 32));
    let terminal = frame(120, 40, &app);
    let buffer = terminal.backend().buffer();
    // The second entry is highlighted and the fourth is under the pointer.
    for (y, reversed) in [(29, false), (30, true), (31, false)] {
        assert_eq!(
            (88..118).all(|x| buffer[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)),
            reversed,
            "row {y}"
        );
    }
    assert!(
        (88..118)
            .all(|x| Style::new().fg(buffer[(x, 32)].fg).bg(buffer[(x, 32)].bg) == theme.hover)
    );
}

#[test]
fn the_menu_takes_every_key_while_it_is_open() {
    let mut app = running();
    open_in(&mut app, Focus::Results);
    let item = |app: &App| app.shell.mouse.menu.map(|open| open.item);
    for (spec, at) in [("k", 0), ("Down", 1), ("j", 2), ("Up", 1)] {
        assert!(app.handle(Event::Key(key(spec))).is_empty());
        assert_eq!(item(&app), Some(at), "after {spec}");
    }
    for _ in 0..20 {
        app.handle(Event::Key(key("j")));
    }
    assert_eq!(item(&app), Some(7), "the last entry is as far as it goes");
    assert!(
        app.handle(Event::Key(key("Esc"))).is_empty(),
        "Esc closed the menu before it cancelled the query"
    );
    assert_eq!(item(&app), None);

    for spec in ["q", "?", "x"] {
        let mut app = idle();
        open_in(&mut app, Focus::Scratch);
        let before = app.tabs.clone();
        assert!(app.handle(Event::Key(key(spec))).is_empty(), "{spec}");
        assert_eq!(item(&app), None, "{spec} closed it");
        assert_eq!(app.tabs, before, "{spec} was not typed");
        assert!(!app.shell.help);
    }
}

#[test]
fn a_click_beside_the_menu_closes_it_and_reaches_nothing() {
    let mut app = two_tabs();
    open_in(&mut app, Focus::Results);
    assert_eq!(
        hits(&app)
            .at(Position::new(20, 0))
            .map(|(_, target)| target),
        Some(Target::Outside)
    );
    click(&mut app, 20, 0);
    assert_eq!(app.shell.mouse.menu, None);
    assert_eq!(app.shell.active_tab, 0, "the tab under it was not clicked");
    assert_eq!(app.shell.focus, Focus::Results);
}
