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
use sql_bench::db::model::{Cell, Column, DbError, QueryEvent, QueryOptions};
use sql_bench::db::{Connection, catalog};

/// The committed `local-oracle`, or nothing when the databases are not wanted.
fn local() -> Option<(config::Config, config::Connection)> {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return None;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml");
    let config = config::load(&path, true).expect("config.local.toml is readable");
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
fn an_insert_commits_without_being_asked() {
    let Some((config, spec)) = local() else {
        return;
    };
    let connection = Connection::open(&spec, &config).expect("the container is up");
    run(&connection, "drop table zz_polish_commit");
    run(&connection, "create table zz_polish_commit (id number)");
    let events = run(
        &connection,
        "insert into zz_polish_commit select level from dual connect by level <= 3",
    );
    assert_eq!(events[0], QueryEvent::RowsAffected(3));
    assert_eq!(done(&events), (0, false));

    // Another session sees the rows: nothing is waiting on a commit that a
    // reconnect or a cancel would quietly roll back.
    let other = Connection::open(&spec, &config).expect("the container is up");
    assert_eq!(
        rows(&run(&other, "select count(*) as n from zz_polish_commit")),
        [[Cell::Decimal("3".to_owned())]]
    );
    run(&connection, "drop table zz_polish_commit");
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
fn a_procedure_from_the_pad_compiles_or_says_why_not() {
    let connection = connection!();
    let good = run(
        &connection,
        "create or replace procedure zz_polish_p is\nbegin\n  null;\nend;",
    );
    assert_eq!(good[0], QueryEvent::RowsAffected(0), "{good:?}");
    assert_eq!(
        rows(&run(
            &connection,
            "select status from user_objects where object_name = 'ZZ_POLISH_P'"
        )),
        [[Cell::Text("VALID".to_owned())]],
        "the end; that closes it went to the server"
    );

    let bad = run(
        &connection,
        "create or replace procedure zz_polish_p is\nbegin\n  nope;\nend;",
    );
    let QueryEvent::Error(DbError::Query { message, line }) = &bad[0] else {
        panic!("an INVALID procedure is not a success: {bad:?}");
    };
    assert!(message.starts_with("PLS-00201"), "{message}");
    assert_eq!(*line, Some(3), "the line nope is on");

    // A qualified package body is looked up under its own owner and type.
    run(
        &connection,
        "create or replace package bench.zz_polish_pk as procedure x; end;",
    );
    let body = run(
        &connection,
        "create or replace package body bench.zz_polish_pk as\n\
         procedure x is begin undefined_thing; end;\nend;",
    );
    let QueryEvent::Error(DbError::Query { message, line }) = &body[0] else {
        panic!("{body:?}");
    };
    assert!(message.starts_with("PLS-00201"), "{message}");
    assert_eq!(*line, Some(2));

    run(&connection, "drop package bench.zz_polish_pk");
    run(&connection, "drop procedure zz_polish_p");
}

#[test]
fn a_message_ends_where_the_server_stopped_talking_about_it() {
    let connection = connection!();
    let message = complaint(&run(&connection, "select 1/0 from dual"));
    assert_eq!(message, "ORA-01476: divisor is equal to zero");
}

#[test]
fn a_binary_float_reads_as_the_number_it_was_written_as() {
    let connection = connection!();
    assert_eq!(
        rows(&run(
            &connection,
            "select cast(0.1 as binary_float) as f from dual"
        )),
        [[Cell::Float(0.1)]]
    );
}

#[test]
fn a_multibyte_clob_comes_back_whole_across_many_reads() {
    let connection = connection!();
    run(&connection, "drop table zz_polish_clob");
    run(
        &connection,
        "create table zz_polish_clob (id number, c clob)",
    );
    // Two, three and four byte characters, and a mix that leaves a read's
    // buffer a few bytes short of full: a read that stops short is not the
    // end of the LOB unless it is short by more than a character.
    let pieces = ["é", "日", "😀", "a日é😀"];
    for (id, piece) in pieces.iter().enumerate() {
        let events = run(
            &connection,
            &format!(
                "declare l clob; p varchar2(32767); begin \
                   for i in 1..1000 loop p := p || '{piece}'; end loop; \
                   insert into zz_polish_clob values ({id}, empty_clob()) returning c into l; \
                   for i in 1..100 loop dbms_lob.writeappend(l, length(p), p); end loop; \
                 end;"
            ),
        );
        assert_eq!(done(&events), (0, false), "{events:?}");
    }
    let events = run(
        &connection,
        "select c, dbms_lob.getlength(c) as units from zz_polish_clob order by id",
    );
    run(&connection, "drop table zz_polish_clob");
    for (row, piece) in rows(&events).iter().zip(pieces) {
        let Cell::Text(text) = &row[0] else {
            panic!("a CLOB is text: {row:?}");
        };
        assert!(!text.ends_with('…'), "under the ceiling");
        assert_eq!(
            row[1].display(),
            text.encode_utf16().count().to_string(),
            "every UTF-16 unit the server holds of {piece}"
        );
        // `writeappend` counts a four byte character as two, so how many
        // arrived is the server's business; that each is whole is ours.
        assert_eq!(*text, piece.repeat(text.len() / piece.len()));
    }
}

#[test]
fn a_blob_past_the_ceiling_keeps_its_first_mebibyte() {
    let connection = connection!();
    run(&connection, "drop table zz_polish_blob");
    run(&connection, "create table zz_polish_blob (b blob)");
    // 0xff is never UTF-8, which is how a BLOB used to lose every byte.
    run(
        &connection,
        "declare l blob; begin \
           insert into zz_polish_blob values (empty_blob()) returning b into l; \
           for i in 1..40 loop \
             dbms_lob.writeappend(l, 32767, utl_raw.copies(hextoraw('FF'), 32767)); \
           end loop; \
         end;",
    );
    let events = run(&connection, "select b from zz_polish_blob");
    run(&connection, "drop table zz_polish_blob");
    let Cell::Bytes(data) = &rows(&events)[0][0] else {
        panic!("a BLOB is bytes: {events:?}");
    };
    assert_eq!(data.len(), 1024 * 1024);
    assert!(data.iter().all(|byte| *byte == 0xff));
}

#[test]
fn a_quoted_mixed_case_table_has_columns_too() {
    let connection = connection!();
    run(&connection, "drop table \"zz_Polish_Mixed\"");
    run(&connection, "create table \"zz_Polish_Mixed\" (id number)");
    let exact = catalog::list_columns(&connection, "BENCH", "zz_Polish_Mixed");
    let folded = catalog::list_columns(&connection, "bench", "customers");
    run(&connection, "drop table \"zz_Polish_Mixed\"");
    let names = |columns: Vec<catalog::ColumnInfo>| {
        columns
            .into_iter()
            .map(|column| column.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(exact.unwrap()), ["ID"], "the name as it was quoted");
    assert_eq!(
        names(folded.unwrap()).len(),
        6,
        "and an unquoted name still folds"
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

#[test]
fn a_refused_login_never_repeats_the_password() {
    let Some((config, mut spec)) = local() else {
        return;
    };
    const SECRET: &str = "Sup3rSecret_Wrong!";
    spec.password = Some(config::Password::Literal(SECRET.to_owned()));
    let failure = Connection::open(&spec, &config).expect_err("bench has a password");
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
    assert!(message.starts_with("ORA-12170"), "{message}");
    assert!(
        elapsed < Duration::from_secs(12),
        "the ten second timeout let it run for {elapsed:?}"
    );
}

#[test]
fn a_procedure_that_returns_nothing_has_no_columns_and_no_rows() {
    let connection = connection!();
    // The procedure commits, so the block puts the status back itself rather
    // than trusting a rollback that has nothing left to undo.
    let events = run(
        &connection,
        "begin \
           bench.mark_shipped(1); \
           update bench.orders set status = 'PAID' where id = 1; \
           commit; \
         end;",
    );
    assert!(columns(&events).is_empty(), "{events:?}");
    assert!(rows(&events).is_empty(), "{events:?}");
    assert_eq!(events[0], QueryEvent::RowsAffected(0));
    assert_eq!(done(&events), (0, false));

    assert_eq!(
        rows(&run(
            &connection,
            "select status from bench.orders where id = 1"
        )),
        [[Cell::Text("PAID".to_owned())]],
        "the block put the order back"
    );
}

#[test]
fn three_hundred_columns_all_arrive() {
    let connection = connection!();
    let select: Vec<String> = (1..=300).map(|n| format!("{n} as c{n}")).collect();
    let events = run(
        &connection,
        &format!("select {} from dual", select.join(", ")),
    );
    let header = &columns(&events)[0];
    assert_eq!(header.len(), 300);
    assert_eq!(header[299].name, "C300");
    assert_eq!(rows(&events)[0].len(), 300);
    assert_eq!(rows(&events)[0][299], Cell::Decimal("300".to_owned()));
}

#[test]
fn a_column_name_may_have_spaces_and_letters_no_keyboard_has() {
    let connection = connection!();
    let events = run(&connection, "select 1 as \"Größe des Kunden\" from dual");
    assert_eq!(columns(&events)[0][0].name, "Größe des Kunden");
    assert_eq!(rows(&events), [[Cell::Decimal("1".to_owned())]]);
}

#[test]
fn two_columns_of_the_same_name_both_arrive() {
    let connection = connection!();
    let events = run(&connection, "select 1 as a, 2 as a from dual");
    let header = &columns(&events)[0];
    assert_eq!(
        header.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["A", "A"],
        "the driver reports what the server said, twice"
    );
    assert_eq!(
        rows(&events),
        [[Cell::Decimal("1".to_owned()), Cell::Decimal("2".to_owned())]]
    );
}

/// The password `compose.yaml` gives SYSTEM, which is the only way to end
/// somebody else's session — `bench` has no ALTER SYSTEM of its own.
const SYSTEM_PASSWORD: &str = "Bench_Pass1!";

#[test]
fn a_session_the_server_ends_is_opened_again_for_the_next_query() {
    let Some((config, spec)) = local() else {
        return;
    };
    let connection = Connection::open(&spec, &config).expect("the container is up");
    // Whatever the column type, the number is what it reads as.
    let one = |events: &[QueryEvent]| rows(events)[0][0].display().into_owned();
    let sid = one(&run(
        &connection,
        "select sys_context('userenv', 'sid') as sid from dual",
    ));

    // A second connection, because a session cannot end itself: this is what
    // a database going away looks like without stopping the container.
    let mut dba = spec.clone();
    dba.user = "system".to_owned();
    dba.password = Some(config::Password::Literal(SYSTEM_PASSWORD.to_owned()));
    let dba = Connection::open(&dba, &config).expect("SYSTEM is the compose password");
    let serial = one(&run(
        &dba,
        &format!("select serial# as s from v$session where sid = {sid}"),
    ));
    run(
        &dba,
        &format!("alter system disconnect session '{sid},{serial}' immediate"),
    );

    // The dead session answers once with the server's complaint...
    let events = run(&connection, "select 1 as one from dual");
    assert!(
        matches!(events[0], QueryEvent::Error(_)),
        "the session is gone: {events:?}"
    );
    // ...and then the handle has to open a new one rather than keep it.
    assert_eq!(
        rows(&run(&connection, "select 1 as one from dual")),
        [[Cell::Decimal("1".to_owned())]],
        "the next query has to open a new session, not reuse the dead one"
    );
}
