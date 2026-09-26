//! The run loop's connections against the SQL Server container: what goes
//! down a tab's one worker, and what a cancel reaches.
//!
//! Skipped unless `SQL_BENCH_TEST_DBS` is set, like the driver tests, and
//! reading the same committed `config.local.toml`.

use std::path::Path;
use std::time::{Duration, Instant};

use sql_bench::app::App;
use sql_bench::app::results::Status;
use sql_bench::config;
use sql_bench::db::model::DbError;
use sql_bench::run::{Pending, Runtime};
use sql_bench::trace::Trace;

/// The app and runtime for `config.local.toml`, or nothing when the
/// databases are not wanted.
fn local() -> Option<(App, Runtime)> {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return None;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml");
    let config = config::load(&path, true).expect("config.local.toml is readable");
    Some((App::new(&config), Runtime::new(&config)))
}

/// One turn of the loop's polling, for `for` long.
fn turn_for(runtime: &mut Runtime, app: &mut App, for_: Duration) {
    let until = Instant::now() + for_;
    while Instant::now() < until {
        runtime.poll_connections(app);
        runtime.poll_queries(app, &Trace::default());
        runtime.poll_catalog(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Turns until nothing is in flight, and says how long that took.
fn settle(runtime: &mut Runtime, app: &mut App) -> Duration {
    let started = Instant::now();
    while app.busy() {
        assert!(started.elapsed() < Duration::from_secs(20), "still busy");
        turn_for(runtime, app, Duration::from_millis(10));
    }
    started.elapsed()
}

#[test]
fn esc_stops_the_query_and_not_the_index_the_connect_asked_for() {
    let Some((mut app, mut runtime)) = local() else {
        return;
    };
    runtime.connect(&mut app, 0);
    turn_for(&mut runtime, &mut app, Duration::ZERO);
    while app.connecting() {
        turn_for(&mut runtime, &mut app, Duration::from_millis(10));
    }
    // Run at once, while the schema list and the index are still to come,
    // and cancel once the query has had time to reach the server.
    let waiting = Pending::Statement("waitfor delay '00:00:05'".to_owned());
    runtime.run(&mut app, 0, waiting);
    turn_for(&mut runtime, &mut app, Duration::from_millis(500));
    runtime.cancel(&mut app, 0);
    let took = settle(&mut runtime, &mut app);
    assert!(took < Duration::from_secs(3), "the cancel took {took:?}");
    assert!(
        matches!(
            app.tabs[0].results.status,
            Status::Failed {
                error: DbError::Cancelled,
                ..
            }
        ),
        "{:?}",
        app.tabs[0].results.status
    );
    assert_eq!(app.shell.error, None, "nothing else was cancelled");
    assert!(
        app.tabs[0].objects.index().is_some(),
        "the index came after the query, not with its cancel"
    );

    // A query still waiting for its turn is cancelled without being sent.
    runtime.load(&mut app, 0, sql_bench::db::catalog::CatalogRequest::Index);
    runtime.run(&mut app, 0, Pending::Statement("select 1".to_owned()));
    runtime.cancel(&mut app, 0);
    assert!(!app.tabs[0].results.running(), "answered at once");
    settle(&mut runtime, &mut app);
    assert!(app.tabs[0].objects.index().is_some());
    assert_eq!(app.shell.error, None);
}

#[test]
fn m_raises_the_cap_for_its_statement_and_the_next_run_starts_from_max_rows() {
    let Some((mut app, mut runtime)) = local() else {
        return;
    };
    runtime.set_max_rows(5);
    runtime.connect(&mut app, 0);
    settle(&mut runtime, &mut app);
    let scan = || Pending::Statement("select top 50 id from bench.events order by id".to_owned());
    let fetched = |app: &App| app.tabs[0].results.rows().len();

    runtime.run(&mut app, 0, scan());
    settle(&mut runtime, &mut app);
    assert_eq!(fetched(&app), 5);
    runtime.more_rows(&mut app, 0);
    settle(&mut runtime, &mut app);
    assert_eq!(
        fetched(&app),
        50,
        "10,000 more allowed, and 50 is all there is"
    );

    runtime.run(&mut app, 0, scan());
    settle(&mut runtime, &mut app);
    assert_eq!(fetched(&app), 5, "a new run is capped at --max-rows again");
}

#[test]
fn a_run_stops_at_a_statement_whose_scan_the_cap_cut_short() {
    let Some((mut app, mut runtime)) = local() else {
        return;
    };
    runtime.set_max_rows(1);
    runtime.connect(&mut app, 0);
    settle(&mut runtime, &mut app);
    let script = ["select name from sys.objects", "select db_name() as db"];
    app.tabs[0].results.expect(vec![0..1, 1..2], 0);
    runtime.run(
        &mut app,
        0,
        Pending::All(script.iter().map(|sql| (*sql).to_owned()).collect()),
    );
    settle(&mut runtime, &mut app);
    assert_eq!(app.tabs[0].results.sets(), 1, "the second never ran");
    assert_eq!(
        app.shell.status,
        "row cap: statement 1 of 2 reset the session, so the 1 after it did not run"
    );
}
