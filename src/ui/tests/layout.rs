//! The three panes, the tab bar above them and the footer below.

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
            " Tab next pane  Shift-Tab previous pane  Ctrl-T next tab  1-9 select tab  ? help",
            "○ disconnected",
        )
    );

    // The scratch pad types its own characters, so its hints are the keys
    // that are not one — and there is room for all of them.
    app.handle(Event::Key(key("Tab")));
    let terminal = frame(120, 40, &app);
    assert_eq!(
        line(&terminal, 39),
        footer_of(
            &terminal,
            " Tab next pane  Shift-Tab previous pane  Ctrl-T next tab  ? help  Esc close help or error  Ctrl-Q quit",
            "○ disconnected",
        )
    );

    app.tabs[0].state = TabState::Connected;
    let terminal = frame(120, 40, &app);
    assert!(line(&terminal, 39).ends_with("● connected"));
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
    for focused in 0..3 {
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
        app.handle(Event::Key(key("Tab")));
    }
}
