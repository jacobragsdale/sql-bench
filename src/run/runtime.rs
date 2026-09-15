//! The connections behind the tabs: one worker per connect attempt, one
//! `Option<db::Connection>` per tab, and a poll the loop takes each turn.
//!
//! Rule 1 in `CLAUDE.md` is that the app never blocks on a database, and
//! [`db::Connection::open`] blocks for up to ten seconds — so it is called on
//! a thread of its own, which also keeps a slow `password_cmd` off the loop.
//! The app is told what happened through [`RuntimeEvent`], never by this
//! module reaching into its state.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::app::{App, RuntimeEvent};
use crate::cli::Cli;
use crate::config::Config;
use crate::db::{self, Connection};

/// What a tab was asked to do while it was still connecting, kept until the
/// connection is up. E5 gives it a shape; the slot is what T4.1 owes it.
pub type Pending = String;

/// Every tab's connection, in the order the tabs are in.
#[derive(Debug, Default)]
pub struct Runtime {
    config: Config,
    tabs: Vec<TabRuntime>,
}

/// One tab's connection, the attempt that may be in flight, and the one
/// request waiting on it.
#[derive(Debug, Default)]
pub struct TabRuntime {
    connection: Option<Connection>,
    /// The connect thread's answer, until it arrives.
    pending: Option<Receiver<Result<Connection, db::model::DbError>>>,
    started: Option<Instant>,
    /// The query or browse that asked for this connection. One slot: a
    /// person who asks twice while a connection opens means the second one.
    queued: Option<Pending>,
}

impl Runtime {
    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            tabs: (0..config.connections.len())
                .map(|_| TabRuntime::default())
                .collect(),
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
            app.apply(event);
            dirty = true;
        }
        dirty
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
        runtime.queue(&mut app, 0, "select 1".to_owned());
        assert_eq!(runtime.queued(0), Some(&"select 1".to_owned()));
        assert_eq!(app.shell.status, "connecting… then running");

        runtime.queue(&mut app, 0, "select 2".to_owned());
        assert_eq!(
            runtime.queued(0),
            Some(&"select 2".to_owned()),
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
