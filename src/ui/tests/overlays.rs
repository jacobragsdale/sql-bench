//! The screens that take the layout's place: help, too small, no config.

use super::*;
use crate::app::tests::object;
use crate::app::{KEYS, Tab};
use crate::db::catalog::{CatalogAnswer, CatalogRequest, DbObject, ObjectKind};

/// The key column of the help for `focus`: the widest key it lists, and two
/// spaces after it.
fn key_width(focus: Focus) -> usize {
    keys_for(focus)
        .map(|(spec, _, _)| spec.chars().count())
        .max()
        .expect("a pane has keys")
        + 2
}

#[test]
fn the_help_lists_the_keys_of_the_focused_pane_and_no_others() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("?")));
    for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
        app.shell.focus = focus;
        // Tall enough that nothing has to scroll, so a missing row is a
        // missing key and not one below the fold.
        let screen = text(&frame(200, 60, &app));
        let title = format!("╭ Help · {} ─", focus.title());
        assert!(screen.contains(&title), "no {title:?} in\n{screen}");
        let width = key_width(focus);
        for (spec, place, does) in KEYS {
            let row = format!(" {spec:<width$}{does}");
            let works_here =
                keys_for(focus).any(|(other, _, other_does)| other == spec && other_does == does);
            // The whole row up to the border, so `select` is not found in
            // `select a range`.
            let listed = screen.lines().any(|line| {
                line.find(&row)
                    .is_some_and(|at| line[at + row.len()..].trim_start().starts_with('│'))
            });
            assert_eq!(
                listed, works_here,
                "{spec} ({place}: {does}) with {focus:?} focused:\n{screen}"
            );
        }
    }

    app.handle(Event::Key(key("Esc")));
    assert!(!text(&frame(120, 60, &app)).contains("╭ Help "));
}

#[test]
fn a_help_too_long_for_the_screen_scrolls_instead_of_being_cut_off() {
    let mut app = two_tabs();
    app.shell.focus = Focus::Scratch;
    app.handle(Event::Key(key("?")));
    let width = key_width(Focus::Scratch);
    let rows: Vec<String> = keys_for(Focus::Scratch)
        .map(|(spec, _, does)| format!(" {spec:<width$}{does}"))
        .collect();
    // The smallest supported terminal: two rows of it are the border, and
    // two more are the layout the overlay sits on.
    let showing = usize::from(MIN_HEIGHT) - 4;
    assert!(showing < rows.len(), "the list fits, so nothing is proven");

    let screen = text(&frame(60, 15, &app));
    assert!(
        screen.contains(&format!(
            "╭ Help · Scratch (1-{showing} of {}) ─",
            rows.len()
        )),
        "{screen}"
    );
    assert!(screen.contains(&rows[0]), "{screen}");
    assert!(!screen.contains(rows.last().expect("keys")), "{screen}");

    // Two pages is past the end, which is as far as it goes.
    app.handle(Event::Key(key("PageDown")));
    app.handle(Event::Key(key("PageDown")));
    let screen = text(&frame(60, 15, &app));
    let top = rows.len() - showing;
    assert!(
        screen.contains(&format!(
            "╭ Help · Scratch ({}-{} of {}) ─",
            top + 1,
            rows.len(),
            rows.len()
        )),
        "{screen}"
    );
    assert!(screen.contains(rows.last().expect("keys")), "{screen}");

    // A screen the whole list fits on shows all of it, scrolled or not, and
    // says no range.
    let screen = text(&frame(200, 60, &app));
    assert!(screen.contains("╭ Help · Scratch ─"), "{screen}");
    for row in &rows {
        assert!(screen.contains(row), "no {row:?} in\n{screen}");
    }

    // Esc puts the offset back, so the next ? opens at the top.
    app.handle(Event::Key(key("Esc")));
    app.handle(Event::Key(key("?")));
    assert!(text(&frame(60, 15, &app)).contains(&rows[0]));
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
    for focus in [Focus::Objects, Focus::Scratch, Focus::Results] {
        app.shell.focus = focus;
        let width = key_width(focus);
        // Wide enough that the footer shows the whole list and not as much
        // of it as fits, so a key missing from it is a key missing from it.
        let footer = line(&frame(200, 60, &app), 59);
        app.shell.help = true;
        let help = text(&frame(200, 60, &app));
        app.shell.help = false;
        for (spec, place, does) in KEYS {
            // The footer hints as many keys as fit, so a key it shows must
            // work here; one it leaves out may simply not have fit.
            if footer.contains(&format!(" {spec} {does}")) {
                assert!(
                    help.contains(&format!(" {spec:<width$}{does}")),
                    "{spec} ({place}: {does}) is hinted with {focus:?} focused but not in:\n{help}"
                );
            }
        }
    }
}

/// Two tabs with an index each, so the finder has rows from both to draw.
fn two_indexed() -> App {
    let mut app = two_tabs();
    let index = |tab: &mut Tab, objects: Vec<DbObject>| {
        tab.objects
            .answer(&CatalogRequest::Index, &Ok(CatalogAnswer::Index(objects)));
    };
    index(
        &mut app.tabs[0],
        vec![
            object("dbo", "customers", ObjectKind::Table),
            object("bench", "sp_customer_orders", ObjectKind::Procedure),
            object("bench", "orders", ObjectKind::Table),
        ],
    );
    index(
        &mut app.tabs[1],
        vec![object("BENCH", "CUSTOMERS", ObjectKind::Table)],
    );
    app
}

#[test]
fn the_finder_lists_the_matches_with_their_kind_and_tab_and_marks_the_chosen_one() {
    let mut app = two_indexed();
    app.handle(Event::Key(key("Ctrl-P")));
    let screen = text(&frame(120, 40, &app));
    assert!(screen.contains("╭ Find · 4 objects ─"), "{screen}");
    assert!(screen.contains("type a name, or schema.name"), "{screen}");

    for character in "cust".chars() {
        app.handle(Event::Key(key(&character.to_string())));
    }
    let terminal = frame(120, 40, &app);
    let screen = text(&terminal);
    assert!(screen.contains("╭ Find · 3 of 4 ─"), "{screen}");
    assert!(screen.contains("> cust"), "{screen}");
    // The name column is as wide as the widest name showing, so the kinds
    // and the tabs line up.
    let width = "bench.sp_customer_orders".len();
    let rows = [
        format!("{:<width$}  {:<9}  local-mssql", "dbo.customers", "table"),
        format!(
            "{:<width$}  {:<9}  local-oracle",
            "BENCH.CUSTOMERS", "table"
        ),
        format!(
            "{:<width$}  {:<9}  local-mssql",
            "bench.sp_customer_orders", "procedure"
        ),
    ];
    let at = (0..40)
        .find(|y| line(&terminal, *y).contains("> cust"))
        .expect("the query line");
    for (offset, row) in rows.iter().enumerate() {
        let y = at + 1 + offset as u16;
        assert!(
            line(&terminal, y).contains(row.trim_end()),
            "row {y}: {:?}",
            line(&terminal, y)
        );
    }
    let chosen = Theme::new(false).cursor;
    let row = line(&terminal, at + 1);
    let x = row
        .find("dbo")
        .map(|byte| row[..byte].chars().count())
        .expect("the first row") as u16;
    assert_eq!(
        painted(&terminal, x, at + 1),
        chosen,
        "the top row is chosen"
    );
    assert_ne!(painted(&terminal, x, at + 2), chosen);

    app.handle(Event::Key(key("Down")));
    let terminal = frame(120, 40, &app);
    assert_eq!(painted(&terminal, x, at + 2), chosen, "and Down moves it");

    app.handle(Event::Key(key("Esc")));
    assert!(!text(&frame(120, 40, &app)).contains("╭ Find"));
}

#[test]
fn the_finder_says_when_there_is_nothing_indexed_and_when_nothing_matches() {
    let mut app = two_tabs();
    app.handle(Event::Key(key("Ctrl-P")));
    let screen = text(&frame(120, 40, &app));
    assert!(
        screen.contains("╭ Find · nothing indexed yet ─"),
        "{screen}"
    );
    assert!(
        screen.contains("nothing indexed yet: c connects a tab"),
        "{screen}"
    );

    let mut app = two_indexed();
    app.handle(Event::Key(key("Ctrl-P")));
    app.handle(Event::Key(key("z")));
    let screen = text(&frame(120, 40, &app));
    assert!(screen.contains("╭ Find · 0 of 4 ─"), "{screen}");
    assert!(screen.contains("no objects match"), "{screen}");
}
