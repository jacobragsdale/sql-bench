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
    let mut parts: Vec<&str> = spec.split('-').collect();
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

#[test]
fn every_key_the_help_lists_is_handled_where_it_says_it_works() {
    for (spec, place, does) in KEYS {
        for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
            if *place == NOT_SCRATCH && focus == Focus::Scratch {
                continue;
            }
            // Set up so that every key has something left to do: the second
            // tab showing, so a tab key can move, and an error to close.
            let mut app = two_tabs();
            app.shell.focus = focus;
            app.shell.active_tab = 1;
            app.shell.error = Some("boom".to_owned());
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
    for expected in [Focus::Scratch, Focus::Results, Focus::Objects] {
        press(&mut app, "Tab");
        assert_eq!(app.shell.focus, expected);
    }
    for expected in [Focus::Results, Focus::Scratch, Focus::Objects] {
        press(&mut app, "Shift-Tab");
        assert_eq!(app.shell.focus, expected);
    }
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
        ["Tab", "Shift-Tab", "Ctrl-T", "?", "Esc", "Ctrl-Q"],
        "the scratch pad keeps the keys that are not characters"
    );
    assert_eq!(keys_for(Focus::Objects).count(), KEYS.len());
}
