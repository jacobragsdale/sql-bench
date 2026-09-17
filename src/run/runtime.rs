//! The connections behind the tabs: one worker per connect attempt, one
//! `Option<db::Connection>` per tab, and a poll the loop takes each turn.
//!
//! Rule 1 in `CLAUDE.md` is that the app never blocks on a database, and
//! [`db::Connection::open`] blocks for up to ten seconds — so it is called on
//! a thread of its own, which also keeps a slow `password_cmd` off the loop.
//! The app is told what happened through [`RuntimeEvent`], never by this
//! module reaching into its state.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::app::results::MORE_ROWS;
use crate::app::{App, RuntimeEvent};
use crate::cli::Cli;
use crate::config::Config;
use crate::db::catalog::CatalogRequest;
use crate::db::model::{Cell, QueryEvent, QueryOptions};
use crate::db::{self, Connection};
use crate::trace::Trace;

/// How many rows the driver reports at a time. Small enough that the grid
/// fills while the scan runs, big enough that a million rows are not a
/// million channel messages.
const BATCH: usize = 500;

/// What a tab was asked to run while it was still connecting, kept until the
/// connection is up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pending {
    Statement(String),
    All(Vec<String>),
}

/// Every tab's connection, in the order the tabs are in.
#[derive(Debug, Default)]
pub struct Runtime {
    config: Config,
    tabs: Vec<TabRuntime>,
    /// What `--max-rows` said, which is every tab's cap until `m` raises it.
    max_rows: usize,
}

/// One tab's connection, the attempt that may be in flight, the one request
/// waiting on it and the query it is running.
#[derive(Debug, Default)]
pub struct TabRuntime {
    connection: Option<Connection>,
    /// The connect thread's answer, until it arrives.
    pending: Option<Receiver<Result<Connection, db::model::DbError>>>,
    started: Option<Instant>,
    /// The query or browse that asked for this connection. One slot: a
    /// person who asks twice while a connection opens means the second one.
    queued: Option<Pending>,
    /// The query in flight, if there is one.
    running: Option<Running>,
    /// The cap this tab's queries run with; `m` raises it by [`MORE_ROWS`].
    cap: usize,
    /// The last statement run, so `m` can run it again for more of it.
    last: Option<String>,
    /// The catalog query the object tree is waiting on, and the ones behind
    /// it. A tab's worker takes one request at a time, so two branches
    /// opened in the same breath are run one after the other rather than
    /// racing for the one channel.
    catalog: Option<Loading>,
    waiting: VecDeque<CatalogRequest>,
}

/// One catalog query in flight: what was asked, where the rows arrive and
/// the ones that have.
#[derive(Debug)]
struct Loading {
    request: CatalogRequest,
    events: Receiver<QueryEvent>,
    rows: Vec<Vec<Cell>>,
}

/// What one statement is being started as: the SQL, what follows it in a
/// run-all, and where it is in that run.
#[derive(Debug)]
struct Start {
    sql: String,
    rest: Vec<String>,
    statement: usize,
    of: usize,
    /// `m` asking for more rows of the same statement, which keeps the grid
    /// where the person left it.
    keep_view: bool,
}

/// One query in flight: where its events arrive, what is still to run after
/// it, and what it has cost so far.
#[derive(Debug)]
struct Running {
    events: Receiver<QueryEvent>,
    /// The statements of a run-all still to come, in order.
    rest: Vec<String>,
    statement: usize,
    of: usize,
    started: Instant,
    rows: usize,
    batches: usize,
    first_batch: Option<Duration>,
}

impl Runtime {
    #[must_use]
    pub fn new(config: &Config) -> Self {
        let max_rows = QueryOptions::default().max_rows.unwrap_or(10_000);
        Self {
            config: config.clone(),
            tabs: (0..config.connections.len())
                .map(|_| TabRuntime {
                    cap: max_rows,
                    ..TabRuntime::default()
                })
                .collect(),
            max_rows,
        }
    }

    /// What `--max-rows` asked for, before anything has been run.
    pub fn set_max_rows(&mut self, max_rows: usize) {
        self.max_rows = max_rows;
        for tab in &mut self.tabs {
            tab.cap = max_rows;
        }
    }

    /// Open (or re-open) this tab's connection on a thread of its own.
    ///
    /// Re-connecting drops the connection that is there, which cancels
    /// whatever it was running. A second `c` while one is already in flight
    /// is ignored: the attempt has its own ten second timeout.
    pub fn connect(&mut self, app: &mut App, tab: usize) {
        let (Some(runtime), Some(spec)) =
            (self.tabs.get_mut(tab), self.config.connections.get(tab))
        else {
            return;
        };
        if runtime.pending.is_some() {
            return;
        }
        runtime.connection = None;
        let (reply, answer) = mpsc::channel();
        let (spec, config) = (spec.clone(), self.config.clone());
        let started = std::thread::Builder::new()
            .name(format!("connect {}", spec.name))
            .spawn(move || {
                // The password is resolved inside `open`, on this thread, so
                // a `password_cmd` that takes a second costs the UI nothing.
                let _ = reply.send(Connection::open(&spec, &config));
            });
        match started {
            Ok(_) => {
                runtime.pending = Some(answer);
                runtime.started = Some(Instant::now());
                app.apply(RuntimeEvent::Connecting { tab });
            }
            Err(why) => app.apply(RuntimeEvent::Failed {
                tab,
                message: format!("no connect thread: {why}"),
            }),
        }
    }

    /// Close this tab's connection, and forget an attempt still in flight.
    ///
    /// Dropping the handle cancels the query it was running before it joins
    /// the worker, which is why nothing else has to be stopped first.
    pub fn disconnect(&mut self, app: &mut App, tab: usize) {
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        runtime.connection = None;
        runtime.pending = None;
        runtime.started = None;
        runtime.queued = None;
        runtime.running = None;
        runtime.catalog = None;
        runtime.waiting.clear();
        app.apply(RuntimeEvent::Disconnected { tab });
    }

    /// Collect whatever the connect threads have finished, and say whether
    /// the screen has to be painted again.
    pub fn poll_connections(&mut self, app: &mut App) -> bool {
        let mut dirty = false;
        for tab in 0..self.tabs.len() {
            let Some(runtime) = self.tabs.get_mut(tab) else {
                continue;
            };
            let Some(answer) = runtime.pending.as_ref() else {
                continue;
            };
            let outcome = match answer.try_recv() {
                Err(TryRecvError::Empty) => continue,
                Ok(outcome) => outcome,
                // The thread went without answering, which only a panic does.
                Err(TryRecvError::Disconnected) => Err(db::model::DbError::Connect(
                    "the connect thread stopped".to_owned(),
                )),
            };
            let elapsed = runtime.started.take().map(|at| at.elapsed());
            #[allow(clippy::cast_possible_truncation)]
            let connect_ms = elapsed.map_or(0, |elapsed| elapsed.as_millis() as u32);
            runtime.pending = None;
            let event = match outcome {
                Ok(connection) => {
                    runtime.connection = Some(connection);
                    RuntimeEvent::Connected { tab, connect_ms }
                }
                Err(error) => {
                    runtime.queued = None;
                    RuntimeEvent::Failed {
                        tab,
                        message: self.message(tab, &error),
                    }
                }
            };
            let connected = matches!(event, RuntimeEvent::Connected { .. });
            app.apply(event);
            dirty = true;
            if connected {
                // The tree is empty until something asks: this is the ask —
                // and the index behind it is what Ctrl-P searches and what
                // every branch fills from without asking again.
                self.load(app, tab, CatalogRequest::Schemas);
                self.load(app, tab, CatalogRequest::Index);
            }
            if let Some(request) = self
                .tabs
                .get_mut(tab)
                .filter(|runtime| runtime.connection.is_some())
                .and_then(|runtime| runtime.queued.take())
            {
                self.start(app, tab, request);
            }
        }
        dirty
    }

    /// Run this request on the tab, or connect first and run it when the
    /// connection is up.
    pub fn run(&mut self, app: &mut App, tab: usize, request: Pending) {
        if self.connection(tab).is_some() {
            self.start(app, tab, request);
        } else {
            self.connect(app, tab);
            self.queue(app, tab, request);
        }
    }

    /// The first statement of a request; a run-all keeps the rest for when
    /// this one is done.
    fn start(&mut self, app: &mut App, tab: usize, request: Pending) {
        let (sql, rest) = match request {
            Pending::Statement(sql) => (sql, Vec::new()),
            Pending::All(mut statements) => {
                if statements.is_empty() {
                    return;
                }
                let rest = statements.split_off(1);
                (statements.remove(0), rest)
            }
        };
        let of = rest.len() + 1;
        self.begin(
            app,
            tab,
            Start {
                sql,
                rest,
                statement: 0,
                of,
                keep_view: false,
            },
        );
    }

    /// Hand one statement to the driver and tell the app it is running.
    fn begin(&mut self, app: &mut App, tab: usize, start: Start) {
        let Start {
            sql,
            rest,
            statement,
            of,
            keep_view,
        } = start;
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        let Some(connection) = runtime.connection.as_ref() else {
            // Disconnected between the key and the turn: say so rather than
            // leave a pane that says `Running` for ever.
            app.apply(RuntimeEvent::Query {
                tab,
                event: QueryEvent::Error(db::model::DbError::Connect("not connected".to_owned())),
            });
            return;
        };
        let events = connection.query(
            &sql,
            QueryOptions {
                batch_size: BATCH,
                max_rows: Some(runtime.cap),
            },
        );
        let started = Instant::now();
        runtime.last = Some(sql);
        runtime.running = Some(Running {
            events,
            rest,
            statement,
            of,
            started,
            rows: 0,
            batches: 0,
            first_batch: None,
        });
        app.apply(RuntimeEvent::QueryStarted {
            tab,
            at: started,
            statement,
            of,
            keep_view,
        });
    }

    /// Everything the running queries have reported since the last turn, in
    /// one go: the loop paints once for however many batches arrived.
    pub fn poll_queries(&mut self, app: &mut App, trace: &Trace) -> bool {
        let mut dirty = false;
        for tab in 0..self.tabs.len() {
            let mut next = None;
            let mut finished = false;
            if let Some(runtime) = self.tabs.get_mut(tab)
                && let Some(running) = runtime.running.as_mut()
            {
                loop {
                    let event = match running.events.try_recv() {
                        Ok(event) => event,
                        Err(TryRecvError::Empty) => break,
                        // Only a panicked worker ends the channel without
                        // saying why, and a grid waiting for ever is worse.
                        Err(TryRecvError::Disconnected) => {
                            QueryEvent::Error(db::model::DbError::Connect(
                                "the connection's worker has stopped".to_owned(),
                            ))
                        }
                    };
                    dirty = true;
                    match &event {
                        QueryEvent::Rows(batch) => {
                            running.rows += batch.len();
                            running.batches += 1;
                            running.first_batch =
                                running.first_batch.or(Some(running.started.elapsed()));
                        }
                        QueryEvent::Done { .. } => finished = true,
                        QueryEvent::Error(_) => {
                            finished = true;
                            // A statement that failed stops the run: the
                            // ones after it were written to follow it.
                            running.rest.clear();
                        }
                        _ => {}
                    }
                    let last = finished;
                    app.apply(RuntimeEvent::Query { tab, event });
                    if last {
                        if trace.is_on() {
                            trace.event(
                                "results",
                                &[
                                    ("rows", &running.rows.to_string()),
                                    ("batches", &running.batches.to_string()),
                                    (
                                        "first_batch_ms",
                                        &format!(
                                            "{:.3}",
                                            running.first_batch.unwrap_or_default().as_secs_f64()
                                                * 1000.0
                                        ),
                                    ),
                                ],
                            );
                        }
                        if !running.rest.is_empty() {
                            let sql = running.rest.remove(0);
                            next = Some(Start {
                                sql,
                                rest: std::mem::take(&mut running.rest),
                                statement: running.statement + 1,
                                of: running.of,
                                keep_view: false,
                            });
                        }
                        break;
                    }
                }
            }
            if finished && let Some(runtime) = self.tabs.get_mut(tab) {
                runtime.running = None;
            }
            if let Some(start) = next {
                self.begin(app, tab, start);
            }
        }
        dirty
    }

    /// Fill a branch of the object tree, or fetch what `i` and `s` show.
    ///
    /// A catalog query is an ordinary query on the tab's own connection —
    /// the same channel, the same worker — so the UI waits for it exactly as
    /// little as it waits for anything else.
    pub fn load(&mut self, app: &mut App, tab: usize, request: CatalogRequest) {
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        if runtime.connection.is_none() {
            app.apply(RuntimeEvent::Catalog {
                tab,
                request,
                result: Err(db::model::DbError::Connect("not connected".to_owned())),
            });
            return;
        }
        app.catalog_started(tab, &request);
        if runtime.catalog.is_some() {
            runtime.waiting.push_back(request);
            return;
        }
        self.begin_load(tab, request);
    }

    /// Hand one catalog query to the driver.
    fn begin_load(&mut self, tab: usize, request: CatalogRequest) {
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        let Some(connection) = runtime.connection.as_ref() else {
            return;
        };
        let events = connection.query(
            &request.sql(connection.kind()),
            QueryOptions {
                batch_size: BATCH,
                // A listing is as long as it is; half a schema would be a
                // tree that quietly lies about what is in the database.
                max_rows: None,
            },
        );
        runtime.catalog = Some(Loading {
            request,
            events,
            rows: Vec::new(),
        });
    }

    /// Everything the catalog queries have reported since the last turn.
    pub fn poll_catalog(&mut self, app: &mut App) -> bool {
        let mut dirty = false;
        for tab in 0..self.tabs.len() {
            let Some(runtime) = self.tabs.get_mut(tab) else {
                continue;
            };
            let backend = runtime.connection.as_ref().map(Connection::kind);
            let mut answer = None;
            if let (Some(loading), Some(backend)) = (runtime.catalog.as_mut(), backend) {
                loop {
                    let event = match loading.events.try_recv() {
                        Ok(event) => event,
                        Err(TryRecvError::Empty) => break,
                        // Only a panicked worker ends the channel without
                        // saying why, and a row that says `…` for ever is
                        // worse than one that says what happened.
                        Err(TryRecvError::Disconnected) => {
                            QueryEvent::Error(db::model::DbError::Connect(
                                "the connection's worker has stopped".to_owned(),
                            ))
                        }
                    };
                    match event {
                        QueryEvent::Rows(batch) => loading.rows.extend(batch),
                        QueryEvent::Done { .. } => {
                            let rows = std::mem::take(&mut loading.rows);
                            answer = Some(loading.request.answer(backend, rows));
                            break;
                        }
                        QueryEvent::Error(error) => {
                            answer = Some(Err(error));
                            break;
                        }
                        _ => {}
                    }
                }
            }
            let Some(result) = answer else {
                continue;
            };
            dirty = true;
            let request = runtime.catalog.take().map(|loading| loading.request);
            let next = runtime.waiting.pop_front();
            if let Some(request) = request {
                app.apply(RuntimeEvent::Catalog {
                    tab,
                    request,
                    result,
                });
            }
            if let Some(next) = next {
                self.begin_load(tab, next);
            }
        }
        dirty
    }

    /// Esc: stop the query this tab is running. The driver answers with
    /// [`db::model::DbError::Cancelled`] through the channel it is already
    /// reporting on, so nothing else has to be unwound here.
    pub fn cancel(&mut self, tab: usize) {
        if let Some(runtime) = self.tabs.get(tab)
            && runtime.running.is_some()
            && let Some(connection) = runtime.connection.as_ref()
        {
            connection.cancel();
        }
    }

    /// `m`: the same statement again with [`MORE_ROWS`] more rows allowed.
    ///
    // ponytail: a re-run, not a server cursor — the decision `docs/DESIGN.md`
    // logs. The rows already on screen are fetched twice; a cursor per tab
    // held open across turns is what it would cost to fetch them once.
    pub fn more_rows(&mut self, app: &mut App, tab: usize) {
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        let Some(sql) = runtime.last.clone() else {
            return;
        };
        runtime.cap += MORE_ROWS;
        self.begin(
            app,
            tab,
            Start {
                sql,
                rest: Vec::new(),
                statement: 0,
                of: 1,
                keep_view: true,
            },
        );
    }

    /// Where the failure was, and what the driver said — never the password,
    /// which this module never sees a resolved copy of.
    fn message(&self, tab: usize, error: &db::model::DbError) -> String {
        self.config.connections.get(tab).map_or_else(
            || error.to_string(),
            |spec| format!("{}:{}: {error}", spec.host, spec.port),
        )
    }

    /// Remember what to run once this tab is connected, and say what the
    /// footer should tell the person. One slot only: a second request
    /// replaces the first.
    pub fn queue(&mut self, app: &mut App, tab: usize, request: Pending) {
        let Some(runtime) = self.tabs.get_mut(tab) else {
            return;
        };
        runtime.queued = Some(request);
        app.shell.status = "connecting… then running".to_owned();
    }

    /// This tab's connection, for the query E5 runs on it.
    #[must_use]
    pub fn connection(&self, tab: usize) -> Option<&Connection> {
        self.tabs.get(tab)?.connection.as_ref()
    }

    /// What this tab is waiting to run.
    #[must_use]
    pub fn queued(&self, tab: usize) -> Option<&Pending> {
        self.tabs.get(tab)?.queued.as_ref()
    }

    /// The cap this tab's next query runs with, which `m` has raised.
    #[must_use]
    pub fn cap(&self, tab: usize) -> Option<usize> {
        Some(self.tabs.get(tab)?.cap)
    }
}

/// The tabs `--connect NAME` and `--connect-all` ask for, in config order.
pub fn startup_tabs(config: &Config, args: &Cli) -> Result<Vec<usize>> {
    if args.connect_all {
        return Ok((0..config.connections.len()).collect());
    }
    args.connect
        .iter()
        .map(|name| {
            config
                .connections
                .iter()
                .position(|connection| connection.name == *name)
                .with_context(|| format!("--connect {name}: no such connection"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TabState;
    use crate::app::tests::two_connections;
    use crate::config::{Connection as Spec, Kind, Password};
    use std::time::Duration;

    fn refused(password: &str) -> Config {
        Config {
            connections: vec![Spec {
                name: "refused".to_owned(),
                kind: Kind::Mssql,
                host: "127.0.0.1".to_owned(),
                // Nothing listens on port 1, so the driver says no at once.
                port: 1,
                database: Some("bench".to_owned()),
                service: None,
                user: "sa".to_owned(),
                password: Some(Password::Literal(password.to_owned())),
                trust_cert: true,
                encrypt: true,
            }],
            ..Config::default()
        }
    }

    /// Poll until the tab is off `Connecting`, the way the loop does.
    fn settle(runtime: &mut Runtime, app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.busy() && Instant::now() < deadline {
            runtime.poll_connections(app);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_failure_says_where_it_was_and_never_what_the_password_is() {
        let config = refused("s3cret");
        let mut app = App::new(&config);
        let mut runtime = Runtime::new(&config);
        runtime.connect(&mut app, 0);
        assert_eq!(app.tabs[0].state, TabState::Connecting);
        settle(&mut runtime, &mut app);

        let TabState::Failed(message) = &app.tabs[0].state else {
            panic!("nothing listens on port 1: {:?}", app.tabs[0].state);
        };
        assert!(message.contains("127.0.0.1:1"), "{message}");
        assert!(!message.contains("s3cret"), "{message}");
        assert!(runtime.connection(0).is_none());
    }

    /// T4.2: `C` on a tab whose `password_cmd` is sleeping has to be the end
    /// of that attempt — including the answer it sends a second later, which
    /// would otherwise put a tab the person disconnected back on `Failed`.
    #[test]
    fn a_disconnect_abandons_an_attempt_and_the_late_answer_is_dropped() {
        let mut config = refused("s3cret");
        // Long enough to abandon the attempt while the thread is still in it.
        config.connections[0].password = Some(Password::Command("sleep 1".to_owned()));
        let mut app = App::new(&config);
        let mut runtime = Runtime::new(&config);
        runtime.connect(&mut app, 0);
        assert_eq!(app.tabs[0].state, TabState::Connecting);

        runtime.disconnect(&mut app, 0);
        assert_eq!(app.tabs[0].state, TabState::Disconnected);
        let deadline = Instant::now() + Duration::from_millis(1800);
        while Instant::now() < deadline {
            assert!(!runtime.poll_connections(&mut app), "a dropped attempt");
            assert_eq!(app.tabs[0].state, TabState::Disconnected);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn one_request_waits_for_a_connection_and_a_second_takes_its_place() {
        let config = two_connections();
        let mut app = App::new(&config);
        let mut runtime = Runtime::new(&config);
        runtime.queue(&mut app, 0, Pending::Statement("select 1".to_owned()));
        assert_eq!(
            runtime.queued(0),
            Some(&Pending::Statement("select 1".to_owned()))
        );
        assert_eq!(app.shell.status, "connecting… then running");

        runtime.queue(&mut app, 0, Pending::Statement("select 2".to_owned()));
        assert_eq!(
            runtime.queued(0),
            Some(&Pending::Statement("select 2".to_owned())),
            "one slot, and the newest request has it"
        );
        assert_eq!(runtime.queued(1), None, "one tab's queue is its own");

        runtime.disconnect(&mut app, 0);
        assert_eq!(runtime.queued(0), None, "a disconnect drops it");
    }

    #[test]
    fn a_disconnect_is_a_disconnected_tab_whether_or_not_it_was_connected() {
        let config = two_connections();
        let mut app = App::new(&config);
        let mut runtime = Runtime::new(&config);
        app.tabs[1].state = TabState::Connected;
        runtime.disconnect(&mut app, 1);
        assert_eq!(app.tabs[1].state, TabState::Disconnected);
        assert_eq!(app.shell.status, "○ local-oracle disconnected");
    }

    #[test]
    fn startup_flags_name_tabs_and_an_unknown_name_is_an_error() {
        use clap::Parser as _;
        let config = two_connections();
        let cli = |args: &[&str]| {
            Cli::parse_from(std::iter::once("sql-bench").chain(args.iter().copied()))
        };
        assert_eq!(startup_tabs(&config, &cli(&[])).expect("none"), Vec::new());
        assert_eq!(
            startup_tabs(&config, &cli(&["--connect", "local-oracle"])).expect("one"),
            [1]
        );
        assert_eq!(
            startup_tabs(&config, &cli(&["--connect-all"])).expect("all"),
            [0, 1]
        );
        let error = startup_tabs(&config, &cli(&["--connect", "nope"])).expect_err("no such one");
        assert!(format!("{error:#}").contains("--connect nope"), "{error:#}");
    }
}
