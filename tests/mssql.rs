//! The SQL Server driver against the container `scripts/db-up.sh` seeds.
//!
//! Skipped unless `SQL_BENCH_TEST_DBS` is set, so `cargo test` on a machine
//! with no database is still green. The connection comes from the committed
//! `config.local.toml`, which is what the dev loop uses.

use std::path::Path;
use std::time::{Duration, Instant};

use sql_bench::config;
use sql_bench::db::Connection;
use sql_bench::db::model::{Cell, Column, DbError, QueryEvent, QueryOptions};

/// The committed `local-mssql`, or nothing when the databases are not wanted.
fn local() -> Option<(config::Config, config::Connection)> {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return None;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml");
    let config = config::load(&path, true).expect("config.local.toml is readable");
    let spec = config
        .connection("local-mssql")
        .expect("config.local.toml names local-mssql")
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

#[test]
fn select_one_row() {
    let connection = connection!();
    let events = run(&connection, "select 1 as one");
    assert_eq!(
        columns(&events),
        [[Column {
            name: "one".to_owned(),
            type_name: "int".to_owned()
        }]]
    );
    assert_eq!(rows(&events), [[Cell::Int(1)]]);
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
            "int",
            "bit",
            "tinyint",
            "smallint",
            "int",
            "bigint",
            "decimal",
            "numeric",
            "money",
            "float",
            "real",
            "char",
            "varchar",
            "nvarchar",
            "text",
            "date",
            "time",
            "datetime",
            "datetime2",
            "datetimeoffset",
            "uniqueidentifier",
            "varbinary",
            "xml",
        ]
    );

    let rows = rows(&events);
    assert_eq!(
        rows[0],
        [
            Cell::Int(1),
            Cell::Bool(true),
            Cell::Int(255),
            Cell::Int(32767),
            Cell::Int(2_147_483_647),
            Cell::Int(9_223_372_036_854_775_807),
            Cell::Decimal("12345.6789".to_owned()),
            Cell::Decimal("123.45".to_owned()),
            Cell::Decimal("1234.5678".to_owned()),
            Cell::Float(12_345_678_901.234),
            Cell::Float(1.25),
            Cell::Text("abcde".to_owned()),
            Cell::Text("varchar value".to_owned()),
            Cell::Text("nvarchar value".to_owned()),
            Cell::Text("text value".to_owned()),
            Cell::DateTime("2024-05-17".to_owned()),
            Cell::DateTime("13:45:30.1234567".to_owned()),
            Cell::DateTime("2024-05-17T13:45:30.000".to_owned()),
            Cell::DateTime("2024-05-17T13:45:30.1234567".to_owned()),
            Cell::DateTime("2024-05-17T13:45:30.1234567+02:00".to_owned()),
            Cell::Text("6f9619ff-8b86-d011-b42d-00c04fc964ff".to_owned()),
            Cell::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            Cell::Text("<root><a id=\"1\">x</a></root>".to_owned()),
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
        message.contains("Incorrect syntax near the keyword 'from'"),
        "{message}"
    );
    assert_eq!(*line, Some(1));
    assert_eq!(events.len(), 1, "an error ends the query");

    // The connection is still good: a server complaint is not a broken socket.
    assert_eq!(rows(&run(&connection, "select 1 as one")), [[Cell::Int(1)]]);
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

    // The cap throws the connection away, so the next query has to reconnect
    // without anyone asking.
    assert_eq!(rows(&run(&connection, "select 1 as one")), [[Cell::Int(1)]]);
}

#[test]
fn an_update_reports_what_it_changed_and_rolls_back() {
    let connection = connection!();
    assert!(matches!(
        run(&connection, "begin transaction").first(),
        Some(QueryEvent::RowsAffected(_))
    ));

    let events = run(&connection, "update bench.customers set country = 'ZZ'");
    assert_eq!(events[0], QueryEvent::RowsAffected(50));
    assert_eq!(done(&events), (0, false));

    assert_eq!(
        rows(&run(
            &connection,
            "select count(*) as changed from bench.customers where country = 'ZZ'"
        )),
        [[Cell::Int(50)]],
        "the transaction sees its own update"
    );

    run(&connection, "rollback transaction");
    assert_eq!(
        rows(&run(
            &connection,
            "select count(*) as changed from bench.customers where country = 'ZZ'"
        )),
        [[Cell::Int(0)]],
        "and the rollback puts the seed data back"
    );
}

#[test]
fn one_batch_can_have_two_result_sets() {
    let connection = connection!();
    let events = run(
        &connection,
        "select 1 as first; select 2 as second, 'two' as spelled",
    );
    let sets = columns(&events);
    assert_eq!(
        sets.iter()
            .map(|set| set.iter().map(|c| c.name.as_str()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [vec!["first"], vec!["second", "spelled"]]
    );
    assert_eq!(
        rows(&events),
        [
            vec![Cell::Int(1)],
            vec![Cell::Int(2), Cell::Text("two".to_owned())]
        ]
    );
    assert_eq!(done(&events), (2, false));
}

#[test]
fn cancel_ends_a_waiting_query_and_the_next_one_reconnects() {
    let connection = connection!();
    let events = connection.query("waitfor delay '00:00:10'", QueryOptions::default());
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    connection.cancel();
    let event = events.recv_timeout(Duration::from_secs(5));
    let elapsed = started.elapsed();
    assert_eq!(event, Ok(QueryEvent::Error(DbError::Cancelled)));
    eprintln!("cancel answered in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

    assert_eq!(rows(&run(&connection, "select 1 as one")), [[Cell::Int(1)]]);
}

#[test]
fn a_refused_login_is_a_connect_error() {
    let Some((config, mut spec)) = local() else {
        return;
    };
    spec.password = Some(config::Password::Literal("not the password".to_owned()));
    let failure = Connection::open(&spec, &config).expect_err("sa has a password");
    let DbError::Connect(message) = &failure else {
        panic!("{failure:?}");
    };
    assert!(message.contains("Login failed"), "{message}");
}

#[test]
fn a_refused_login_never_repeats_the_password() {
    let Some((config, mut spec)) = local() else {
        return;
    };
    const SECRET: &str = "Sup3rSecret_Wrong!";
    spec.password = Some(config::Password::Literal(SECRET.to_owned()));
    let failure = Connection::open(&spec, &config).expect_err("sa has a password");
    assert!(
        !failure.to_string().contains(SECRET),
        "the password came back in the message: {failure}"
    );
}

#[test]
fn an_unreachable_host_gives_up_inside_the_connect_timeout() {
    let Some((config, mut spec)) = local() else {
        return;
    };
    // A black hole rather than a closed port: a refusal comes back at once and
    // would prove nothing about the timeout.
    spec.host = "10.255.255.1".to_owned();
    let started = Instant::now();
    let failure = Connection::open(&spec, &config).expect_err("nothing answers there");
    let elapsed = started.elapsed();
    eprintln!("an unreachable host failed in {elapsed:?}: {failure}");
    let DbError::Connect(message) = &failure else {
        panic!("a host that never answers is a connect failure: {failure:?}");
    };
    assert!(message.contains("10.255.255.1"), "{message}");
    assert!(
        elapsed < Duration::from_secs(12),
        "the ten second timeout let it run for {elapsed:?}"
    );
}

#[test]
fn a_hundred_kilobyte_nvarchar_max_comes_back_whole() {
    let connection = connection!();
    let events = run(&connection, "select body from bench.big_text order by id");
    let rows = rows(&events);
    assert_eq!(rows[0], [Cell::Text("short body".to_owned())]);
    assert_eq!(rows[1], [Cell::Null]);
    let Cell::Text(body) = &rows[2][0] else {
        panic!("an nvarchar(max) is text: {:?}", rows[2]);
    };
    assert_eq!(body.chars().count(), 102_400);
    assert!(body.starts_with("Lorem ipsuLorem ipsu"), "{}", &body[..40]);
    assert!(body.ends_with("Lorem ipsu"), "{}", &body[body.len() - 40..]);
}

#[test]
fn a_procedure_that_returns_nothing_has_no_columns_and_no_rows() {
    let connection = connection!();
    // Rolled back in the same batch, so the seed data is what it was.
    let events = run(
        &connection,
        "begin transaction; exec bench.sp_mark_shipped 1; rollback transaction",
    );
    assert!(columns(&events).is_empty(), "{events:?}");
    assert!(rows(&events).is_empty(), "{events:?}");
    assert_eq!(done(&events), (0, false));

    assert_eq!(
        rows(&run(
            &connection,
            "select status from bench.orders where id = 1"
        )),
        [[Cell::Text("PAID".to_owned())]],
        "the rollback left the order alone"
    );
}

#[test]
fn three_hundred_columns_all_arrive() {
    let connection = connection!();
    let select: Vec<String> = (1..=300).map(|n| format!("{n} as c{n}")).collect();
    let events = run(&connection, &format!("select {}", select.join(", ")));
    let header = &columns(&events)[0];
    assert_eq!(header.len(), 300);
    assert_eq!(header[299].name, "c300");
    assert_eq!(rows(&events)[0].len(), 300);
    assert_eq!(rows(&events)[0][299], Cell::Int(300));
}

#[test]
fn a_column_name_may_have_spaces_and_letters_no_keyboard_has() {
    let connection = connection!();
    let events = run(&connection, "select 1 as [Größe des Kunden]");
    assert_eq!(columns(&events)[0][0].name, "Größe des Kunden");
    assert_eq!(rows(&events), [[Cell::Int(1)]]);
}

#[test]
fn two_columns_of_the_same_name_both_arrive() {
    let connection = connection!();
    let events = run(&connection, "select 1 as a, 2 as a");
    let header = &columns(&events)[0];
    assert_eq!(
        header.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["a", "a"],
        "the driver reports what the server said, twice"
    );
    assert_eq!(rows(&events), [[Cell::Int(1), Cell::Int(2)]]);
}

#[test]
fn a_connection_the_server_closes_is_opened_again_for_the_next_query() {
    let connection = connection!();
    // Severity 20 is fatal: the server logs it and closes the connection,
    // which is what a database going away looks like from here without
    // stopping one. What the handle must not do is keep the dead socket.
    let events = run(
        &connection,
        "raiserror('sql-bench T2.5: a fatal error on purpose', 20, 1) with log",
    );
    assert!(
        matches!(events[0], QueryEvent::Error(_)),
        "the server said no: {events:?}"
    );

    assert_eq!(
        rows(&run(&connection, "select 1 as one")),
        [[Cell::Int(1)]],
        "the next query has to open a new connection, not reuse the dead one"
    );
}
