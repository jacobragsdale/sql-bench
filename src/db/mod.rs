//! The databases: the value model every backend reports in, and the
//! connection handle whose worker thread owns the driver so the event loop
//! never waits on a network.
//!
//! One handle, one thread, one driver connection. [`Connection::query`] hands
//! back a `Receiver` straight away and the worker fills it; [`Connection::cancel`]
//! sets a flag the backend looks at while it waits. How a backend answers is
//! its own business — tiberius throws the socket away because TDS gives it
//! nothing better, Oracle breaks the call OCI is inside — but either way the
//! connection is spent and the next query opens a new one.

pub mod catalog;
pub mod model;
mod mssql;
mod oracle;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::config::{self, Kind};
use crate::trace::Trace;
use model::{Cell, Column, DbError, QueryEvent, QueryOptions};

/// How often a backend that is waiting on a server looks at the cancel flag.
/// Cancel has to answer in well under a second and a poll this cheap is
/// invisible next to a network round trip.
const CANCEL_POLL_MS: u64 = 50;

/// An open database connection, run from a thread of its own.
///
/// Dropping it cancels whatever is running and joins the thread.
#[derive(Debug)]
pub struct Connection {
    name: String,
    kind: Kind,
    requests: Sender<Request>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// What the handle asks the worker for. Cancel is not in here: the worker is
/// inside a query when a cancel arrives and would not read the channel until
/// the query it is meant to stop has finished. A flag the backend polls is the
/// only thing that reaches it in time.
enum Request {
    Query {
        sql: String,
        options: QueryOptions,
        reply: Sender<QueryEvent>,
    },
    Close,
}

impl Connection {
    /// Opens a connection, blocking until the server answers or the driver's
    /// ten second timeout runs out.
    ///
    /// The whole configuration comes along because Oracle's client directory
    /// lives in it; SQL Server needs nothing from it.
    pub fn open(spec: &config::Connection, config: &config::Config) -> Result<Self, DbError> {
        // Resolved here rather than on the worker: a `password_cmd` should
        // fail against the caller, which is still allowed to print to a
        // terminal.
        let password = spec
            .password()
            .map_err(|why| DbError::Connect(format!("{why:#}")))?;
        let spec = spec.clone();
        let config = config.clone();
        let (name, kind) = (spec.name.clone(), spec.kind);
        Self::spawn(name, kind, Trace::from_env(), move || {
            Backend::open(&spec, password, &config)
        })
    }

    /// Runs `sql` and reports through the returned channel until
    /// [`QueryEvent::Done`] or [`QueryEvent::Error`]. Dropping the receiver
    /// stops the query at the next batch.
    #[must_use]
    pub fn query(&self, sql: &str, options: QueryOptions) -> Receiver<QueryEvent> {
        let (reply, events) = mpsc::channel();
        // Cleared here and not by the worker: the worker is still inside the
        // previous query when `cancel` is called, so only this side can put
        // the two in order.
        self.cancel.store(false, Ordering::SeqCst);
        let request = Request::Query {
            sql: sql.to_owned(),
            options,
            reply: reply.clone(),
        };
        if self.requests.send(request).is_err() {
            let _ = reply.send(QueryEvent::Error(DbError::Connect(
                "the connection's worker has stopped".to_owned(),
            )));
        }
        events
    }

    /// Stops the running query. Its receiver ends with
    /// [`DbError::Cancelled`]; the next query opens a new connection.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Starts the worker and waits for it to have a connection, so that a
    /// handle that exists is a handle that works.
    fn spawn(
        name: String,
        kind: Kind,
        trace: Trace,
        open: impl FnOnce() -> Result<Backend, DbError> + Send + 'static,
    ) -> Result<Self, DbError> {
        let (requests, inbox) = mpsc::channel();
        let (ready, opened) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let traced = name.clone();
        let worker = std::thread::Builder::new()
            .name(format!("db {name}"))
            .spawn(move || work(&traced, &trace, open, &inbox, &flag, &ready))
            .map_err(|why| DbError::Connect(format!("no worker thread: {why}")))?;
        match opened.recv() {
            Ok(Ok(())) => Ok(Self {
                name,
                kind,
                requests,
                cancel,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => Err(DbError::Connect(
                "the worker stopped before it connected".to_owned(),
            )),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Cancel first: a worker inside a long scan would not read `Close`
        // until the scan finished, and nobody is waiting for those rows.
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.requests.send(Request::Close);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The worker thread: open once, then serve requests until the handle goes.
///
/// It is also where the trace is written from, because it is the only place
/// that sees a whole connect and a whole query: one `connect` line and one
/// `query` line each, and no clock read at all when nothing is being traced.
fn work(
    name: &str,
    trace: &Trace,
    open: impl FnOnce() -> Result<Backend, DbError>,
    inbox: &Receiver<Request>,
    cancel: &Arc<AtomicBool>,
    ready: &Sender<Result<(), DbError>>,
) {
    let connecting = trace.is_on().then(Instant::now);
    let opened = open();
    if let Some(connecting) = connecting {
        trace.event(
            "connect",
            &[("conn", name), ("ms", &millis(connecting).to_string())],
        );
    }
    let mut backend = match opened {
        Ok(backend) => {
            let _ = ready.send(Ok(()));
            backend
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    while let Ok(request) = inbox.recv() {
        match request {
            Request::Close => break,
            Request::Query {
                sql,
                options,
                reply,
            } => {
                let mut sink = Sink::new(reply, Arc::clone(cancel), options);
                let timing = match backend.run(&sql, &mut sink) {
                    Ok(()) => sink.finish(),
                    Err(error) => sink.fail(error),
                };
                if trace.is_on() {
                    trace.event(
                        "query",
                        &[
                            ("conn", name),
                            ("rows", &timing.rows.to_string()),
                            ("truncated", if timing.truncated { "true" } else { "false" }),
                            ("connect_ms", &timing.connect_ms.to_string()),
                            ("first_row_ms", &timing.first_row_ms.to_string()),
                            ("total_ms", &timing.total_ms.to_string()),
                        ],
                    );
                }
            }
        }
    }
}

/// The drivers. An enum and not a trait: there are two of them, they are both
/// in this crate, and nobody is going to write a third.
// One of these exists per worker thread, so tiberius' kilobyte of config
// being bigger than an Oracle handle costs nothing.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Mssql(mssql::Backend),
    Oracle(oracle::Backend),
    #[cfg(test)]
    Fake(fake::Backend),
}

impl Backend {
    fn open(
        spec: &config::Connection,
        password: Option<String>,
        config: &config::Config,
    ) -> Result<Self, DbError> {
        match spec.kind {
            Kind::Mssql => Ok(Self::Mssql(mssql::Backend::open(spec, password)?)),
            Kind::Oracle => Ok(Self::Oracle(oracle::Backend::open(spec, password, config)?)),
        }
    }

    fn run(&mut self, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
        match self {
            Self::Mssql(backend) => backend.run(sql, sink),
            Self::Oracle(backend) => backend.run(sql, sink),
            #[cfg(test)]
            Self::Fake(backend) => backend.run(sql, sink),
        }
    }
}

/// What a backend reports through: it batches rows, counts them against the
/// cap, times the phases and knows when to give up. Every backend hands it
/// values and reads the [`Flow`] it gets back.
struct Sink {
    reply: Sender<QueryEvent>,
    cancel: Arc<AtomicBool>,
    options: QueryOptions,
    batch: Vec<Vec<Cell>>,
    rows: usize,
    truncated: bool,
    started: Instant,
    connect_ms: u32,
    first_row_ms: Option<u32>,
}

/// Whether the backend should keep reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

impl Sink {
    fn new(reply: Sender<QueryEvent>, cancel: Arc<AtomicBool>, options: QueryOptions) -> Self {
        Self {
            reply,
            cancel,
            options,
            batch: Vec::with_capacity(options.batch_size),
            rows: 0,
            truncated: false,
            started: Instant::now(),
            connect_ms: 0,
            first_row_ms: None,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// How many rows go in one batch, for a driver that can be told to fetch
    /// exactly that many per round trip.
    fn batch_size(&self) -> usize {
        self.options.batch_size
    }

    /// The flag itself, for a backend that wants to watch it while it waits.
    fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// True once the cap stopped a query that had more rows to give.
    fn truncated(&self) -> bool {
        self.truncated
    }

    /// How long this query spent opening a connection: zero when one was
    /// already there.
    fn connected(&mut self, connect_ms: u32) {
        self.connect_ms = connect_ms;
    }

    /// A new result set. Whatever is batched belongs to the old one.
    fn columns(&mut self, columns: Vec<Column>) -> Flow {
        if self.flush() == Flow::Stop {
            return Flow::Stop;
        }
        self.send(QueryEvent::Columns(columns))
    }

    fn row(&mut self, row: Vec<Cell>) -> Flow {
        // Checked before the row is kept, so `truncated` means the server had
        // one more row than the cap — a result set of exactly `max_rows` is
        // whole, and says so.
        if self.options.max_rows.is_some_and(|max| self.rows >= max) {
            self.truncated = true;
            self.flush();
            return Flow::Stop;
        }
        if self.first_row_ms.is_none() {
            self.first_row_ms = Some(millis(self.started));
        }
        self.batch.push(row);
        self.rows += 1;
        if self.batch.len() >= self.options.batch_size {
            self.flush()
        } else {
            Flow::Go
        }
    }

    fn rows_affected(&mut self, rows: u64) -> Flow {
        self.send(QueryEvent::RowsAffected(rows))
    }

    fn flush(&mut self) -> Flow {
        if self.batch.is_empty() {
            return Flow::Go;
        }
        let batch = std::mem::replace(&mut self.batch, Vec::with_capacity(self.options.batch_size));
        self.send(QueryEvent::Rows(batch))
    }

    fn send(&self, event: QueryEvent) -> Flow {
        // A dropped receiver is the UI having moved on, not an error.
        if self.reply.send(event).is_err() || self.cancelled() {
            Flow::Stop
        } else {
            Flow::Go
        }
    }

    fn finish(mut self) -> Timing {
        self.flush();
        let timing = self.timing();
        let _ = self.reply.send(QueryEvent::Done {
            rows: timing.rows,
            truncated: timing.truncated,
            connect_ms: timing.connect_ms,
            first_row_ms: timing.first_row_ms,
            total_ms: timing.total_ms,
        });
        timing
    }

    fn fail(self, error: DbError) -> Timing {
        let timing = self.timing();
        let _ = self.reply.send(QueryEvent::Error(error));
        timing
    }

    fn timing(&self) -> Timing {
        let total_ms = millis(self.started);
        Timing {
            rows: self.rows,
            truncated: self.truncated,
            connect_ms: self.connect_ms,
            // A query with no rows took as long as it took to answer at all.
            first_row_ms: self.first_row_ms.unwrap_or(total_ms),
            total_ms,
        }
    }
}

/// What one query cost: the numbers `QueryEvent::Done` carries, kept after
/// it has been sent so the worker can trace them.
struct Timing {
    rows: usize,
    truncated: bool,
    connect_ms: u32,
    first_row_ms: u32,
    total_ms: u32,
}

fn millis(since: Instant) -> u32 {
    u32::try_from(since.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// A backend that answers from a one word script, so the worker protocol, the
/// row cap and cancel can be tested without a server.
#[cfg(test)]
mod fake {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{Cell, Column, DbError, Flow, Sink};

    pub(super) struct Backend {
        /// Rows handed to the sink, so a test can see a query stop early.
        pub(super) emitted: Arc<AtomicUsize>,
    }

    impl Backend {
        pub(super) fn run(&mut self, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
            if sink.cancelled() {
                return Err(DbError::Cancelled);
            }
            sink.connected(1);
            let (word, argument) = sql.split_once(':').unwrap_or((sql, ""));
            let count: usize = argument.parse().unwrap_or(0);
            match word {
                "rows" => self.rows(sink, count),
                "sets" => self.sets(sink, count),
                "affected" => {
                    sink.rows_affected(count as u64);
                    Ok(())
                }
                "sleep" => self.sleep(sink),
                "boom" => Err(DbError::Query {
                    message: "fake: boom".to_owned(),
                    line: Some(1),
                }),
                other => Err(DbError::Unsupported(format!("fake script {other:?}"))),
            }
        }

        fn rows(&self, sink: &mut Sink, count: usize) -> Result<(), DbError> {
            if sink.columns(vec![Column {
                name: "n".to_owned(),
                type_name: "int".to_owned(),
            }]) == Flow::Stop
            {
                return Ok(());
            }
            for n in 0..count {
                self.emitted.fetch_add(1, Ordering::SeqCst);
                if sink.row(vec![Cell::Int(n as i64)]) == Flow::Stop {
                    return Ok(());
                }
            }
            Ok(())
        }

        fn sets(&self, sink: &mut Sink, count: usize) -> Result<(), DbError> {
            for set in 0..count {
                if sink.columns(vec![Column {
                    name: format!("set{set}"),
                    type_name: "int".to_owned(),
                }]) == Flow::Stop
                {
                    return Ok(());
                }
                self.emitted.fetch_add(1, Ordering::SeqCst);
                if sink.row(vec![Cell::Int(set as i64)]) == Flow::Stop {
                    return Ok(());
                }
            }
            Ok(())
        }

        /// A query the server is thinking about: it ends when cancelled, the
        /// way `waitfor delay` does.
        fn sleep(&self, sink: &mut Sink) -> Result<(), DbError> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if sink.cancelled() {
                    return Err(DbError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(super::CANCEL_POLL_MS));
            }
            Err(DbError::Timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use super::*;

    /// A connection whose server is the script in [`fake`], and the counter
    /// of rows that script has produced.
    fn fake() -> (Connection, Arc<AtomicUsize>) {
        traced(Trace::default())
    }

    fn traced(trace: Trace) -> (Connection, Arc<AtomicUsize>) {
        let emitted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&emitted);
        let connection = Connection::spawn("fake".to_owned(), Kind::Mssql, trace, move || {
            Ok(Backend::Fake(fake::Backend { emitted: counter }))
        })
        .expect("the fake backend always opens");
        (connection, emitted)
    }

    fn collect(events: &Receiver<QueryEvent>) -> Vec<QueryEvent> {
        events.iter().collect()
    }

    #[test]
    fn a_query_reports_columns_then_batches_then_done() {
        let (connection, _) = fake();
        let events = collect(&connection.query(
            "rows:5",
            QueryOptions {
                batch_size: 2,
                ..QueryOptions::default()
            },
        ));
        assert_eq!(
            events[0],
            QueryEvent::Columns(vec![Column {
                name: "n".to_owned(),
                type_name: "int".to_owned()
            }])
        );
        assert_eq!(
            events[1],
            QueryEvent::Rows(vec![vec![Cell::Int(0)], vec![Cell::Int(1)]])
        );
        assert_eq!(
            events[2],
            QueryEvent::Rows(vec![vec![Cell::Int(2)], vec![Cell::Int(3)]])
        );
        assert_eq!(events[3], QueryEvent::Rows(vec![vec![Cell::Int(4)]]));
        assert!(
            matches!(
                events[4],
                QueryEvent::Done {
                    rows: 5,
                    truncated: false,
                    ..
                }
            ),
            "{:?}",
            events[4]
        );
        assert_eq!(events.len(), 5, "the channel ends after Done");
    }

    #[test]
    fn the_row_cap_truncates_and_stops_reading() {
        let (connection, emitted) = fake();
        let events = collect(&connection.query(
            "rows:1000",
            QueryOptions {
                batch_size: 2,
                max_rows: Some(4),
            },
        ));
        let rows: usize = events
            .iter()
            .filter_map(|event| match event {
                QueryEvent::Rows(batch) => Some(batch.len()),
                _ => None,
            })
            .sum();
        assert_eq!(rows, 4);
        assert!(
            matches!(
                events.last(),
                Some(QueryEvent::Done {
                    rows: 4,
                    truncated: true,
                    ..
                })
            ),
            "{events:?}"
        );
        assert_eq!(
            emitted.load(Ordering::SeqCst),
            5,
            "the backend stops one row after the cap, which is how it knows there was more"
        );
    }

    #[test]
    fn a_result_set_that_is_exactly_the_cap_is_not_truncated() {
        let (connection, _) = fake();
        let events = collect(&connection.query(
            "rows:4",
            QueryOptions {
                batch_size: 500,
                max_rows: Some(4),
            },
        ));
        assert!(
            matches!(
                events.last(),
                Some(QueryEvent::Done {
                    rows: 4,
                    truncated: false,
                    ..
                })
            ),
            "{events:?}"
        );
    }

    #[test]
    fn a_statement_with_no_rows_reports_what_it_affected() {
        let (connection, _) = fake();
        let events = collect(&connection.query("affected:7", QueryOptions::default()));
        assert_eq!(events[0], QueryEvent::RowsAffected(7));
        assert!(
            matches!(
                events[1],
                QueryEvent::Done {
                    rows: 0,
                    truncated: false,
                    ..
                }
            ),
            "{:?}",
            events[1]
        );
    }

    #[test]
    fn every_result_set_gets_its_own_columns() {
        let (connection, _) = fake();
        let events = collect(&connection.query("sets:2", QueryOptions::default()));
        let sets: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                QueryEvent::Columns(columns) => Some(columns[0].name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sets, ["set0", "set1"]);
    }

    #[test]
    fn a_failure_ends_the_channel_without_a_done() {
        let (connection, _) = fake();
        let events = collect(&connection.query("boom", QueryOptions::default()));
        assert_eq!(
            events,
            [QueryEvent::Error(DbError::Query {
                message: "fake: boom".to_owned(),
                line: Some(1),
            })]
        );
    }

    #[test]
    fn cancel_ends_the_running_query_and_the_next_one_still_works() {
        let (connection, _) = fake();
        let events = connection.query("sleep", QueryOptions::default());
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        connection.cancel();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(QueryEvent::Error(DbError::Cancelled))
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );

        let after = collect(&connection.query("rows:1", QueryOptions::default()));
        assert!(
            matches!(after.last(), Some(QueryEvent::Done { rows: 1, .. })),
            "{after:?}"
        );
    }

    #[test]
    fn dropping_the_receiver_stops_the_query() {
        let (connection, emitted) = fake();
        drop(connection.query(
            "rows:1000000",
            QueryOptions {
                batch_size: 1,
                max_rows: None,
            },
        ));
        // The next query only runs once the first one has given up.
        let after = collect(&connection.query("rows:1", QueryOptions::default()));
        assert!(matches!(
            after.last(),
            Some(QueryEvent::Done { rows: 1, .. })
        ));
        assert!(
            emitted.load(Ordering::SeqCst) < 1_000_000,
            "{} rows were produced for a reader that had gone",
            emitted.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn a_traced_connection_writes_one_connect_line_and_one_line_per_query() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.tsv");
        let (connection, _) = traced(Trace::new(Some(path.clone())));
        collect(&connection.query(
            "rows:3",
            QueryOptions {
                batch_size: 500,
                max_rows: Some(2),
            },
        ));
        collect(&connection.query("boom", QueryOptions::default()));
        drop(connection);

        let written = std::fs::read_to_string(&path).unwrap();
        let kinds: Vec<&str> = written
            .lines()
            .map(|line| line.split('\t').nth(1).unwrap())
            .collect();
        assert_eq!(kinds, ["connect", "query", "query"], "{written}");
        let fields: Vec<&str> = written
            .lines()
            .nth(1)
            .unwrap()
            .split('\t')
            .skip(2)
            .collect();
        assert_eq!(
            fields[..4],
            ["conn=fake", "rows=2", "truncated=true", "connect_ms=1"]
        );
        assert!(
            fields[4].starts_with("first_row_ms=") && fields[5].starts_with("total_ms="),
            "{fields:?}"
        );
        let failed: Vec<&str> = written
            .lines()
            .nth(2)
            .unwrap()
            .split('\t')
            .skip(2)
            .collect();
        assert_eq!(failed[..3], ["conn=fake", "rows=0", "truncated=false"]);
    }

    #[test]
    fn a_handle_says_which_connection_it_is() {
        let (connection, _) = fake();
        assert_eq!(connection.name(), "fake");
        assert_eq!(connection.kind(), Kind::Mssql);
    }
}
