//! The object tree pane: indentation, glyphs, what a row that is loading or
//! that failed says, the filter in the title, and the source viewer.

use super::*;
use crate::app::objects::Objects;
use crate::app::tests::browsed;
use crate::config::Kind;
use crate::db::catalog::{CatalogAnswer, CatalogRequest, ObjectKind};
use crate::db::model::DbError;

/// The Objects pane's rows with something on them, borders and padding taken
/// off. The pane is the left 36 columns of the body.
fn tree(app: &App) -> Vec<String> {
    let terminal = frame(120, 40, app);
    (2..38)
        .map(|y| {
            line(&terminal, y)
                .chars()
                .take(35)
                .skip(2)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .take_while(|row| !row.starts_with('─'))
        .filter(|row| !row.is_empty())
        .collect()
}

fn browsing() -> App {
    let mut app = two_tabs();
    app.tabs[0].objects = browsed(Kind::Mssql);
    app
}

#[test]
fn a_branch_is_indented_two_a_level_and_says_whether_it_is_open() {
    let rows = tree(&browsing());
    assert_eq!(
        rows,
        [
            "▾ dbo",
            "  ▾ Tables",
            "    ▸ customers",
            "    ▸ orders",
            "  ▸ Views",
            "  ▸ Procedures",
            "  ▸ Functions",
            "  ▸ Sequences",
            "▸ bench",
        ]
    );
}

#[test]
fn a_row_that_is_loading_says_so_until_the_answer_comes_back() {
    let mut app = browsing();
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::View,
    };
    app.tabs[0].objects.started(&request);
    assert!(
        tree(&app).contains(&"  ▸ Views …".to_owned()),
        "{:?}",
        tree(&app)
    );

    app.tabs[0]
        .objects
        .answer(&request, &Ok(CatalogAnswer::Objects(Vec::new())));
    assert!(!tree(&app).iter().any(|row| row.contains('…')));
}

#[test]
fn a_branch_that_would_not_load_wears_the_complaint() {
    let mut app = browsing();
    let request = CatalogRequest::Objects {
        schema: "dbo".to_owned(),
        kind: ObjectKind::View,
    };
    app.tabs[0].objects.answer(
        &request,
        &Err(DbError::Query {
            message: "ORA-00942".to_owned(),
            line: None,
        }),
    );
    assert!(
        tree(&app).contains(&"  ▸ Views ✗ ORA-00942".to_owned()),
        "{:?}",
        tree(&app)
    );
    assert_eq!(
        painted(&frame(120, 40, &app), 14, 6),
        Theme::new(false).error
    );
}

#[test]
fn the_filter_is_in_the_title_and_what_it_hides_is_off_the_pane() {
    let mut app = browsing();
    app.shell.focus = Focus::Objects;
    for spec in ["/", "c", "u", "s", "t"] {
        app.handle(Event::Key(key(spec)));
    }
    let screen = text(&frame(120, 40, &app));
    assert!(screen.contains("╭ Objects /cust ─"), "{screen}");
    assert_eq!(
        tree(&app),
        ["▾ dbo", "  ▾ Tables", "    ▸ customers"],
        "what matches, and the branches above it"
    );

    // A filter nothing matches says so rather than showing an empty pane.
    for spec in ["x", "y", "z"] {
        app.handle(Event::Key(key(spec)));
    }
    assert_eq!(tree(&app), ["no objects match", "[ Clear filter ]"]);
}

#[test]
fn the_pane_says_where_to_start_until_a_connection_fills_it() {
    let app = two_tabs();
    assert_eq!(tree(&app), ["[ Connect ]"]);
}

#[test]
fn the_source_of_an_object_is_drawn_with_line_numbers_and_scrolls() {
    let mut app = two_tabs();
    let lines: Vec<String> = (1..=40).map(|number| format!("line {number}")).collect();
    app.tabs[0]
        .results
        .show_source("bench.sp_customer_orders".to_owned(), &lines.join("\n"));
    let terminal = frame(120, 40, &app);
    let screen = text(&terminal);
    assert!(
        screen.contains("╭ Source · bench.sp_customer_orders · 40 lines ─"),
        "{screen}"
    );
    assert!(screen.contains(" 1 line 1"), "{screen}");

    // The results pane's own movement keys scroll it.
    app.shell.focus = Focus::Results;
    app.handle(Event::Key(key("G")));
    let screen = text(&frame(120, 40, &app));
    assert!(screen.contains("40 line 40"), "{screen}");
    assert!(!screen.contains(" 1 line 1"), "{screen}");
}

#[test]
fn the_columns_of_a_table_are_a_grid_like_any_other_result() {
    let mut app = two_tabs();
    app.tabs[0].objects = Objects::new(Kind::Mssql, "sa");
    app.tabs[0].results.show_columns(
        "bench.customers columns".to_owned(),
        &[crate::db::catalog::ColumnInfo {
            name: "id".to_owned(),
            type_text: "int".to_owned(),
            nullable: false,
            is_pk: true,
        }],
    );
    let screen = text(&frame(120, 40, &app));
    assert!(
        screen.contains("╭ bench.customers columns · 1 rows ─"),
        "{screen}"
    );
    assert!(screen.contains("name  type  nullable  pk"), "{screen}");
    assert!(screen.contains("id    int   no        yes"), "{screen}");
}
