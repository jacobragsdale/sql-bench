//! The keys, and the state they change.

use super::*;
use crate::config::{Config, Connection};

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
    app
}

#[test]
fn every_key_the_help_lists_is_handled_where_it_says_it_works() {
    for (spec, place, does) in KEYS {
        for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
            let elsewhere = match *place {
                NOT_SCRATCH => focus == Focus::Scratch,
                SCRATCH => focus != Focus::Scratch,
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
