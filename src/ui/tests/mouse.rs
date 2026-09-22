//! What the frame says can be clicked, and what a click there does.

use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::Instant;

use super::*;
use crate::app::pointer::Mouse;
use crate::app::results::{Inspector, Results};
use crate::app::tests::browsed;
use crate::app::{Action, RuntimeEvent};
use crate::config::Kind;
use crate::db::model::{Cell, Column, QueryEvent};

/// The hits of one 120x40 frame of `app`.
fn hits(app: &App) -> Hits {
    drawn(app, 120, 40).0
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

/// A state the parity test is run in, and how to get an app into it.
type State = (&'static str, fn() -> App);

#[test]
fn every_button_drawn_is_exactly_its_key_and_does_something() {
    let states: [State; 9] = [
        ("idle", idle),
        ("running", running),
        ("truncated", truncated),
        ("multi-set", multi_set),
        ("filtered", filtered),
        ("failed", failed),
        ("source view", source_view),
        ("prompt", prompting),
        ("roomy", roomy),
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
            "╭ Objects {}╮╭ Scratch [modified] ─── ▶ Run ─ ■ Stop ─╮",
            "─".repeat(7)
        )
    );
    assert!(row_with(&running, SMALL, "╭ Running").ends_with(" 3 rows ─── ■ Cancel ─╮"));
    assert_eq!(
        row_with(&multi_set(), SMALL, "╭ Results"),
        "│   ▸ Views      │╭ Results · set 2/2 · 3 rows · 3 ms ─ ▶ ─╮"
    );
    assert_eq!(
        row_with(&filtered(), SMALL, "╭ Objects"),
        "╭ Objects /zzz ──╮╭ Scratch [modified] ─── ▶▶ All ─ ▶ Run ─╮"
    );
    assert_eq!(
        row_with(&failed(), SMALL, "╭ Objects"),
        format!(
            "╭ Objects {}╮╭ Scratch ─── ✎ Editor ─ ▶▶ All ─ ▶ Run ─╮",
            "─".repeat(7)
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
