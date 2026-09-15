//! The screens that take the layout's place: help, too small, no config.

use super::*;
use crate::app::KEYS;

#[test]
fn the_help_lists_every_key_with_where_it_works_and_what_it_does() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("?")));
    let terminal = frame(120, 40, &app);
    let screen = text(&terminal);
    assert!(screen.contains("╭ Help "), "{screen}");
    for (spec, place, does) in KEYS {
        let row = format!(" {spec:<10}{place:<12}{does}");
        assert!(screen.contains(&row), "no {row:?} in\n{screen}");
    }

    app.handle(Event::Key(key("Esc")));
    assert!(!text(&frame(120, 40, &app)).contains("╭ Help "));
}

#[test]
fn a_terminal_under_the_minimum_gets_one_message_instead_of_the_layout() {
    let app = two_tabs();
    let terminal = frame(40, 10, &app);
    let screen = text(&terminal);
    assert_eq!(
        screen
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>(),
        ["          sql-bench needs 60x15"]
    );
    // One column or one row short is still short.
    for (width, height) in [(59, 40), (120, 14)] {
        assert!(
            text(&frame(width, height, &app)).contains("needs 60x15"),
            "{width}x{height} drew the layout"
        );
    }
    assert!(text(&frame(60, 15, &app)).contains("╭ Objects "));
}

#[test]
fn no_connections_is_one_pane_saying_where_the_config_goes() {
    let app = App::new(&crate::config::Config::default());
    let screen = text(&frame(60, 15, &app));
    assert!(screen.contains("╭ sql-bench "), "{screen}");
    for expected in [
        "No connections yet.",
        "~/.config/sql-bench/config.toml",
        "$SQL_BENCH_CONFIG",
        "[[connection]]",
        "kind = \"mssql\"",
        "q quits.",
    ] {
        assert!(screen.contains(expected), "no {expected:?} in\n{screen}");
    }
    assert!(!screen.contains("Objects"), "no layout to show:\n{screen}");
}

#[test]
fn the_footer_hints_and_the_help_are_the_same_table() {
    let mut app = two_tabs();
    app.shell.help = true;
    // Wide enough that the footer shows the whole list and not as much of it
    // as fits, so a key missing from it is a key missing from it.
    let help = text(&frame(200, 60, &app));
    app.shell.help = false;
    for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
        app.shell.focus = focus;
        let footer = line(&frame(200, 60, &app), 59);
        for (spec, place, does) in KEYS {
            assert!(
                help.contains(&format!(" {spec:<10}{place:<12}{does}")),
                "the help does not list {spec} ({place}: {does}):\n{help}"
            );
            let works_here =
                keys_for(focus).any(|(other, _, other_does)| other == spec && other_does == does);
            // The footer hints as many keys as fit, so a key it shows must
            // work here; one it leaves out may simply not have fit.
            if footer.contains(&format!(" {spec} {does}")) {
                assert!(
                    works_here,
                    "{spec} ({place}: {does}) hinted with {focus:?} focused:\n{footer}"
                );
            }
        }
    }
}
