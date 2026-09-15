//! The Oracle driver against the container `scripts/db-up.sh` seeds.
//!
//! Skipped unless `SQL_BENCH_TEST_DBS` is set, so `cargo test` on a machine
//! with no database is still green. The connection comes from the committed
//! `config.local.toml`, which is what the dev loop uses — and so does the
//! Instant Client directory, which has to be on the loader's path as well
//! (`LD_LIBRARY_PATH`, or `ldconfig`); see the README.

use std::path::Path;
use std::time::{Duration, Instant};

use sql_bench::config;
use sql_bench::db::Connection;
use sql_bench::db::model::{Cell, Column, DbError, QueryEvent, QueryOptions};

/// The committed `local-oracle`, or nothing when the databases are not wanted.
fn local() -> Option<(config::Config, config::Connection)> {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return None;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml");
    let config = config::load(&path).expect("config.local.toml is readable");
    let spec = config
        .connection("local-oracle")
        .expect("config.local.toml names local-oracle")
        .clone();
    Some((config, spec))
}

fn connect() -> Option<Connection> {
    let (config, spec) = local()?;
    Some(Connection::open(&spec, &config).expect("the container is up"))
}

/// Every test starts with this; without a database it is a no-op.
macro_rules! connection {
    () => {
        match connect() {
            Some(connection) => connection,
            None => return,
        }
    };
}

fn run(connection: &Connection, sql: &str) -> Vec<QueryEvent> {
    with(connection, sql, QueryOptions::default())
}

fn with(connection: &Connection, sql: &str, options: QueryOptions) -> Vec<QueryEvent> {
    connection.query(sql, options).iter().collect()
}

fn rows(events: &[QueryEvent]) -> Vec<Vec<Cell>> {
    events
        .iter()
        .filter_map(|event| match event {
            QueryEvent::Rows(batch) => Some(batch.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn columns(events: &[QueryEvent]) -> Vec<Vec<Column>> {
    events
        .iter()
        .filter_map(|event| match event {
            QueryEvent::Columns(columns) => Some(columns.clone()),
            _ => None,
        })
        .collect()
}

fn done(events: &[QueryEvent]) -> (usize, bool) {
    match events.last() {
        Some(QueryEvent::Done {
            rows, truncated, ..
        }) => (*rows, *truncated),
        other => panic!("the query did not finish: {other:?}"),
    }
}

fn complaint(events: &[QueryEvent]) -> String {
    match &events[0] {
        QueryEvent::Error(DbError::Query { message, .. }) => message.clone(),
        other => panic!("expected the server to complain: {other:?}"),
    }
}

#[test]
fn select_one_row() {
    let connection = connection!();
    let events = run(&connection, "select 1 as one from dual");
    assert_eq!(
        columns(&events),
        [[Column {
            name: "ONE".to_owned(),
            // A literal is an unconstrained NUMBER, which is 38 digits and so
            // never an i64.
            type_name: "NUMBER".to_owned()
        }]]
    );
    assert_eq!(rows(&events), [[Cell::Decimal("1".to_owned())]]);
    assert_eq!(done(&events), (1, false));
}

#[test]
fn every_type_the_server_has_maps_to_a_cell() {
    let connection = connection!();
    let events = run(&connection, "select * from bench.all_types order by id");
    let header = columns(&events);
    let names: Vec<&str> = header[0]
        .iter()
        .map(|column| column.type_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "NUMBER(10)",
            "NUMBER",
            "NUMBER(10,2)",
            "BINARY_FLOAT",
            "BINARY_DOUBLE",
            "CHAR(5)",
            "VARCHAR2(20)",
            "NVARCHAR2(20)",
            "DATE",
            "TIMESTAMP",
            "TIMESTAMP WITH TIME ZONE",
            "INTERVAL DAY TO SECOND",
            "RAW(8)",
            "CLOB",
            "BLOB",
        ]
    );

    let rows = rows(&events);
    assert_eq!(
        rows[0],
        [
            Cell::Int(1),
            Cell::Decimal("12345.6789".to_owned()),
            Cell::Decimal("123.45".to_owned()),
            Cell::Float(1.25),
            Cell::Float(1.2345678901234),
            Cell::Text("abcde".to_owned()),
            Cell::Text("varchar2 value".to_owned()),
            Cell::Text("nvarchar2 value".to_owned()),
            Cell::DateTime("2024-05-17 00:00:00".to_owned()),
            Cell::DateTime("2024-05-17 13:45:30.123456".to_owned()),
            Cell::DateTime("2024-05-17 13:45:30.123456 +02:00".to_owned()),
            Cell::Text("+02 03:04:05.600000".to_owned()),
            Cell::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            Cell::Text("clob value".to_owned()),
            Cell::Bytes(vec![0xaa, 0xbb, 0xcc]),
        ]
    );

    let mut all_null = vec![Cell::Null; rows[1].len()];
    all_null[0] = Cell::Int(2);
    assert_eq!(rows[1], all_null, "the row where every column is NULL");
    assert_eq!(done(&events), (2, false));
}

#[test]
fn unicode_survives_the_wire() {
    let connection = connection!();
    let events = run(
        &connection,
        "select name from bench.customers where id in (1, 2, 3) order by id",
    );
    assert_eq!(
        rows(&events),
        [
            [Cell::Text("Zoë Bauer".to_owned())],
            [Cell::Text("Ægir Nilsen".to_owned())],
            [Cell::Text("李雷".to_owned())],
        ]
    );
}

#[test]
fn a_syntax_error_is_the_servers_own_complaint() {
    let connection = connection!();
    let events = run(&connection, "select from bench.customers");
    let QueryEvent::Error(DbError::Query { message, line }) = &events[0] else {
        panic!("{events:?}");
    };
    assert!(
        message.starts_with("ORA-00936: missing expression"),
        "{message}"
    );
    assert_eq!(*line, Some(1));
    assert_eq!(events.len(), 1, "an error ends the query");

    // The connection is still good: a server complaint is not a broken session.
    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual")),
        [[Cell::Decimal("1".to_owned())]]
    );
}

#[test]
fn a_syntax_error_names_the_line_it_is_on() {
    let connection = connection!();
    let events = run(&connection, "select id,\n       nope\nfrom bench.customers");
    let QueryEvent::Error(DbError::Query { message, line }) = &events[0] else {
        panic!("{events:?}");
    };
    assert!(message.starts_with("ORA-00904"), "{message}");
    assert_eq!(*line, Some(2), "the offset the server reports, as a line");
}

#[test]
fn a_big_scan_stops_at_the_cap() {
    let connection = connection!();
    let started = Instant::now();
    let events = with(
        &connection,
        "select * from bench.events",
        QueryOptions::default(),
    );
    let elapsed = started.elapsed();
    assert_eq!(done(&events), (10_000, true));
    assert_eq!(rows(&events).len(), 10_000);
    eprintln!("10_000 of 1_000_000 rows in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");

    // Closing the cursor is all a truncated scan costs here, so the session
    // that answered it answers the next one too.
    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual")),
        [[Cell::Decimal("1".to_owned())]]
    );
}

#[test]
fn an_update_reports_what_it_changed_and_rolls_back() {
    let connection = connection!();
    // No `begin transaction`: in Oracle the first DML opens one.
    let events = run(&connection, "update bench.customers set country = 'ZZ'");
    assert_eq!(events[0], QueryEvent::RowsAffected(50));
    assert_eq!(done(&events), (0, false));

    assert_eq!(
        rows(&run(
            &connection,
            "select count(*) as changed from bench.customers where country = 'ZZ'"
        )),
        [[Cell::Decimal("50".to_owned())]],
        "the transaction sees its own update"
    );

    run(&connection, "rollback");
    assert_eq!(
        rows(&run(
            &connection,
            "select count(*) as changed from bench.customers where country = 'ZZ'"
        )),
        [[Cell::Decimal("0".to_owned())]],
        "and the rollback puts the seed data back"
    );
}

#[test]
fn one_statement_is_all_a_submission_may_hold() {
    // SQL Server takes a batch of statements and answers with a result set
    // each; Oracle takes one statement and says so. The scratch pad sends one
    // at a time either way.
    let connection = connection!();
    let events = run(
        &connection,
        "select 1 as first from dual; select 2 as second from dual",
    );
    assert!(
        complaint(&events).starts_with("ORA-03405"),
        "{}",
        complaint(&events)
    );
}

#[test]
fn a_plsql_block_runs_and_changes_nothing_the_statement_can_count() {
    let connection = connection!();
    let events = run(&connection, "begin null; end;");
    assert_eq!(events[0], QueryEvent::RowsAffected(0));
    assert_eq!(done(&events), (0, false));

    // A block that does work is still a block: what it changed is its own
    // business, and this statement affected no rows of its own.
    let events = run(
        &connection,
        "begin update bench.orders set status = 'SHIPPED' where id = 1; rollback; end;",
    );
    assert_eq!(events[0], QueryEvent::RowsAffected(0));
}

#[test]
fn a_trailing_semicolon_comes_off_a_statement_and_stays_on_a_block() {
    let connection = connection!();
    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual;")),
        [[Cell::Decimal("1".to_owned())]],
        "the terminator every editor writes is not sent"
    );
    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual;\n\n")),
        [[Cell::Decimal("1".to_owned())]],
        "nor is the whitespace after it"
    );

    assert_eq!(
        run(&connection, "begin null; end;")[0],
        QueryEvent::RowsAffected(0),
        "a block keeps its terminator"
    );
    let without = run(&connection, "begin null; end");
    assert!(
        complaint(&without).contains("PLS-00103"),
        "a block that lost its terminator is not a block: {}",
        complaint(&without)
    );
}

#[test]
fn a_hundred_kilobyte_clob_comes_back_whole() {
    let connection = connection!();
    let events = run(&connection, "select body from bench.big_text order by id");
    let rows = rows(&events);
    assert_eq!(rows[0], [Cell::Text("short body".to_owned())]);
    assert_eq!(rows[1], [Cell::Null]);
    let Cell::Text(body) = &rows[2][0] else {
        panic!("a CLOB is text: {:?}", rows[2]);
    };
    assert_eq!(body.chars().count(), 102_400);
    assert!(body.starts_with("Lorem ipsuLorem ipsu"), "{}", &body[..40]);
    assert!(body.ends_with("Lorem ipsu"), "{}", &body[body.len() - 40..]);
    assert!(!body.ends_with('…'), "well under the one mebibyte ceiling");
}

#[test]
fn a_blob_comes_back_as_bytes() {
    let connection = connection!();
    let events = run(
        &connection,
        "select data from bench.binary_blobs order by id",
    );
    let rows = rows(&events);
    assert_eq!(rows[0], [Cell::Bytes(vec![1, 2, 3, 4, 5])]);
    let Cell::Bytes(data) = &rows[1][0] else {
        panic!("a BLOB is bytes: {:?}", rows[1]);
    };
    assert_eq!(data.len(), 2000);
    assert_eq!(&data[..4], b"ABAB");
}

#[test]
fn cancel_ends_a_running_query_and_the_next_one_reconnects() {
    let connection = connection!();
    // A cartesian join and not `dbms_session.sleep(10)`: an OCI break stops the
    // call the server is working on, and a server asleep inside PL/SQL is not
    // working on anything — it sleeps the ten seconds out and only then looks.
    let events = connection.query(
        "select count(*) from bench.events e1, bench.events e2",
        QueryOptions::default(),
    );
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    connection.cancel();
    let event = events.recv_timeout(Duration::from_secs(5));
    let elapsed = started.elapsed();
    assert_eq!(event, Ok(QueryEvent::Error(DbError::Cancelled)));
    eprintln!("cancel answered in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual")),
        [[Cell::Decimal("1".to_owned())]],
        "the interrupted session is thrown away and the next query opens one"
    );
}

#[test]
fn a_refused_login_is_a_connect_error() {
    let Some((config, mut spec)) = local() else {
        return;
    };
    spec.password = Some(config::Password::Literal("not the password".to_owned()));
    let failure = Connection::open(&spec, &config).expect_err("bench has a password");
    let DbError::Connect(message) = &failure else {
        panic!("{failure:?}");
    };
    assert!(message.starts_with("ORA-01017"), "{message}");
}
