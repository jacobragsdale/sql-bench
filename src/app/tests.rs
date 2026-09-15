//! The keys, and the state they change.

use super::*;
use crate::config::{Config, Connection};
use crate::db::catalog::{CatalogAnswer, CatalogRequest, DbObject, ObjectKind};
use crate::db::model::{Cell, Column, QueryEvent};
use objects::Objects;
use results::Results;

/// The two connections `config.local.toml` names, which is what the dev loop
/// and every test has.
pub(crate) fn two_connections() -> Config {
    let connection = |name: &str, kind: Kind| Connection {
        name: name.to_owned(),
        kind,
        host: "localhost".to_owned(),
        port: kind.default_port(),
        database: (kind == Kind::Mssql).then(|| "bench".to_owned()),
        service: (kind == Kind::Oracle).then(|| "FREEPDB1".to_owned()),
        user: "bench".to_owned(),
        password: None,
        trust_cert: true,
        encrypt: true,
    };
    Config {
        connections: vec![
            connection("local-mssql", Kind::Mssql),
            connection("local-oracle", Kind::Oracle),
        ],
        ..Config::default()
    }
}

/// A result set of `rows` × `columns`: the first column counts, the second
/// is text long enough to be cut, the third is NULL and the rest are short
/// text — which is every way the grid draws a cell.
pub(crate) fn filled(rows: usize, columns: usize) -> Results {
    let mut results = Results::default();
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(
        (0..columns)
            .map(|column| Column {
                name: format!("column_{column}"),
                type_name: if column % 3 == 0 {
                    "int".to_owned()
                } else {
                    "varchar(40)".to_owned()
                },
            })
            .collect(),
    ));
    results.apply(QueryEvent::Rows(
        (0..rows)
            .map(|row| {
                (0..columns)
                    .map(|column| match column {
                        0 => Cell::Int(row as i64),
                        1 => {
                            Cell::Text(format!("row {row} of a value far too long for one column"))
                        }
                        2 => Cell::Null,
                        _ => Cell::Text(format!("c{column}r{row}")),
                    })
                    .collect()
            })
            .collect(),
    ));
    results.apply(QueryEvent::Done {
        rows,
        truncated: false,
        connect_ms: 1,
        first_row_ms: 2,
        total_ms: 42,
    });
    results
}

/// One object of a listing, as the catalog reports it.
pub(crate) fn object(schema: &str, name: &str, kind: ObjectKind) -> DbObject {
    DbObject {
        schema: schema.to_owned(),
        name: name.to_owned(),
        kind,
        modified: Some("2025-01-01 00:00:00".to_owned()),
    }
}

/// A tree with a schema open, its tables loaded and the cursor on one of
/// them — which is where every key of the objects pane has something to do.
pub(crate) fn browsed(backend: Kind) -> Objects {
    let mut objects = Objects::new(backend, "bench");
    let schemas = CatalogAnswer::Schemas(vec!["dbo".to_owned(), "bench".to_owned()]);
    objects.answer(&CatalogRequest::Schemas, &Ok(schemas));
    // The own schema is open already, so this is its Tables branch.
    objects.key(key("j"));
    objects.key(key("l"));
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::Table,
    };
    let listing = CatalogAnswer::Objects(vec![
        object("dbo", "customers", ObjectKind::Table),
        object("dbo", "orders", ObjectKind::Table),
    ]);
    objects.answer(&request, &Ok(listing));
    objects.key(key("j"));
    objects
}

pub(crate) fn two_tabs() -> App {
    App::new(&two_connections())
}

/// A key the way [`KEYS`] spells it, as crossterm sends it.
pub(crate) fn key(spec: &str) -> KeyEvent {
    // The one spelling that is a range rather than a chord; any of its nine
    // digits would do, and this one is a tab both fixtures have.
    if spec == "1-9" {
        return KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);
    }
    let mut parts: Vec<&str> = spec.rsplitn(2, '-').collect();
    parts.reverse();
    let last = parts.pop().expect("a key to press");
    let mut modifiers = KeyModifiers::NONE;
    for part in parts {
        modifiers |= match part {
            "Ctrl" => KeyModifiers::CONTROL,
            "Shift" => KeyModifiers::SHIFT,
            other => panic!("{spec}: unknown modifier {other:?}"),
        };
    }
    let code = match last {
        "Tab" if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        "Esc" => KeyCode::Esc,
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Home" => KeyCode::Home,
        "Space" => KeyCode::Char(' '),
        "End" => KeyCode::End,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        // The one row that names four keys; the left one stands for them.
        "Arrows" => KeyCode::Left,
        "F5" => KeyCode::F(5),
        other => {
            let mut characters = other.chars();
            let character = characters.next().expect("a key to press");
            assert!(characters.next().is_none(), "{spec}: not one key");
            KeyCode::Char(character)
        }
    };
    KeyEvent::new(code, modifiers)
}

fn press(app: &mut App, spec: &str) -> Vec<Action> {
    app.handle(Event::Key(key(spec)))
}

/// An app where every key in [`KEYS`] has something left to do: the second
/// tab showing so a tab key can move, an error to close, and — for the
/// scratch pad's keys — a statement to run, a word to delete, an edit to
/// take back and a selection to copy.
fn ready(focus: Focus) -> App {
    let mut app = two_tabs();
    app.shell.focus = focus;
    app.shell.active_tab = 1;
    app.shell.error = Some("boom".to_owned());
    let scratch = &mut app.tabs[1].scratch;
    scratch.set_text("select count(*) from bench.events;");
    for _ in 0..12 {
        scratch.handle(key("Right"));
    }
    // An edit, so Ctrl-Z has a burst to take back, and a selection over it
    // so Ctrl-C has something to copy.
    scratch.handle(key("x"));
    scratch.handle(key("Shift-Left"));
    app.tabs[1].objects = browsed(app.tabs[1].kind);
    // A grid with somewhere to move in every direction, a second set for
    // `[` and `]`, and a cap that stopped it so `m` has more to fetch.
    let results = &mut app.tabs[1].results;
    *results = filled(50, 4);
    results.apply(QueryEvent::Columns(vec![Column {
        name: "second".to_owned(),
        type_name: "int".to_owned(),
    }]));
    results.apply(QueryEvent::Rows(vec![vec![Cell::Int(1)]]));
    results.apply(QueryEvent::Done {
        rows: 1,
        truncated: true,
        connect_ms: 1,
        first_row_ms: 2,
        total_ms: 3,
    });
    // Back to the first set, which is the one with rows to move about in.
    results.key(key("["));
    for _ in 0..20 {
        results.key(key("j"));
    }
    results.key(key("l"));
    app
}

#[test]
fn every_key_the_help_lists_is_handled_where_it_says_it_works() {
    for (spec, place, does) in KEYS {
        for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
            let elsewhere = match *place {
                NOT_SCRATCH => focus == Focus::Scratch,
                SCRATCH => focus != Focus::Scratch,
                RESULTS => focus != Focus::Results,
                OBJECTS => focus != Focus::Objects,
                _ => false,
            };
            if elsewhere {
                continue;
            }
            let mut app = ready(focus);
            let before = app.clone();
            let actions = press(&mut app, spec);
            assert!(
                !actions.is_empty() || app != before,
                "{spec} ({place}: {does}) does nothing with {focus:?} focused"
            );
        }
    }
}

#[test]
fn tab_and_shift_tab_cycle_the_focus_both_ways() {
    let mut app = two_tabs();
    for expected in [Focus::Results, Focus::Scratch, Focus::Objects] {
        press(&mut app, "Shift-Tab");
        assert_eq!(app.shell.focus, expected);
    }
    press(&mut app, "Tab");
    assert_eq!(app.shell.focus, Focus::Scratch);

    // Tab is the pad's own key once it has the focus: it types two spaces,
    // and Shift-Tab is the way out.
    press(&mut app, "Tab");
    assert_eq!(app.shell.focus, Focus::Scratch);
    assert_eq!(app.tabs[0].scratch.text(), "  ");
    press(&mut app, "Shift-Tab");
    assert_eq!(app.shell.focus, Focus::Objects);
}

#[test]
fn ctrl_t_wraps_round_the_tabs_and_a_digit_picks_one() {
    let mut app = two_tabs();
    for expected in [1, 0, 1] {
        press(&mut app, "Ctrl-T");
        assert_eq!(app.shell.active_tab, expected);
    }
    press(&mut app, "1-9");
    assert_eq!(app.shell.active_tab, 0);
    press(&mut app, "9");
    assert_eq!(app.shell.active_tab, 0, "there is no ninth tab to go to");
}

#[test]
fn q_quits_unless_the_scratch_pad_has_it_and_ctrl_q_always_does() {
    let mut app = two_tabs();
    assert_eq!(press(&mut app, "q"), vec![Action::Quit]);
    app.shell.focus = Focus::Scratch;
    assert_eq!(press(&mut app, "q"), vec![], "q is a character to type");
    assert_eq!(press(&mut app, "Ctrl-Q"), vec![Action::Quit]);
}

#[test]
fn a_digit_is_a_character_to_type_in_the_scratch_pad() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    press(&mut app, "1-9");
    assert_eq!(app.shell.active_tab, 0);
}

#[test]
fn esc_closes_the_help_first_and_the_error_after() {
    let mut app = two_tabs();
    app.shell.error = Some("could not connect".to_owned());
    press(&mut app, "?");
    assert!(app.shell.help);
    press(&mut app, "Esc");
    assert!(!app.shell.help);
    assert_eq!(app.shell.error.as_deref(), Some("could not connect"));
    press(&mut app, "Esc");
    assert_eq!(app.shell.error, None);
}

#[test]
fn the_open_help_takes_the_scroll_keys_the_pane_would_have_had() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    press(&mut app, "?");
    for spec in ["j", "Down", "PageDown"] {
        press(&mut app, spec);
    }
    assert_eq!(app.shell.help_scroll, 2 + HELP_PAGE);
    assert_eq!(
        app.tabs[0].scratch.text(),
        "",
        "the pad typed the help's keys"
    );
    for spec in ["k", "Up", "PageUp"] {
        press(&mut app, spec);
    }
    assert_eq!(app.shell.help_scroll, 0);

    press(&mut app, "PageDown");
    press(&mut app, "?");
    assert!(!app.shell.help);
    assert_eq!(app.shell.help_scroll, 0, "the next ? opens at the top");
    // With it closed the pad has them back.
    press(&mut app, "j");
    assert_eq!(app.tabs[0].scratch.text(), "j");
}

#[test]
fn a_tab_is_a_connection_from_the_config_and_starts_disconnected() {
    let app = two_tabs();
    assert_eq!(
        app.tabs
            .iter()
            .map(|tab| (tab.name.as_str(), tab.kind, tab.state.clone()))
            .collect::<Vec<_>>(),
        [
            ("local-mssql", Kind::Mssql, TabState::Disconnected),
            ("local-oracle", Kind::Oracle, TabState::Disconnected),
        ]
    );
    assert_eq!(app.tab().map(|tab| tab.name.as_str()), Some("local-mssql"));
}

#[test]
fn no_connections_is_an_app_with_no_tabs_that_still_takes_keys() {
    let mut app = App::new(&Config::default());
    assert!(app.tabs.is_empty());
    assert_eq!(app.tab(), None);
    press(&mut app, "1-9");
    press(&mut app, "Ctrl-T");
    assert_eq!(app.shell.active_tab, 0);
    assert_eq!(press(&mut app, "q"), vec![Action::Quit]);
}

#[test]
fn c_asks_to_connect_the_tab_on_screen_and_shift_c_to_disconnect_it() {
    let mut app = two_tabs();
    assert_eq!(press(&mut app, "c"), vec![Action::Connect(0)]);
    assert_eq!(press(&mut app, "C"), vec![Action::Disconnect(0)]);
    press(&mut app, "Ctrl-T");
    assert_eq!(press(&mut app, "c"), vec![Action::Connect(1)]);

    // A connection is not something the scratch pad asks for; both are
    // characters to type there.
    app.shell.focus = Focus::Scratch;
    assert_eq!(press(&mut app, "c"), vec![]);
    assert_eq!(press(&mut app, "C"), vec![]);
}

#[test]
fn a_runtime_event_is_the_only_thing_that_moves_a_tab_and_it_says_so_in_the_footer() {
    let mut app = two_tabs();
    app.apply(RuntimeEvent::Connecting { tab: 0 });
    assert_eq!(app.tabs[0].state, TabState::Connecting);
    assert!(app.busy(), "a connecting tab is a busy app");

    app.apply(RuntimeEvent::Connected {
        tab: 0,
        connect_ms: 42,
    });
    assert_eq!(app.tabs[0].state, TabState::Connected);
    assert_eq!(app.tabs[0].connect_ms, Some(42));
    assert_eq!(app.shell.status, "● local-mssql connected in 42 ms");
    assert!(!app.busy());

    app.apply(RuntimeEvent::Failed {
        tab: 1,
        message: "localhost:1521: ORA-12541".to_owned(),
    });
    assert_eq!(
        app.tabs[1].state,
        TabState::Failed("localhost:1521: ORA-12541".to_owned())
    );
    assert_eq!(app.shell.status, "✗ local-oracle failed");
    assert_eq!(app.tabs[0].state, TabState::Connected, "one tab each");

    app.apply(RuntimeEvent::Disconnected { tab: 0 });
    assert_eq!(app.tabs[0].state, TabState::Disconnected);
    assert_eq!(app.tabs[0].connect_ms, None);
    assert_eq!(app.shell.status, "○ local-mssql disconnected");

    // A tab that is not there is a stale message, not a panic.
    app.apply(RuntimeEvent::Connected {
        tab: 9,
        connect_ms: 1,
    });
}

#[test]
fn the_spinner_moves_once_a_tick_while_something_is_connecting_and_never_otherwise() {
    let mut shell = Shell::default();
    let started = Instant::now();
    assert!(!shell.tick(started, false), "nothing is connecting");
    assert_eq!(shell.spinner, 0);

    assert!(shell.tick(started, true), "the first frame is due at once");
    assert_eq!(shell.spinner, 1);
    assert!(
        !shell.tick(started + SPIN_EVERY / 2, true),
        "half a tick is no frame"
    );
    assert!(shell.tick(started + SPIN_EVERY, true));
    assert_eq!(shell.spinner, 2);
    assert_eq!(shell.mark(&TabState::Connecting), SPINNER[2]);
    assert_eq!(shell.mark(&TabState::Connected), "●");

    // Off again, and the next connection starts its own first frame.
    assert!(!shell.tick(started + SPIN_EVERY, false));
    assert!(shell.tick(started + SPIN_EVERY, true));
}

#[test]
fn a_resize_is_remembered_and_asks_for_nothing() {
    let mut app = two_tabs();
    assert_eq!(app.handle(Event::Resize(120, 40)), vec![]);
    assert_eq!(app.shell.size, Size::new(120, 40));
}

#[test]
fn the_keys_of_a_pane_are_its_own_and_the_ones_that_work_anywhere() {
    let scratch: Vec<&str> = keys_for(Focus::Scratch).map(|(key, ..)| *key).collect();
    assert_eq!(
        scratch,
        [
            "Shift-Tab",
            "Ctrl-T",
            "Ctrl-R",
            "F5",
            "Ctrl-E",
            "Ctrl-Z",
            "Ctrl-C",
            "Shift-Arrows",
            "Tab",
            "Home",
            "End",
            "Ctrl-A",
            "Ctrl-U",
            "Ctrl-K",
            "Ctrl-W",
            "Ctrl-Left",
            "Ctrl-Right",
            "?",
            "Esc",
            "Ctrl-Q",
        ],
        "the pad's own keys, and the ones that are not characters"
    );
    let objects: Vec<&str> = keys_for(Focus::Objects).map(|(key, ..)| *key).collect();
    assert!(
        !objects.contains(&"Ctrl-R"),
        "no pane is offered another pane's keys: {objects:?}"
    );
}

#[test]
fn the_grids_keys_move_the_cell_cursor_and_ask_for_what_the_app_cannot_do() {
    let mut app = ready(Focus::Results);
    let selected = app.tabs[1].results.selected();
    press(&mut app, "G");
    assert_eq!(app.tabs[1].results.selected().0, 49, "the last row");
    press(&mut app, "g");
    press(&mut app, "0");
    assert_eq!(app.tabs[1].results.selected(), (0, 0));
    press(&mut app, "$");
    assert_eq!(app.tabs[1].results.selected().1, 3, "the last column");
    assert_ne!(app.tabs[1].results.selected(), selected);

    // The keys that are not the grid's to answer.
    assert_eq!(press(&mut app, "m"), vec![Action::MoreRows { tab: 1 }]);
    assert_eq!(press(&mut app, "Enter"), vec![]);
    assert!(app.shell.inspector.is_some(), "Enter opens the inspector");
    press(&mut app, "Esc");
    assert!(app.shell.inspector.is_none());

    // A tab is still a tab and a digit is still a tab, in the grid too.
    assert_eq!(press(&mut app, "c"), vec![Action::Connect(1)]);
    press(&mut app, "1-9");
    assert_eq!(app.shell.active_tab, 0);
}

#[test]
fn esc_stops_a_running_query_before_it_closes_an_error() {
    let mut app = two_tabs();
    app.shell.error = Some("boom".to_owned());
    app.apply(RuntimeEvent::QueryStarted {
        tab: 0,
        at: Instant::now(),
        statement: 0,
        of: 1,
        keep_view: false,
    });
    assert!(app.busy(), "a running query is a busy app");
    assert_eq!(press(&mut app, "Esc"), vec![Action::Cancel(0)]);
    assert_eq!(
        app.shell.error.as_deref(),
        Some("boom"),
        "the error waits its turn"
    );

    app.apply(RuntimeEvent::Query {
        tab: 0,
        event: QueryEvent::Error(crate::db::model::DbError::Cancelled),
    });
    assert!(!app.busy());
    assert_eq!(press(&mut app, "Esc"), vec![]);
    assert_eq!(app.shell.error, None);
}

#[test]
fn a_statement_the_server_said_no_to_is_flagged_in_the_pad_until_the_next_edit() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    app.tabs[0]
        .scratch
        .set_text("select 1;\n\nselect * from nope;");
    press(&mut app, "Down");
    press(&mut app, "Down");
    assert_eq!(
        press(&mut app, "Ctrl-R"),
        vec![Action::RunStatement {
            tab: 0,
            sql: "select * from nope;".to_owned()
        }]
    );

    app.apply(RuntimeEvent::QueryStarted {
        tab: 0,
        at: Instant::now(),
        statement: 0,
        of: 1,
        keep_view: false,
    });
    assert_eq!(app.shell.status, "running…");
    app.apply(RuntimeEvent::Query {
        tab: 0,
        event: QueryEvent::Error(crate::db::model::DbError::Query {
            message: "Invalid object name 'nope'.".to_owned(),
            line: Some(1),
        }),
    });
    assert_eq!(
        app.tabs[0].scratch.flagged(),
        Some(&(2..3)),
        "the lines the statement was on"
    );

    press(&mut app, "x");
    assert_eq!(app.tabs[0].scratch.flagged(), None, "an edit clears it");
}

#[test]
fn f5_runs_every_statement_and_a_failure_says_which_one_it_was() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    app.tabs[0]
        .scratch
        .set_text("select 1;\n\nselect 2;\n\nselect 3;");
    assert_eq!(
        press(&mut app, "F5"),
        vec![Action::RunAll {
            tab: 0,
            statements: vec![
                "select 1;".to_owned(),
                "select 2;".to_owned(),
                "select 3;".to_owned()
            ]
        }]
    );

    app.apply(RuntimeEvent::QueryStarted {
        tab: 0,
        at: Instant::now(),
        statement: 1,
        of: 3,
        keep_view: false,
    });
    assert_eq!(app.shell.status, "running statement 2 of 3");
    app.apply(RuntimeEvent::Query {
        tab: 0,
        event: QueryEvent::Error(crate::db::model::DbError::Query {
            message: "no".to_owned(),
            line: None,
        }),
    });
    assert_eq!(app.shell.status, "statement 2 of 3 failed");
    assert_eq!(app.tabs[0].scratch.flagged(), Some(&(2..3)));
}

#[test]
fn y_copies_the_cell_and_shift_y_the_row_with_a_null_as_nothing() {
    let mut app = ready(Focus::Results);
    // The cursor is on row 20, column 1: the long text one.
    assert_eq!(app.tabs[1].results.selected(), (20, 1));
    let cell = "row 20 of a value far too long for one column";
    assert_eq!(
        press(&mut app, "y"),
        vec![Action::Copy(cell.to_owned())],
        "the whole value, not the forty characters the grid drew"
    );
    assert_eq!(app.shell.clipboard, cell);
    assert_eq!(app.shell.status, "copied 1 cell");

    assert_eq!(
        press(&mut app, "Y"),
        vec![Action::Copy(format!("20\t{cell}\t\tc3r20"))],
        "tab-separated, and the NULL column is empty"
    );
    assert_eq!(app.shell.status, "copied 1 row");

    // A result set with no rows has no cell to copy and nothing to say.
    app.tabs[1].results = Results::default();
    assert_eq!(press(&mut app, "y"), vec![]);
    assert_eq!(press(&mut app, "Y"), vec![]);
    assert_eq!(press(&mut app, "Enter"), vec![]);
    assert!(app.shell.inspector.is_none(), "nothing to inspect");
}

#[test]
fn the_open_inspector_takes_the_scroll_keys_and_esc_closes_it_before_an_error() {
    let mut app = ready(Focus::Results);
    let row = app.tabs[1].results.selected().0;
    press(&mut app, "Enter");
    assert!(app.shell.inspector.is_some());

    for spec in ["j", "Down", "PageDown"] {
        press(&mut app, spec);
    }
    // The cell is 44 characters, so the whole value is one line and the
    // scroll is clamped to it.
    assert_eq!(app.shell.inspector.map(|open| open.scroll), Some(0));
    assert_eq!(
        app.tabs[1].results.selected().0,
        row,
        "the grid never saw the scroll keys"
    );

    // Esc closes the inspector first; the error waits its turn.
    press(&mut app, "Esc");
    assert!(app.shell.inspector.is_none());
    assert_eq!(app.shell.error.as_deref(), Some("boom"));
    press(&mut app, "Esc");
    assert_eq!(app.shell.error, None);
}

#[test]
fn the_inspector_scrolls_a_value_longer_than_the_overlay() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Results;
    let mut results = Results::default();
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(vec![Column {
        name: "body".to_owned(),
        type_name: "nvarchar(max)".to_owned(),
    }]));
    results.apply(QueryEvent::Rows(vec![vec![Cell::Text("x".repeat(1_000))]]));
    app.tabs[0].results = results;

    press(&mut app, "Enter");
    assert_eq!(app.inspect_lines().len(), 1_000_usize.div_ceil(68));
    for _ in 0..40 {
        press(&mut app, "j");
    }
    assert_eq!(
        app.shell.inspector.map(|open| open.scroll),
        Some(14),
        "as far as the last line and no further"
    );
    press(&mut app, "PageUp");
    assert_eq!(app.shell.inspector.map(|open| open.scroll), Some(4));
}

#[test]
fn e_opens_the_export_prompt_which_then_takes_every_key() {
    let mut app = ready(Focus::Results);
    assert_eq!(press(&mut app, "e"), vec![]);
    let prompt = app.shell.prompt.clone().expect("the prompt");
    assert!(
        prompt.text.starts_with("~/sql-bench-local-oracle-"),
        "prefilled with the tab's connection: {}",
        prompt.text
    );
    assert!(prompt.text.ends_with(".csv"));

    // Every key is one the prompt is being typed with, `?` and q included.
    press(&mut app, "Ctrl-U");
    for spec in [
        "?", "q", "/", "t", "m", "p", "/", "a", ".", "j", "s", "o", "n",
    ] {
        press(&mut app, spec);
    }
    assert!(!app.shell.help, "? was a character");
    assert!(!app.shell.should_quit, "q was a character");
    assert_eq!(
        press(&mut app, "Enter"),
        vec![Action::Export {
            tab: 1,
            path: "?q/tmp/a.json".to_owned()
        }]
    );
    assert!(app.shell.prompt.is_none(), "Enter closes it");

    // Esc is the way out, and an empty path asks for nothing.
    press(&mut app, "e");
    press(&mut app, "Esc");
    assert!(app.shell.prompt.is_none());
    press(&mut app, "e");
    press(&mut app, "Ctrl-U");
    assert_eq!(press(&mut app, "Enter"), vec![]);
    assert!(app.shell.prompt.is_none());
}

/// The rows the tree would draw, as `<indent><name>` — which is the flat
/// list, its depths and what is expanded, all in one assertion.
fn rows(objects: &Objects) -> Vec<String> {
    objects
        .visible()
        .into_iter()
        .map(|index| {
            let node = &objects.nodes()[index];
            format!("{:width$}{}", "", node.item.name(), width = node.depth * 2)
        })
        .collect()
}

/// Put the cursor on the row with this name, from the top.
fn go_to(objects: &mut Objects, name: &str) {
    objects.key(key("g"));
    for _ in 0..objects.nodes().len() {
        if objects.nodes()[objects.cursor()].item.name() == name {
            return;
        }
        objects.key(key("j"));
    }
    panic!("no {name} in {:?}", rows(objects));
}

/// An app connected to its second tab with a tree on it, which is what every
/// objects test starts from.
fn browsing() -> App {
    let mut app = two_tabs();
    app.shell.active_tab = 1;
    app.tabs[1].objects = browsed(Kind::Oracle);
    app
}

#[test]
fn the_schema_list_puts_the_connections_own_schema_first_and_opens_it() {
    let mut objects = Objects::new(Kind::Oracle, "bench");
    objects.answer(
        &CatalogRequest::Schemas,
        &Ok(CatalogAnswer::Schemas(vec![
            "APP".to_owned(),
            "BENCH".to_owned(),
            "PDBADMIN".to_owned(),
        ])),
    );
    assert_eq!(
        rows(&objects),
        [
            "BENCH",
            "  Tables",
            "  Views",
            "  Procedures",
            "  Functions",
            "  Packages",
            "  Sequences",
            "APP",
            "PDBADMIN",
        ],
        "the schema it connected to is the one it opens, and Oracle is the \
         one backend with packages"
    );

    let mut mssql = Objects::new(Kind::Mssql, "sa");
    mssql.answer(
        &CatalogRequest::Schemas,
        &Ok(CatalogAnswer::Schemas(vec![
            "bench".to_owned(),
            "dbo".to_owned(),
        ])),
    );
    assert_eq!(
        rows(&mssql),
        [
            "dbo",
            "  Tables",
            "  Views",
            "  Procedures",
            "  Functions",
            "  Sequences",
            "bench",
        ],
        "a login lands in dbo, and no SQL Server has a package"
    );
}

#[test]
fn a_branch_loads_once_and_opens_and_closes_without_asking_again() {
    let mut objects = Objects::new(Kind::Mssql, "sa");
    objects.answer(
        &CatalogRequest::Schemas,
        &Ok(CatalogAnswer::Schemas(vec!["dbo".to_owned()])),
    );
    objects.key(key("j"));
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::Table,
    };
    assert_eq!(objects.key(key("l")), objects::Hit::Load(request.clone()));
    objects.started(&request);
    assert!(objects.busy(), "the answer has not come back yet");
    assert!(
        objects.nodes()[1].loading,
        "and the row says so until it does"
    );

    objects.answer(
        &request,
        &Ok(CatalogAnswer::Objects(vec![object(
            "dbo",
            "customers",
            ObjectKind::Table,
        )])),
    );
    assert!(!objects.busy());
    assert!(!objects.nodes()[1].loading);
    let open = [
        "dbo",
        "  Tables",
        "    customers",
        "  Views",
        "  Procedures",
        "  Functions",
        "  Sequences",
    ];
    let closed = [
        "dbo",
        "  Tables",
        "  Views",
        "  Procedures",
        "  Functions",
        "  Sequences",
    ];
    assert_eq!(rows(&objects), open);

    // Closed and opened again, it asks nothing: the answer is still here.
    assert_eq!(objects.key(key("Space")), objects::Hit::Moved);
    assert_eq!(rows(&objects), closed);
    assert_eq!(objects.key(key("Space")), objects::Hit::Moved);
    assert_eq!(rows(&objects), open);

    // `r` is how it is asked again.
    assert_eq!(objects.key(key("r")), objects::Hit::Load(request));
    assert_eq!(rows(&objects), closed, "and it empties first");
}

#[test]
fn a_load_that_failed_says_so_on_the_row_and_in_the_footer() {
    let mut app = browsing();
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::View,
    };
    app.apply(RuntimeEvent::Catalog {
        tab: 1,
        request: request.clone(),
        result: Err(crate::db::model::DbError::Query {
            message: "ORA-00942: table or view does not exist".to_owned(),
            line: None,
        }),
    });
    let objects = &app.tabs[1].objects;
    let views = objects
        .nodes()
        .iter()
        .find(|node| node.item.name() == "Views")
        .expect("the views branch");
    assert_eq!(
        views.error.as_deref(),
        Some("ORA-00942: table or view does not exist")
    );
    assert!(!views.expanded, "a branch that did not load is not open");
    assert_eq!(
        app.shell.error.as_deref(),
        Some("ORA-00942: table or view does not exist")
    );
}

#[test]
fn the_filter_keeps_what_matches_and_the_branches_above_it() {
    let mut app = browsing();
    // A second table and a view, so there is something to hide.
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::View,
    };
    go_to(&mut app.tabs[1].objects, "Views");
    app.tabs[1].objects.key(key("l"));
    app.apply(RuntimeEvent::Catalog {
        tab: 1,
        request,
        result: Ok(CatalogAnswer::Objects(vec![object(
            "dbo",
            "v_customer_totals",
            ObjectKind::View,
        )])),
    });

    press(&mut app, "/");
    assert!(app.tabs[1].objects.filtering());
    for character in ["c", "u", "s", "t"] {
        press(&mut app, character);
    }
    assert_eq!(app.tabs[1].objects.filter(), "cust");
    assert_eq!(
        rows(&app.tabs[1].objects),
        [
            "dbo",
            "  Tables",
            "    customers",
            "  Views",
            "    v_customer_totals"
        ],
        "orders is not a match and no branch above one"
    );

    // The pane keeps the letters: `c` is not a connect key while a filter is
    // being typed.
    assert_eq!(app.tabs[1].state, TabState::Disconnected);
    press(&mut app, "Esc");
    assert_eq!(app.tabs[1].objects.filter(), "");
    assert!(!app.tabs[1].objects.filtering());
    assert!(rows(&app.tabs[1].objects).contains(&"    orders".to_owned()));
}

#[test]
fn enter_on_a_table_puts_a_select_in_the_pad_and_moves_the_focus_to_it() {
    let mut app = browsing();
    app.shell.focus = Focus::Objects;
    assert_eq!(press(&mut app, "Enter"), vec![]);
    assert_eq!(
        app.tabs[1].scratch.text(),
        "select * from dbo.customers fetch first 100 rows only\n"
    );
    assert_eq!(app.shell.focus, Focus::Scratch);

    // SQL Server spells the same hundred rows its own way, and a line
    // somebody is writing is not written over.
    let mut app = two_tabs();
    app.tabs[0].objects = browsed(Kind::Mssql);
    app.tabs[0].scratch.set_text("select 1");
    press(&mut app, "Enter");
    assert_eq!(
        app.tabs[0].scratch.text(),
        "select top 100 * from dbo.customers\nselect 1",
        "the line the cursor is on is pushed down rather than written over"
    );
}

#[test]
fn i_asks_for_the_columns_and_s_for_the_source_of_what_the_cursor_is_on() {
    let mut app = browsing();
    assert_eq!(
        press(&mut app, "i"),
        vec![Action::LoadObjects {
            tab: 1,
            request: CatalogRequest::Columns {
                schema: "dbo".to_owned(),
                table: "customers".to_owned(),
                show: true,
            }
        }]
    );
    assert_eq!(
        press(&mut app, "s"),
        vec![],
        "a table is its columns, and it says so rather than asking"
    );
    assert_eq!(
        app.shell.status,
        "a table has no source text; i shows its columns"
    );

    // The columns come back into the tree and the grid at once.
    app.apply(RuntimeEvent::Catalog {
        tab: 1,
        request: CatalogRequest::Columns {
            schema: "dbo".to_owned(),
            table: "customers".to_owned(),
            show: true,
        },
        result: Ok(CatalogAnswer::Columns(vec![
            crate::db::catalog::ColumnInfo {
                name: "id".to_owned(),
                type_text: "int".to_owned(),
                nullable: false,
                is_pk: true,
            },
        ])),
    });
    assert_eq!(app.tabs[1].results.rows().len(), 1);
    assert_eq!(
        app.tabs[1].results.title(),
        "dbo.customers columns · 1 rows"
    );
    press(&mut app, "l");
    assert!(rows(&app.tabs[1].objects).contains(&"      id".to_owned()));
}

#[test]
fn s_on_a_procedure_shows_its_source_in_the_results_pane() {
    let mut app = browsing();
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::Procedure,
    };
    // Down to the procedures branch, and open it.
    go_to(&mut app.tabs[1].objects, "Procedures");
    app.tabs[1].objects.key(key("l"));
    app.apply(RuntimeEvent::Catalog {
        tab: 1,
        request,
        result: Ok(CatalogAnswer::Objects(vec![object(
            "dbo",
            "sp_customer_orders",
            ObjectKind::Procedure,
        )])),
    });
    go_to(&mut app.tabs[1].objects, "sp_customer_orders");
    let source = CatalogRequest::Source {
        schema: "dbo".to_owned(),
        name: "sp_customer_orders".to_owned(),
        kind: ObjectKind::Procedure,
    };
    assert_eq!(
        press(&mut app, "s"),
        vec![Action::LoadObjects {
            tab: 1,
            request: source.clone()
        }]
    );
    app.catalog_started(1, &source);
    assert!(app.busy(), "a catalog query in flight is the app working");

    app.apply(RuntimeEvent::Catalog {
        tab: 1,
        request: source,
        result: Ok(CatalogAnswer::Source(
            "CREATE PROCEDURE sp_customer_orders AS\nBEGIN\nEND;".to_owned(),
        )),
    });
    assert!(!app.busy());
    assert_eq!(
        app.tabs[1].results.title(),
        "Source · dbo.sp_customer_orders · 3 lines"
    );
    // The results pane's own keys scroll it, and nothing else does.
    app.shell.focus = Focus::Results;
    press(&mut app, "G");
    assert_eq!(app.tabs[1].results.source().expect("the source").scroll, 2);
    press(&mut app, "g");
    assert_eq!(app.tabs[1].results.source().expect("the source").scroll, 0);
}

#[test]
fn y_copies_the_qualified_name_of_whatever_the_cursor_is_on() {
    let mut app = browsing();
    assert_eq!(
        press(&mut app, "y"),
        vec![Action::Copy("dbo.customers".to_owned())]
    );
    assert_eq!(app.shell.clipboard, "dbo.customers");
    go_to(&mut app.tabs[1].objects, "dbo");
    assert_eq!(press(&mut app, "y"), vec![Action::Copy("dbo".to_owned())]);
}

#[test]
fn h_closes_a_branch_and_then_goes_up_one_and_l_opens_and_steps_in() {
    let mut app = browsing();
    let objects = &mut app.tabs[1].objects;
    assert_eq!(objects.nodes()[objects.cursor()].item.name(), "customers");
    objects.key(key("h"));
    assert_eq!(objects.nodes()[objects.cursor()].item.name(), "Tables");
    objects.key(key("h"));
    assert_eq!(
        rows(objects),
        [
            "dbo",
            "  Tables",
            "  Views",
            "  Procedures",
            "  Functions",
            "  Packages",
            "  Sequences",
            "bench"
        ]
    );
    objects.key(key("h"));
    assert_eq!(objects.nodes()[objects.cursor()].item.name(), "dbo");
    objects.key(key("l"));
    objects.key(key("l"));
    objects.key(key("l"));
    assert_eq!(
        objects.nodes()[objects.cursor()].item.name(),
        "customers",
        "the branches were loaded already, so opening one steps into it"
    );
}

#[test]
fn a_connection_that_comes_or_goes_empties_the_tree() {
    let mut app = browsing();
    assert!(!app.tabs[1].objects.is_empty());
    app.apply(RuntimeEvent::Disconnected { tab: 1 });
    assert!(app.tabs[1].objects.is_empty());
    assert!(!app.busy(), "and nothing is still waited on");
}
