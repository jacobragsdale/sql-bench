//! Oracle, over the `oracle` crate and the Instant Client it loads at runtime.
//!
//! The driver is synchronous, so the worker thread just calls it. Two things
//! are not like SQL Server. Cancel is real: OCI has a break, so a thread of
//! this file's own watches the flag and interrupts the fetch the worker is
//! stuck in, instead of throwing the socket away. And a statement is one
//! statement — Oracle has no batches, and the `;` every SQL editor puts on the
//! end is not part of one, so [`statement`] takes it off again for everything
//! that is not a PL/SQL block.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use oracle::sql_type::{Blob, Clob, Nclob, OracleType};
use oracle::{Connection as Driver, InitParams, SqlValue};

use super::model::{Cell, Column, DbError};
use super::{CANCEL_POLL_MS, Flow, Sink};
use crate::config;

/// Where to look for the Instant Client when the config does not say.
const CLIENT_DIR_ENV: &str = "SQL_BENCH_ORACLE_CLIENT_DIR";

/// How much of a LOB is worth having in a terminal. A CLOB column can hold
/// four gigabytes and a workbench has no business holding one because
/// somebody typed `select *`; the locator lets us stop reading here.
const LOB_LIMIT: usize = 1024 * 1024;

/// Long enough for a container still waking up, short enough that a wrong
/// host does not look like a hang. Easy Connect Plus carries it, because
/// ODPI-C has no connect timeout of its own.
const CONNECT_TIMEOUT_SECS: u32 = 10;

pub(super) struct Backend {
    user: String,
    password: String,
    connect_string: String,
    /// Shared with the cancel watcher, and `None` after a break: the next
    /// query opens a new session rather than trusting an interrupted one.
    driver: Option<Arc<Driver>>,
}

impl Backend {
    pub(super) fn open(
        spec: &config::Connection,
        password: Option<String>,
        config: &config::Config,
    ) -> Result<Self, DbError> {
        init(client_dir(config, std::env::var_os(CLIENT_DIR_ENV)))?;
        let mut backend = Self {
            user: spec.user.clone(),
            password: password.unwrap_or_default(),
            connect_string: format!(
                "//{}:{}/{}?connect_timeout={CONNECT_TIMEOUT_SECS}",
                spec.host,
                spec.port,
                spec.service.as_deref().unwrap_or_default(),
            ),
            driver: None,
        };
        backend.connect()?;
        Ok(backend)
    }

    pub(super) fn run(&mut self, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
        if sink.cancelled() {
            return Err(DbError::Cancelled);
        }
        let connecting = Instant::now();
        if self.driver.is_none() {
            self.connect()?;
        }
        sink.connected(super::millis(connecting));

        let driver = Arc::clone(self.driver.as_ref().expect("a connect leaves a driver"));
        let watch = Watch::start(&driver, sink.flag());
        let outcome = execute(&driver, sql, sink);
        if watch.stop() {
            // The break lands in the middle of a round trip, and where OCI
            // leaves the session afterwards is its business, not ours.
            self.driver = None;
            return Err(DbError::Cancelled);
        }
        // A server complaint is a good session; anything else is not.
        if !matches!(outcome, Ok(()) | Err(DbError::Query { .. })) {
            self.driver = None;
        }
        outcome
    }

    fn connect(&mut self) -> Result<(), DbError> {
        let driver = Driver::connect(&self.user, &self.password, &self.connect_string)
            .map_err(|why| DbError::Connect(complaint(&why)))?;
        self.driver = Some(Arc::new(driver));
        Ok(())
    }
}

/// The Instant Client directory: what the config says, else what the
/// environment says, else nothing — and then ODPI-C searches the usual places
/// (`LD_LIBRARY_PATH`, the run path, `$ORACLE_HOME`) the way the vendor
/// documents.
fn client_dir(config: &config::Config, env: Option<OsString>) -> Option<PathBuf> {
    config
        .oracle
        .client_lib_dir
        .clone()
        .or_else(|| env.filter(|dir| !dir.is_empty()).map(PathBuf::from))
}

/// ODPI-C loads the client library once per process, so the first connection
/// decides where it comes from and every later one lives with that — and with
/// its failure, which is why the answer is remembered either way.
fn init(dir: Option<PathBuf>) -> Result<(), DbError> {
    static CLIENT: OnceLock<Result<(), DbError>> = OnceLock::new();
    CLIENT.get_or_init(|| load(dir)).clone()
}

fn load(dir: Option<PathBuf>) -> Result<(), DbError> {
    let mut params = InitParams::new();
    if let Some(dir) = dir {
        params.oracle_client_lib_dir(dir).map_err(|_| no_client())?;
    }
    params.init().map_err(|_| no_client())?;
    Ok(())
}

/// ODPI-C's own complaint names a C header and a documentation URL, neither
/// of which tells anyone here what to do about it.
fn no_client() -> DbError {
    DbError::Connect(
        "Oracle client library not found; set [oracle] client_lib_dir or \
         SQL_BENCH_ORACLE_CLIENT_DIR (see README)"
            .to_owned(),
    )
}

/// The cancel flag turned into an OCI break. The worker thread is blocked
/// inside a fetch and cannot look at anything, so somebody else has to:
/// `oracle::Connection` is `Sync` and `break_execution` is exactly what OCI
/// offers for being called while a call is in flight. A running statement
/// comes back in single-digit milliseconds.
// ponytail: the ceiling is what a break can reach. OCI interrupts the call
// the server is *working on*, so a session asleep inside PL/SQL
// (`dbms_session.sleep`) runs its sleep out and only then sees the break —
// the cancel is reported late, not lost. Nothing short of killing the session
// from a second connection fixes that, and no ticket asks for it.
struct Watch {
    finished: Arc<AtomicBool>,
    thread: JoinHandle<bool>,
}

impl Watch {
    fn start(driver: &Arc<Driver>, cancel: Arc<AtomicBool>) -> Self {
        let finished = Arc::new(AtomicBool::new(false));
        let (driver, done) = (Arc::clone(driver), Arc::clone(&finished));
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    return driver.break_execution().is_ok();
                }
                // Parked rather than slept, so a query that finishes normally
                // pays the unpark and not the rest of the poll interval.
                std::thread::park_timeout(Duration::from_millis(CANCEL_POLL_MS));
            }
            false
        });
        Self { finished, thread }
    }

    /// True when it had to break the query.
    fn stop(self) -> bool {
        self.finished.store(true, Ordering::SeqCst);
        self.thread.thread().unpark();
        self.thread.join().unwrap_or(false)
    }
}

/// Runs one statement. Oracle has no batch and no second result set, so this
/// is a query with its rows or a statement with its count, and nothing else.
fn execute(driver: &Driver, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
    let sql = statement(sql);
    // The fetch array is the batch the sink wants, so one round trip fills one
    // `QueryEvent::Rows`. The locator keeps a `select *` over a table of
    // gigabyte CLOBs from being copied into this process whole.
    let size = u32::try_from(sink.batch_size()).unwrap_or(u32::MAX);
    let mut stmt = driver
        .statement(sql)
        .fetch_array_size(size)
        .prefetch_rows(size)
        .lob_locator()
        .build()
        .map_err(|why| failure(&why, sql))?;

    if !stmt.is_query() {
        stmt.execute(&[]).map_err(|why| failure(&why, sql))?;
        // OCI answers 1 for any PL/SQL block, meaning "one block ran", which
        // would read as one row changed. What a block changed is its own
        // business and the statement itself affected nothing.
        let affected = if stmt.is_plsql() {
            0
        } else {
            stmt.row_count().unwrap_or(0)
        };
        sink.rows_affected(affected);
        return Ok(());
    }

    let rows = stmt.query(&[]).map_err(|why| failure(&why, sql))?;
    let columns: Vec<Column> = rows.column_info().iter().map(column).collect();
    if sink.columns(columns) == Flow::Stop {
        return Ok(());
    }
    for row in rows {
        let row = row.map_err(|why| failure(&why, sql))?;
        let mut cells = Vec::with_capacity(row.sql_values().len());
        for (info, value) in row.column_info().iter().zip(row.sql_values()) {
            cells.push(cell(info.oracle_type(), value)?);
        }
        if sink.row(cells) == Flow::Stop {
            return Ok(());
        }
    }
    Ok(())
}

/// Oracle takes one statement and no terminator: a `;` after it is ORA-00933
/// on 19c and earlier, and 23ai only tolerates it. Editors write one anyway,
/// so one comes off — but not off a PL/SQL block, where `end;` is the
/// language and not punctuation, and losing it is PLS-00103.
fn statement(sql: &str) -> &str {
    let sql = sql.trim_end();
    if is_plsql(sql) {
        return sql;
    }
    sql.strip_suffix(';').map_or(sql, str::trim_end)
}

fn is_plsql(sql: &str) -> bool {
    let first = sql.split_whitespace().next().unwrap_or_default();
    first.eq_ignore_ascii_case("begin") || first.eq_ignore_ascii_case("declare")
}

fn column(info: &oracle::ColumnInfo) -> Column {
    Column {
        name: info.name().to_owned(),
        // `NUMBER(10)`, `TIMESTAMP(6) WITH TIME ZONE`: what the server calls
        // the column, spelled the way the server spells it.
        type_name: info.oracle_type().to_string(),
    }
}

/// One value, chosen by the column's declared type rather than by what the
/// driver happened to define it as — `NUMBER(10)` is fetched as a native
/// integer but it is still a `NUMBER` to everyone reading the grid.
fn cell(column_type: &OracleType, value: &SqlValue<'_>) -> Result<Cell, DbError> {
    if value.is_null().unwrap_or(true) {
        return Ok(Cell::Null);
    }
    let cell = match column_type {
        // An i64 holds 18 digits; wider than that, or with a scale, and the
        // exact text the server sent is the only lossless form.
        OracleType::Number(precision, 0) if (1..=18).contains(precision) => Cell::Int(get(value)?),
        OracleType::Int64 => Cell::Int(get(value)?),
        OracleType::Number(..) | OracleType::Float(_) => Cell::Decimal(get(value)?),
        OracleType::BinaryFloat | OracleType::BinaryDouble => Cell::Float(get(value)?),
        OracleType::Date
        | OracleType::Timestamp(_)
        | OracleType::TimestampTZ(_)
        | OracleType::TimestampLTZ(_) => Cell::DateTime(get(value)?),
        OracleType::Raw(_) | OracleType::LongRaw => Cell::Bytes(get(value)?),
        OracleType::CLOB => text(&mut get::<Clob>(value)?)?,
        OracleType::NCLOB => text(&mut get::<Nclob>(value)?)?,
        OracleType::BLOB => Cell::Bytes(lob(&mut get::<Blob>(value)?)?.0),
        // CHAR, VARCHAR2, NCHAR, NVARCHAR2, LONG, the intervals, ROWID, XML,
        // JSON: the driver's own text, which is the value as the server wrote
        // it rather than a Rust type's idea of it.
        _ => Cell::Text(get(value)?),
    };
    Ok(cell)
}

fn get<T: oracle::sql_type::FromSql>(value: &SqlValue<'_>) -> Result<T, DbError> {
    value.get().map_err(|why| DbError::Query {
        message: complaint(&why),
        line: None,
    })
}

fn text(reader: &mut impl std::io::Read) -> Result<Cell, DbError> {
    let (bytes, truncated) = lob(reader)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push('…');
    }
    Ok(Cell::Text(text))
}

/// At most [`LOB_LIMIT`] bytes of a locator, and whether there were more.
fn lob(reader: &mut impl std::io::Read) -> Result<(Vec<u8>, bool), DbError> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    while bytes.len() <= LOB_LIMIT {
        let read = reader.read(&mut chunk).map_err(|why| DbError::Query {
            message: why.to_string(),
            line: None,
        })?;
        if read == 0 {
            return Ok((bytes, false));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    // Cut on a character boundary so the lossy decode above does not invent a
    // replacement character at the very end of every truncated CLOB.
    while bytes.len() > LOB_LIMIT || std::str::from_utf8(&bytes).is_err() {
        bytes.pop();
    }
    Ok((bytes, true))
}

/// What the server said, on the line of the statement it said it about:
/// Oracle reports a character offset and the scratch pad shows lines.
fn failure(why: &oracle::Error, sql: &str) -> DbError {
    DbError::Query {
        message: complaint(why),
        line: why
            .db_error()
            .map(|complaint| line_of(sql, complaint.offset() as usize)),
    }
}

fn line_of(sql: &str, offset: usize) -> u32 {
    let lines = sql.chars().take(offset).filter(|c| *c == '\n').count() + 1;
    u32::try_from(lines).unwrap_or(1)
}

/// `ORA-00933: SQL command not properly ended` rather than the crate's
/// `OCI Error: ORA-00933: ...`, and without the newline OCI ends it with.
fn complaint(why: &oracle::Error) -> String {
    why.db_error()
        .map_or_else(|| why.to_string(), |db| db.message().trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn config(client_lib_dir: Option<&str>) -> config::Config {
        config::Config {
            oracle: config::Oracle {
                client_lib_dir: client_lib_dir.map(PathBuf::from),
            },
            connections: Vec::new(),
        }
    }

    #[test]
    fn the_config_names_the_client_before_the_environment_does() {
        assert_eq!(
            client_dir(&config(Some("/opt/ic")), Some("/elsewhere/ic".into())),
            Some(PathBuf::from("/opt/ic"))
        );
        assert_eq!(
            client_dir(&config(None), Some("/elsewhere/ic".into())),
            Some(PathBuf::from("/elsewhere/ic"))
        );
        assert_eq!(
            client_dir(&config(None), None),
            None,
            "nothing configured leaves ODPI-C to search the usual places"
        );
        assert_eq!(
            client_dir(&config(None), Some(OsString::new())),
            None,
            "an empty variable is not a directory"
        );
    }

    #[test]
    fn a_missing_client_says_what_to_set() {
        let empty = tempfile::tempdir().unwrap();
        let failure = load(Some(empty.path().to_path_buf())).unwrap_err();
        assert_eq!(failure, no_client());
        assert_eq!(
            failure.to_string(),
            "cannot connect: Oracle client library not found; set [oracle] \
             client_lib_dir or SQL_BENCH_ORACLE_CLIENT_DIR (see README)"
        );
    }

    #[test]
    fn a_trailing_semicolon_comes_off_a_statement() {
        assert_eq!(statement("select 1 from dual;"), "select 1 from dual");
        assert_eq!(statement("select 1 from dual"), "select 1 from dual");
        assert_eq!(statement("select 1 from dual ;\n\n"), "select 1 from dual");
        assert_eq!(
            statement("select ';' from dual;"),
            "select ';' from dual",
            "only the one on the end"
        );
        assert_eq!(
            statement("select 1 from dual;;"),
            "select 1 from dual;",
            "one, and only one"
        );
    }

    #[test]
    fn a_plsql_block_keeps_its_terminator() {
        assert_eq!(statement("begin null; end;"), "begin null; end;");
        assert_eq!(statement("BEGIN null; END;\n"), "BEGIN null; END;");
        assert_eq!(
            statement("declare n number; begin null; end;"),
            "declare n number; begin null; end;"
        );
        assert_eq!(
            statement("  Declare\n  n number;\nbegin null; end;"),
            "  Declare\n  n number;\nbegin null; end;"
        );
    }

    #[test]
    fn a_word_that_only_starts_with_begin_is_not_a_block() {
        assert!(!is_plsql("beginning_balance"));
        assert!(!is_plsql("select * from beginnings"));
        assert!(is_plsql("begin\nnull;\nend;"));
    }

    #[test]
    fn the_line_is_counted_from_the_offset_the_server_reports() {
        let sql = "select\n  nope\nfrom dual";
        assert_eq!(line_of(sql, 0), 1);
        assert_eq!(line_of(sql, 9), 2);
        assert_eq!(line_of(sql, 15), 3);
    }

    #[test]
    fn a_lob_stops_at_the_ceiling_and_says_so() {
        let short = b"short".to_vec();
        assert_eq!(lob(&mut short.as_slice()).unwrap(), (short.clone(), false));

        let exact = vec![b'x'; LOB_LIMIT];
        assert_eq!(lob(&mut exact.as_slice()).unwrap(), (exact, false));

        let long = vec![b'x'; LOB_LIMIT + 1];
        let (bytes, truncated) = lob(&mut long.as_slice()).unwrap();
        assert_eq!(bytes.len(), LOB_LIMIT);
        assert!(truncated);
    }

    #[test]
    fn a_truncated_clob_is_cut_between_characters_and_marked() {
        // A multi-byte character straddling the ceiling: the cut goes back to
        // the last whole one rather than leaving half of it behind.
        let mut source = vec![b'x'; LOB_LIMIT - 1];
        source.extend_from_slice("é".as_bytes());
        source.extend_from_slice(&[b'y'; 10]);
        let Cell::Text(text) = text(&mut source.as_slice()).unwrap() else {
            panic!("a CLOB is text");
        };
        assert!(text.ends_with('…'));
        assert_eq!(text.chars().count(), LOB_LIMIT - 1 + 1, "no replacements");
        assert_eq!(text.len(), LOB_LIMIT - 1 + '…'.len_utf8());
    }

    #[test]
    fn a_client_directory_that_is_a_tilde_is_the_config_modules_business() {
        // Left here on purpose: `config::load` expands `~`, so by the time the
        // backend sees a path it is one the operating system can open.
        assert_eq!(
            client_dir(&config(Some("~/ic")), None),
            Some(Path::new("~/ic").to_path_buf())
        );
    }
}
