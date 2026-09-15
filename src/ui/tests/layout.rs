//! The three panes, the tab bar above them and the footer below.

use std::time::Instant;

use super::*;

/// Where the footer's right end sits: one column off the edge.
fn footer_of(terminal: &Terminal<TestBackend>, hints: &str, state: &str) -> String {
    let width = usize::from(terminal.backend().buffer().area.width);
    let gap = width - hints.chars().count() - state.chars().count() - 1;
    format!("{hints}{:gap$}{state}", "")
}

#[test]
fn the_tab_bar_names_every_connection_and_marks_where_it_is() {
    let mut app = two_tabs();
    let terminal = frame(120, 40, &app);
    assert_eq!(line(&terminal, 0), " 1 local-mssql ○  2 local-oracle ○");

    app.tabs[1].state = TabState::Connected;
    app.tabs[0].state = TabState::Failed("login failed".to_owned());
    assert_eq!(
        line(&frame(120, 40, &app), 0),
        " 1 local-mssql ✗  2 local-oracle ●"
    );
}

#[test]
fn the_tab_showing_is_the_accented_one() {
    let mut app = two_tabs();
    let theme = Theme::new(false);
    let terminal = frame(120, 40, &app);
    // The digit each tab is selected by, which is where its label starts.
    assert_eq!(painted(&terminal, 1, 0), theme.accent);
    assert_eq!(painted(&terminal, 18, 0), theme.dim);

    app.handle(Event::Key(key("Ctrl-T")));
    let terminal = frame(120, 40, &app);
    assert_eq!(painted(&terminal, 1, 0), theme.dim);
    assert_eq!(painted(&terminal, 18, 0), theme.accent);
}

#[test]
fn the_three_panes_are_titled_placeholders() {
    let terminal = frame(120, 40, &two_tabs());
    let screen = text(&terminal);
    for (title, placeholder) in [
        ("╭ Objects ", "press c to connect"),
        ("╭ Scratch ", "your SQL goes here"),
        ("╭ Results ", "nothing has run yet"),
    ] {
        assert!(screen.contains(title), "no {title} pane in\n{screen}");
        assert!(
            screen.contains(placeholder),
            "no {placeholder} in\n{screen}"
        );
    }
}

#[test]
fn the_footer_hints_the_focused_panes_keys_and_says_where_the_connection_is() {
    let mut app = two_tabs();
    let terminal = frame(120, 40, &app);
    assert_eq!(
        line(&terminal, 39),
        footer_of(
            &terminal,
            " Tab next pane  Shift-Tab previous pane  Ctrl-T next tab  1-9 select tab  c connect  C disconnect",
            "○ disconnected",
        )
    );

    // The scratch pad types its own characters, so its hints are the keys
    // that are not one, its own chords first.
    app.handle(Event::Key(key("Tab")));
    let terminal = frame(120, 40, &app);
    assert_eq!(
        line(&terminal, 39),
        footer_of(
            &terminal,
            " Shift-Tab previous pane  Ctrl-T next tab  Ctrl-R run the statement  F5 run all  Ctrl-E edit in $EDITOR",
            "○ disconnected",
        )
    );

    app.tabs[0].state = TabState::Connected;
    let terminal = frame(120, 40, &app);
    assert!(line(&terminal, 39).ends_with("● connected"));

    // How long it took, once the connection reported it.
    app.tabs[0].connect_ms = Some(4);
    assert!(line(&frame(120, 40, &app), 39).ends_with("● connected 4ms"));

    app.tabs[0].state = TabState::Connecting;
    app.tabs[0].connect_ms = None;
    assert!(line(&frame(120, 40, &app), 39).ends_with("⠋ connecting"));

    app.tabs[0].state = TabState::Failed("no".to_owned());
    assert!(line(&frame(120, 40, &app), 39).ends_with("✗ failed"));
}

#[test]
fn a_connecting_tab_is_marked_with_the_spinner_frame_the_shell_is_on() {
    let mut app = two_tabs();
    app.tabs[0].state = TabState::Connecting;
    let started = Instant::now();
    for (step, expected) in ["⠋", "⠙", "⠹", "⠸", "⠋"].into_iter().enumerate() {
        assert_eq!(
            line(&frame(120, 40, &app), 0),
            format!(" 1 local-mssql {expected}  2 local-oracle ○")
        );
        #[allow(clippy::cast_possible_truncation)]
        let now = started + crate::app::SPIN_EVERY * step as u32;
        assert!(app.shell.tick(now, true), "a frame is due");
    }
}

#[test]
fn a_failed_connection_is_the_message_and_how_to_retry_in_the_results_pane() {
    let mut app = two_tabs();
    app.tabs[0].state = TabState::Failed("localhost:1433: cannot connect: refused".to_owned());
    let terminal = frame(120, 40, &app);
    let screen = text(&terminal);
    assert!(
        screen.contains("localhost:1433: cannot connect: refused"),
        "{screen}"
    );
    assert!(screen.contains("c to retry"), "{screen}");
    assert!(!screen.contains("nothing has run yet"), "{screen}");

    // The message is in the error colour, at the top left of the pane.
    let (x, y) = corners(&terminal)
        .into_iter()
        .max_by_key(|(_, y)| *y)
        .expect("the results pane");
    assert_eq!(painted(&terminal, x + 2, y + 1), Theme::new(false).error);
}

#[test]
fn an_error_takes_the_footer_over_until_it_is_closed() {
    let mut app = two_tabs();
    app.shell.error = Some("could not connect to local-mssql".to_owned());
    let terminal = frame(120, 40, &app);
    let theme = Theme::new(false);
    assert!(
        line(&terminal, 39).starts_with(" could not connect to local-mssql"),
        "{}",
        line(&terminal, 39)
    );
    assert_eq!(painted(&terminal, 1, 39), theme.error);

    app.handle(Event::Key(key("Esc")));
    assert!(line(&frame(120, 40, &app), 39).starts_with(" Tab next pane"));
}

#[test]
fn the_focused_pane_is_the_one_with_the_accent_border() {
    let theme = Theme::new(false);
    let mut app = two_tabs();
    for (focused, focus) in [Focus::Objects, Focus::Scratch, Focus::Results]
        .into_iter()
        .enumerate()
    {
        app.shell.focus = focus;
        let terminal = frame(120, 40, &app);
        let corners = corners(&terminal);
        assert_eq!(corners.len(), 3, "three panes have three corners");
        for (pane, (x, y)) in corners.into_iter().enumerate() {
            let expected = if pane == focused {
                theme.accent
            } else {
                theme.border
            };
            assert_eq!(
                painted(&terminal, x, y),
                expected,
                "pane {pane} at ({x}, {y}) with {focused} focused"
            );
        }
    }
}

#[test]
fn a_long_message_is_cut_rather_than_pushing_the_connection_off_the_footer() {
    let mut app = two_tabs();
    app.shell.error =
        Some("could not connect to local-mssql: login failed for user 'sa' after 10 s".to_owned());
    for width in [60, 80, 120] {
        let terminal = frame(width, 15, &app);
        let footer = line(&terminal, 14);
        assert!(footer.ends_with("○ disconnected"), "{width}: {footer}");
        assert!(
            footer.starts_with(" could not connect"),
            "{width}: {footer}"
        );
        assert_eq!(
            footer.chars().count(),
            usize::from(width) - 1,
            "{width}: {footer}"
        );
    }
    assert!(
        line(&frame(60, 15, &app), 14).contains('…'),
        "a message that does not fit says so"
    );

    // A status is the same footer, and a short one is not cut at all.
    app.shell.error = None;
    app.shell.status = "1 row in 3 ms".to_owned();
    let footer = line(&frame(60, 15, &app), 14);
    assert!(footer.starts_with(" 1 row in 3 ms "), "{footer}");
    assert!(footer.ends_with("○ disconnected"), "{footer}");
}
