//! The grid: what a result set looks like on a frame, and what one frame of
//! it costs whatever the scan returned.

use std::time::{Duration, Instant};

use super::*;
use crate::app::Focus;
use crate::app::results::Results;
use crate::app::tests::filled;
use crate::db::model::{Cell, Column, DbError, QueryEvent};

/// The two-tab app with this result set on the tab showing, focused so the
/// cell cursor is the one the grid paints.
fn showing(results: Results) -> App {
    let mut app = two_tabs();
    app.shell.focus = Focus::Results;
    app.tabs[0].results = results;
    app
}

/// The inside of the results pane: the lowest pane corner, plus the border
/// and the padding.
fn pane(terminal: &Terminal<TestBackend>) -> (u16, u16) {
    let (x, y) = corners(terminal)
        .into_iter()
        .max_by_key(|(_, y)| *y)
        .expect("the results pane");
    (x + 2, y + 1)
}

/// One row of the pane's inside, as text: from the padding to the border on
/// the far side of it.
fn body(terminal: &Terminal<TestBackend>, row: u16) -> String {
    let (x, y) = pane(terminal);
    let buffer = terminal.backend().buffer();
    (x..buffer.area.width - 2)
        .map(|column| buffer[(column, y + row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// One result set of one column and one row, which is what the inspector
/// tests open on.
fn one_cell(name: &str, type_name: &str, cell: Cell) -> Results {
    let mut results = Results::default();
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(vec![Column {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
    }]));
    results.apply(QueryEvent::Rows(vec![vec![cell]]));
    results
}

/// Where the inspector's overlay sits on a 120-column frame: 72 wide and
/// centred, which is nowhere near a pane's own border.
const INSPECT_X: u16 = 24;
const INSPECT_WIDE: u16 = 72;

/// The rows of the overlay, border and all, with the padding trimmed off the
/// right of each.
fn overlay(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let row = |y: u16| {
        (INSPECT_X..INSPECT_X + INSPECT_WIDE)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let corner = |glyph: &str| {
        (0..buffer.area.height)
            .find(|y| buffer[(INSPECT_X, *y)].symbol() == glyph)
            .unwrap_or_else(|| panic!("no {glyph} where the overlay is"))
    };
    (corner("╭")..=corner("╰")).map(row).collect()
}

/// A theme token as [`painted`] reports it: a cell always has a foreground,
/// even when the style that painted it named none.
fn as_painted(style: Style) -> Style {
    style.fg(ratatui::style::Color::Reset)
}

#[test]
fn the_grid_is_a_header_a_type_row_and_the_columns_that_fit() {
    let app = showing(filled(50, 20));
    let terminal = frame(120, 40, &app);
    let screen = text(&terminal);
    assert!(
        screen.contains("╭ Results · 50 rows · 42 ms "),
        "the title says what the run cost:\n{screen}"
    );
    // Four of the twenty columns fit across the pane: the header is as wide
    // as its own type name, and a long value stops at forty characters.
    assert_eq!(
        body(&terminal, 0),
        format!(
            "{:<8}  {:<40}  {:<11}  {:<8}",
            "column_0", "column_1", "column_2", "column_3"
        )
        .trim_end()
    );
    assert_eq!(
        body(&terminal, 1),
        format!(
            "{:<8}  {:<40}  {:<11}  {:<8}",
            "int", "varchar(40)", "varchar(40)", "int"
        )
        .trim_end()
    );
    let first = body(&terminal, 2);
    assert!(
        first.starts_with("       0  row 0 of a value far too long for one c…"),
        "numbers to the right, a long value cut at forty: {first:?}"
    );
    assert!(first.contains("NULL"), "{first:?}");
    assert_eq!(
        body(&terminal, 20),
        "      18  row 18 of a value far too long for one …  NULL         c3r18",
        "the last row that fits, and nothing below it"
    );
}

#[test]
fn the_cell_cursor_is_reversed_and_a_null_is_dim() {
    let mut app = showing(filled(50, 20));
    let theme = Theme::new(false);
    let terminal = frame(120, 40, &app);
    let (x, y) = pane(&terminal);
    assert_eq!(
        painted(&terminal, x, y + 2),
        as_painted(theme.cursor),
        "the first cell"
    );
    // Column 2 is the NULL one: eight and forty characters of column, and
    // the two spaces between each.
    assert_eq!(painted(&terminal, x + 52, y + 2), theme.dim);

    // The cursor moves with the keys, and the column it lands in with it.
    app.handle(Event::Key(key("j")));
    app.handle(Event::Key(key("l")));
    let terminal = frame(120, 40, &app);
    assert_eq!(painted(&terminal, x, y + 2), as_painted(Style::default()));
    assert_eq!(painted(&terminal, x + 10, y + 3), as_painted(theme.cursor));
}

#[test]
fn a_short_pane_gives_the_rows_the_row_the_types_would_have_had() {
    let app = showing(filled(50, 4));
    // At fifteen rows the results pane is eight high, which is the line the
    // types are dropped at.
    let terminal = frame(120, 15, &app);
    assert!(body(&terminal, 0).starts_with("column_0"));
    assert!(
        body(&terminal, 1).starts_with("       0"),
        "the first row, not the types: {:?}",
        body(&terminal, 1)
    );
}

#[test]
fn a_failure_is_the_servers_own_message_with_the_line_it_names() {
    let mut app = showing(Results::default());
    app.tabs[0].results.start(Instant::now(), 0, 1, false);
    app.tabs[0].results.apply(QueryEvent::Error(DbError::Query {
        message: "Invalid object name 'bench.nope'.".to_owned(),
        line: Some(2),
    }));
    let terminal = frame(120, 40, &app);
    assert_eq!(
        body(&terminal, 0),
        "line 2: Invalid object name 'bench.nope'."
    );
    let (x, y) = pane(&terminal);
    assert_eq!(painted(&terminal, x, y), Theme::new(false).error);
}

#[test]
fn a_run_of_several_statements_ends_with_the_summary_line() {
    let mut results = Results::default();
    for statement in 0..2 {
        results.start(Instant::now(), statement, 2, false);
        results.apply(QueryEvent::Columns(vec![Column {
            name: "n".to_owned(),
            type_name: "int".to_owned(),
        }]));
        results.apply(QueryEvent::Rows(vec![vec![Cell::Int(1)]]));
        results.apply(QueryEvent::Done {
            rows: 1,
            truncated: false,
            connect_ms: 0,
            first_row_ms: 1,
            total_ms: 4,
        });
    }
    let terminal = frame(120, 40, &showing(results));
    let screen = text(&terminal);
    assert!(
        screen.contains("2 statements, 2 result sets, 0 rows affected"),
        "{screen}"
    );
}

/// One draw of a hundred thousand rows, the second one so that nothing is
/// being allocated for the first time.
fn one_draw() -> Duration {
    let app = showing(filled(100_000, 8));
    let theme = Theme::new(false);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("a test terminal");
    terminal
        .draw(|frame| render(frame, &app, &theme))
        .expect("a frame");
    let at = Instant::now();
    terminal
        .draw(|frame| render(frame, &app, &theme))
        .expect("a frame");
    at.elapsed()
}

#[test]
fn a_hundred_thousand_rows_cost_one_window_to_draw() {
    let drew = one_draw();
    assert!(
        drew < Duration::from_millis(20),
        "a debug draw of 100,000 rows took {drew:?}, which is the whole \
         key-to-frame budget; the release budget is checked by \
         `cargo test --release -- --ignored`"
    );
}

#[test]
#[ignore = "release timing: cargo test --release -- --ignored"]
fn a_hundred_thousand_rows_draw_inside_the_five_millisecond_budget() {
    let drew = one_draw();
    assert!(
        drew < Duration::from_millis(5),
        "a release draw of 100,000 rows took {drew:?}"
    );
}

#[test]
fn the_inspector_wraps_a_long_value_and_says_how_long_it_is() {
    // 100 repeats of ten characters: the row the replay script opens, in
    // miniature, and a length with a comma in it.
    let body = "Lorem ipsu".repeat(100);
    let mut app = showing(one_cell("body", "nvarchar(max)", Cell::Text(body.clone())));
    app.handle(Event::Key(key("Enter")));
    let lines = overlay(&frame(120, 40, &app));
    assert_eq!(
        lines[0],
        format!("╭ body · nvarchar(max) · 1,000 chars {}╮", "─".repeat(34))
    );
    assert_eq!(
        lines[1],
        format!("│ {} │", body.chars().take(68).collect::<String>())
    );
    assert_eq!(
        lines[2],
        format!("│ {} │", body.chars().skip(68).take(68).collect::<String>())
    );
    // Fifteen lines of value, plus the two of border.
    assert_eq!(lines.len(), 1_000_usize.div_ceil(68) + 2);
    assert_eq!(lines[15], format!("│ {:<68} │", &body[68 * 14..]));
}

#[test]
fn a_value_taller_than_the_screen_scrolls_inside_the_tab_bar_and_the_footer() {
    // Five hundred lines that say which one they are, so a scroll is not
    // just the same glyph one row up.
    let body: Vec<String> = (0..500).map(|line| format!("line {line}")).collect();
    let app = &mut showing(one_cell(
        "body",
        "nvarchar(max)",
        Cell::Text(body.join("\n")),
    ));
    app.handle(Event::Key(key("Enter")));
    let terminal = frame(120, 40, app);
    let lines = overlay(&terminal);
    assert_eq!(
        lines.len(),
        38,
        "the tab bar and the footer are not covered"
    );
    assert_eq!(line(&terminal, 0), " 1 local-mssql ○  2 local-oracle ○");
    assert_eq!(lines[1], format!("│ {:<68} │", "line 0"));

    app.handle(Event::Key(key("j")));
    let lines = overlay(&frame(120, 40, app));
    assert_eq!(lines[1], format!("│ {:<68} │", "line 1"));

    // Past the end is as far as it goes, and what shows is the last page.
    for _ in 0..200 {
        app.handle(Event::Key(key("PageDown")));
    }
    let lines = overlay(&frame(120, 40, app));
    assert_eq!(lines[1], format!("│ {:<68} │", "line 464"));
    assert_eq!(lines[36], format!("│ {:<68} │", "line 499"));
    assert_eq!(lines[37], format!("╰{}╯", "─".repeat(70)));
}

#[test]
fn the_inspector_draws_bytes_as_a_hex_dump_with_the_printable_ones_beside_it() {
    let mut data: Vec<u8> = b"Lorem ipsum dolor sit".to_vec();
    data.push(0);
    let mut app = showing(one_cell("data", "varbinary(max)", Cell::Bytes(data)));
    app.handle(Event::Key(key("Enter")));
    let lines = overlay(&frame(120, 40, &app));
    assert!(
        lines[0].starts_with("╭ data · varbinary(max) · 22 bytes "),
        "{:?}",
        lines[0]
    );
    assert_eq!(
        lines[1],
        format!(
            "│ {:<68} │",
            "000000  4c6f7265 6d206970 73756d20 646f6c6f  Lorem ipsum dolo"
        )
    );
    assert_eq!(
        lines[2],
        format!(
            "│ {:<68} │",
            "000010  72207369 7400                        r sit."
        ),
        "a short last line pads, and a byte no font has is a dot"
    );
}

#[test]
fn the_inspector_says_null_for_a_null_and_nothing_for_an_empty_string() {
    let mut app = showing(one_cell("body", "nvarchar(max)", Cell::Null));
    app.handle(Event::Key(key("Enter")));
    let lines = overlay(&frame(120, 40, &app));
    assert_eq!(
        lines[0],
        format!("╭ body · nvarchar(max) · NULL {}╮", "─".repeat(41))
    );
    assert_eq!(lines[1], format!("│ {:<68} │", "NULL"));
    assert_eq!(lines.len(), 3);

    let mut app = showing(one_cell("body", "nvarchar(max)", Cell::Text(String::new())));
    app.handle(Event::Key(key("Enter")));
    let lines = overlay(&frame(120, 40, &app));
    assert!(lines[0].starts_with("╭ body · nvarchar(max) · 0 chars "));
    assert_eq!(lines.len(), 3, "one empty line, not none and not two");
}

#[test]
fn a_value_with_line_breaks_keeps_them() {
    let mut app = showing(one_cell(
        "body",
        "nvarchar(max)",
        Cell::Text("one\r\ntwo\n\nfour".to_owned()),
    ));
    app.handle(Event::Key(key("Enter")));
    let lines = overlay(&frame(120, 40, &app));
    assert_eq!(
        lines[1..5],
        ["one", "two", "", "four"].map(|text| format!("│ {text:<68} │"))
    );
}

#[test]
fn the_export_prompt_is_the_footer_with_a_cursor_on_it() {
    let mut app = showing(filled(3, 2));
    app.handle(Event::Key(key("e")));
    let terminal = frame(120, 40, &app);
    let footer = line(&terminal, 39);
    let path = app.shell.prompt.as_ref().expect("the prompt").text.clone();
    assert!(
        footer.starts_with(&format!(" Export to: {path} ")),
        "{footer:?}"
    );
    assert!(footer.ends_with("○ disconnected"), "{footer:?}");
    // The cursor is the cell past the end of the path, where the next
    // character goes.
    let at = u16::try_from(" Export to: ".len() + path.chars().count()).expect("a column");
    assert_eq!(
        painted(&terminal, at, 39),
        as_painted(Theme::new(false).cursor)
    );
}

/// T5.4: a name drawn two terminal columns per character used to be padded
/// as though it were one, so every column after it leaned by a column per
/// wide glyph. ratatui blanks the cell a wide glyph covers, so one character
/// of a frame is one terminal column and the offsets below are the columns.
#[test]
fn a_wide_glyph_takes_two_columns_and_the_column_after_it_still_lines_up() {
    let mut results = Results::default();
    results.start(Instant::now(), 0, 1, false);
    results.apply(QueryEvent::Columns(
        [("name", "nvarchar"), ("country", "char")]
            .map(|(name, type_name)| Column {
                name: name.to_owned(),
                type_name: type_name.to_owned(),
            })
            .to_vec(),
    ));
    results.apply(QueryEvent::Rows(
        [("Zoë Bauer", "DE"), ("李雷", "CN"), ("山田太郎", "JP")]
            .map(|(name, country)| {
                vec![Cell::Text(name.to_owned()), Cell::Text(country.to_owned())]
            })
            .to_vec(),
    ));
    let terminal = frame(120, 40, &showing(results));
    let rows: Vec<String> = (0..5).map(|row| body(&terminal, row)).collect();
    assert_eq!(
        rows,
        [
            "name       country",
            "nvarchar   char",
            "Zoë Bauer  DE",
            "李 雷        CN",
            "山 田 太 郎    JP",
        ]
    );
    for (row, country) in rows.iter().zip(["country", "", "DE", "CN", "JP"]) {
        if country.is_empty() {
            continue;
        }
        let column = row.find(country).map(|byte| row[..byte].chars().count());
        assert_eq!(
            column,
            Some(11),
            "the second column starts at the same terminal column on every row: {row:?}"
        );
    }
}
