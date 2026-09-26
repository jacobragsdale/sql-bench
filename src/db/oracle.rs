//! Oracle, over the `oracle` crate and the Instant Client it loads at runtime.
//!
//! The driver is synchronous, so the worker thread just calls it. Two things
//! are not like SQL Server. Cancel is real: OCI has a break, so a thread of
//! this file's own watches the flag and interrupts the fetch the worker is
//! stuck in, instead of throwing the socket away. And a statement is one
//! statement — Oracle has no batches, and the `;` every SQL editor puts on the
//! end is not part of one, so [`statement`] takes it off again for everything
//! that is not PL/SQL. And every statement commits, the way SQL Server's do:
//! a workbench whose `insert` quietly rolls back on the next reconnect or
//! cancel is worse than one with no manual transactions.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use oracle::io::SeekInChars;
use oracle::oci_attr::DefaultLobPrefetchSize;
use oracle::sql_type::{Blob, Clob, Lob, Nclob, OracleType};
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

/// How much of each LOB rides along with its row, in the LOB's own unit
/// (characters for a CLOB). A LOB this small costs no round trip of its own:
/// its size and its data both come from the fetch. Oracle reserves this much
/// per row of the fetch array, so it stays small.
const LOB_PREFETCH: u32 = 8 * 1024;

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
                // `::1` would read as a host and two ports; brackets are how
                // Easy Connect takes an IPv6 address.
                if spec.host.contains(':') && !spec.host.starts_with('[') {
                    format!("[{}]", spec.host)
                } else {
                    spec.host.clone()
                },
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

        // `select … for update` locks the rows it reads, and the commit an
        // autocommit execute makes at once would end the cursor they are
        // read through: ORA-01002 past the first batch. So it commits once
        // it has been read, which lets the locks go all the same.
        let locking = locks_rows(sql);
        if locking && let Some(driver) = self.driver.as_mut().and_then(Arc::get_mut) {
            driver.set_autocommit(false);
        }
        let driver = Arc::clone(self.driver.as_ref().expect("a connect leaves a driver"));
        let watch = Watch::start(&driver, sink.flag());
        let mut outcome = execute(&driver, sql, sink);
        if locking {
            outcome = outcome.and_then(|()| driver.commit().map_err(|why| failure(&why, sql)));
        }
        if watch.stop() {
            // The break lands in the middle of a round trip, and where OCI
            // leaves the session afterwards is its business, not ours.
            self.driver = None;
            return Err(DbError::Cancelled);
        }
        // A server complaint is usually a good session — but not when the
        // complaint *is* the session ending: a killed session and a stopped
        // database both arrive as an ORA the same shape as a typo's
        // (ORA-00028, ORA-01089, DPI-1080). Asking the driver costs one round
        // trip on a path that already failed, and is the only answer that
        // does not need a list of codes nobody can finish.
        let good = match &outcome {
            Ok(()) => true,
            Err(DbError::Query { .. }) => driver.ping().is_ok(),
            Err(_) => false,
        };
        drop(driver);
        if !good {
            self.driver = None;
            // Then the complaint was the session ending, not the statement's:
            // no line of it is to blame.
            if let Err(DbError::Query { message, .. }) = outcome {
                outcome = Err(DbError::Lost(message));
            }
        } else if locking && let Some(driver) = self.driver.as_mut().and_then(Arc::get_mut) {
            // A failed one leaves its locks to this rollback, not to the
            // next statement's commit.
            if outcome.is_err() {
                let _ = driver.rollback();
            }
            driver.set_autocommit(true);
        }
        outcome
    }

    /// Drop the session, mid-answer or not; the next query connects again.
    pub(super) fn reset(&mut self) {
        self.driver = None;
    }

    fn connect(&mut self) -> Result<(), DbError> {
        let mut driver = Driver::connect(&self.user, &self.password, &self.connect_string)
            .map_err(|why| DbError::Connect(complaint(&why)))?;
        driver.set_autocommit(true);
        // Only speed rides on this: a client that refuses it still reads
        // every LOB, a round trip or two slower.
        let _ = driver.set_oci_attr::<DefaultLobPrefetchSize>(&LOB_PREFETCH);
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
    let looked = dir.as_ref().map(|dir| dir.display().to_string());
    if let Some(dir) = dir {
        params
            .oracle_client_lib_dir(dir)
            .map_err(|_| no_client(looked.as_deref()))?;
    }
    params.init().map_err(|_| no_client(looked.as_deref()))?;
    Ok(())
}

/// ODPI-C's own complaint names a C header and a documentation URL, neither
/// of which tells anyone here what to do about it.
fn no_client(looked: Option<&str>) -> DbError {
    DbError::Connect(match looked {
        Some(dir) => format!(
            "Oracle client library not found in {dir}; set [oracle] client_lib_dir or \
             SQL_BENCH_ORACLE_CLIENT_DIR to where it is (see README)"
        ),
        None => "Oracle client library not found; set [oracle] client_lib_dir or \
                 SQL_BENCH_ORACLE_CLIENT_DIR (see README)"
            .to_owned(),
    })
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
        // A stored program that does not compile is still created, INVALID,
        // and OCI calls that success with a warning. It is not success to
        // anyone who just typed it.
        if driver.last_warning().is_some()
            && let Some(error) = compile_error(driver, sql)
        {
            return Err(error);
        }
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

/// Whether a query takes locks as it reads: `for update`, whatever follows.
fn locks_rows(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    words
        .windows(2)
        .any(|pair| pair[0] == "for" && pair[1].starts_with("update"))
}

/// Oracle takes one statement and no terminator: a `;` after it is ORA-00933
/// on 19c and earlier, and 23ai only tolerates it. Editors write one anyway,
/// so one comes off — but not off PL/SQL, a block or a `CREATE` of a stored
/// program, where `end;` is the language and not punctuation, and losing it
/// is PLS-00103.
fn statement(sql: &str) -> &str {
    let sql = sql.trim_end();
    if is_plsql(sql) {
        return sql;
    }
    sql.strip_suffix(';').map_or(sql, str::trim_end)
}

fn is_plsql(sql: &str) -> bool {
    let first = super::code_start(sql)
        .split_whitespace()
        .next()
        .unwrap_or_default();
    first.eq_ignore_ascii_case("begin")
        || first.eq_ignore_ascii_case("declare")
        || created(sql).is_some()
}

/// What a `CREATE` of stored PL/SQL makes: its type as `ALL_ERRORS` spells
/// it, where in `sql` that type's keyword starts, and the word that names it.
fn created(sql: &str) -> Option<(&'static str, usize, &str)> {
    let mut words = super::code_start(sql).split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("create") {
        return None;
    }
    let mut word = words.next()?;
    if word.eq_ignore_ascii_case("or") {
        words.next()?; // replace
        word = words.next()?;
    }
    if word.eq_ignore_ascii_case("editionable") || word.eq_ignore_ascii_case("noneditionable") {
        word = words.next()?;
    }
    let (kind, body) = match word.to_ascii_lowercase().as_str() {
        "procedure" => ("PROCEDURE", ""),
        "function" => ("FUNCTION", ""),
        "trigger" => ("TRIGGER", ""),
        "package" => ("PACKAGE", "PACKAGE BODY"),
        "type" => ("TYPE", "TYPE BODY"),
        _ => return None,
    };
    // A slice of `sql`, so its address is its offset.
    let at = word.as_ptr() as usize - sql.as_ptr() as usize;
    let name = words.next()?;
    if !body.is_empty() && name.eq_ignore_ascii_case("body") {
        return Some((body, at, words.next()?));
    }
    Some((kind, at, name))
}

/// The first error `ALL_ERRORS` holds for the program `sql` just created, on
/// the line of `sql` it is on. `None` when `sql` created no such program.
fn compile_error(driver: &Driver, sql: &str) -> Option<DbError> {
    let (kind, at, word) = created(sql)?;
    // `bench.p(a number)`, `"Bench"."P"`: unquoted parts fold to upper case
    // the way Oracle stored them.
    let name = word.split('(').next().unwrap_or(word);
    let mut parts = name.split('.').map(|part| match part.strip_prefix('"') {
        Some(quoted) => quoted.trim_end_matches('"').to_owned(),
        None => part.to_uppercase(),
    });
    let (owner, name) = match (parts.next(), parts.next()) {
        (Some(owner), Some(name)) => (Some(owner), name),
        (Some(name), None) => (None, name),
        _ => return None,
    };
    // Unqualified is the session's schema. Not a bound NULL and `coalesce`:
    // a NULL bind is NVARCHAR2 to OCI and `sys_context` is not (ORA-12704).
    let owner = owner.map_or_else(
        || "sys_context('USERENV', 'CURRENT_SCHEMA')".to_owned(),
        |owner| format!("'{}'", owner.replace('\'', "''")),
    );
    let errors = driver
        .query_as::<(u32, String)>(
            &format!(
                "select line, text from all_errors \
                 where owner = {owner} and name = :1 and type = :2 \
                   and attribute = 'ERROR' \
                 order by sequence"
            ),
            &[&name, &kind],
        )
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let (line, text) = errors.first()?;
    // `ALL_ERRORS` counts from the line the type keyword is on, which is
    // where the stored source starts.
    let before = u32::try_from(sql[..at].matches('\n').count()).unwrap_or(0);
    let more = match errors.len() {
        1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    Some(DbError::Query {
        message: format!("{}{more}", text.trim_end()),
        line: Some(line + before),
    })
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
        OracleType::BinaryFloat => Cell::Float(super::widen(get(value)?)),
        // 23ai's BOOLEAN, which reads the way SQL Server's `bit` does.
        OracleType::Boolean => Cell::Bool(get(value)?),
        OracleType::BinaryDouble => Cell::Float(get(value)?),
        OracleType::Date
        | OracleType::Timestamp(_)
        | OracleType::TimestampTZ(_)
        | OracleType::TimestampLTZ(_) => Cell::DateTime(year_of_four(get(value)?)),
        OracleType::Raw(_) | OracleType::LongRaw => Cell::Bytes(get(value)?),
        OracleType::CLOB => {
            let mut clob = get::<Clob>(value)?;
            let size = size(&clob)?;
            text(lob(
                &mut clob,
                size,
                |clob| clob.seek_in_chars(SeekFrom::Current(0)),
                Some(|clob| clob.seek_in_chars(SeekFrom::Current(2))),
            )?)
        }
        OracleType::NCLOB => {
            let mut nclob = get::<Nclob>(value)?;
            let size = size(&nclob)?;
            text(lob(
                &mut nclob,
                size,
                |nclob| nclob.seek_in_chars(SeekFrom::Current(0)),
                Some(|nclob| nclob.seek_in_chars(SeekFrom::Current(2))),
            )?)
        }
        OracleType::BLOB => {
            let mut blob = get::<Blob>(value)?;
            let size = size(&blob)?;
            Cell::Bytes(lob(&mut blob, size, Seek::stream_position, None)?.0)
        }
        // CHAR, VARCHAR2, NCHAR, NVARCHAR2, LONG, the intervals, ROWID, XML,
        // JSON: the driver's own text, which is the value as the server wrote
        // it rather than a Rust type's idea of it.
        _ => Cell::Text(get(value)?),
    };
    Ok(cell)
}

/// The driver writes the year 5 as `5-03-04`: four digits, the way SQL
/// Server writes it and the way a date sorts as text.
fn year_of_four(date: String) -> String {
    let (sign, rest) = date
        .strip_prefix('-')
        .map_or(("", date.as_str()), |rest| ("-", rest));
    match rest.find('-') {
        Some(digits) if digits < 4 => format!("{sign}{}{rest}", "0".repeat(4 - digits)),
        _ => date,
    }
}

fn get<T: oracle::sql_type::FromSql>(value: &SqlValue<'_>) -> Result<T, DbError> {
    value.get().map_err(|why| DbError::Query {
        message: complaint(&why),
        line: None,
    })
}

fn text((mut bytes, truncated): (Vec<u8>, bool)) -> Cell {
    if truncated {
        // Cut on a character boundary so the lossy decode below does not
        // invent a replacement character at the end of every long CLOB.
        while std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push('…');
    }
    Cell::Text(text)
}

/// At most [`LOB_LIMIT`] bytes of a locator, and whether there were more.
/// `at` is how far in the reader is, in the locator's own unit (characters
/// for a CLOB, which is also what its size counts): stopping at the size
/// saves the empty read that would otherwise find the end, a round trip per
/// cell. A short read is no sign of the end, since a CLOB read stops at
/// 16,384 characters whatever the buffer.
///
/// `past_pair`, a CLOB's, steps over the one character no read can get back.
/// The prefetch that brought the first [`LOB_PREFETCH`] characters with the
/// row ends on the first half of an emoji when one straddles its end, and
/// every read that touches that half fails with ORA-22831. So the first read
/// stops short of it — a CLOB shorter than that never notices — and when the
/// next one fails that way the character reads `�` rather than the whole
/// result being lost to it.
fn lob<R: Read>(
    locator: &mut R,
    size: u64,
    at: impl Fn(&mut R) -> std::io::Result<u64>,
    past_pair: Option<fn(&mut R) -> std::io::Result<u64>>,
) -> Result<(Vec<u8>, bool), DbError> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    // A character is at most four bytes, so this holds every one before
    // the prefetch's last.
    let mut room = match past_pair {
        Some(_) => 4 * (LOB_PREFETCH as usize - 1),
        None => chunk.len(),
    };
    let mut stepped = false;
    while bytes.len() <= LOB_LIMIT && at(locator).map_err(broken)? < size {
        match locator.read(&mut chunk[..room]) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(why) if !stepped && code(&why) == Some(22831) && past_pair.is_some() => {
                bytes.extend_from_slice("\u{fffd}".as_bytes());
                past_pair
                    .map_or(Ok(0), |step| step(locator))
                    .map_err(broken)?;
                stepped = true;
            }
            Err(why) => return Err(broken(why)),
        }
        room = chunk.len();
    }
    let truncated = bytes.len() > LOB_LIMIT;
    bytes.truncate(LOB_LIMIT);
    Ok((bytes, truncated))
}

/// The ORA- number inside what a LOB read failed with.
fn code(why: &std::io::Error) -> Option<i32> {
    let inner = why.get_ref()?.downcast_ref::<oracle::Error>()?;
    inner.db_error().map(oracle::DbError::code)
}

/// A LOB read that failed, said the way every other failure is: without
/// `OCI Error:` in front or the documentation link behind.
fn broken(why: std::io::Error) -> DbError {
    DbError::Query {
        message: match why
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<oracle::Error>())
        {
            Some(inner) => complaint(inner),
            None => why.to_string(),
        },
        line: None,
    }
}

/// A LOB's length, which the prefetch brought along with the row.
fn size(locator: &impl Lob) -> Result<u64, DbError> {
    locator.size().map_err(|why| DbError::Query {
        message: complaint(&why),
        line: None,
    })
}

/// What the server said, on the line of the statement it said it about:
/// Oracle reports where parsing stopped and the scratch pad shows lines.
/// An error with no such place — a block that raised, a divide by zero —
/// has offset 0, and saying `line 1` for it would point at the wrong line;
/// PL/SQL's own `ORA-06512: at line 3` is in the message instead.
fn failure(why: &oracle::Error, sql: &str) -> DbError {
    DbError::Query {
        message: complaint(why),
        line: why
            .db_error()
            .map(|complaint| complaint.offset() as usize)
            .filter(|offset| *offset > 0)
            .map(|offset| line_of(sql, offset)),
    }
}

/// ODPI-C counts the offset in bytes, so a `é` before the error is two.
fn line_of(sql: &str, offset: usize) -> u32 {
    let before = sql.as_bytes().get(..offset).unwrap_or(sql.as_bytes());
    let lines = before.iter().filter(|byte| **byte == b'\n').count() + 1;
    u32::try_from(lines).unwrap_or(1)
}

/// `ORA-00933: SQL command not properly ended` rather than the crate's
/// `OCI Error: ORA-00933: ...`, and without the newline OCI ends it with.
fn complaint(why: &oracle::Error) -> String {
    match why.db_error() {
        // A string the driver fetches XMLTYPE into holds 4,000 bytes.
        Some(db) if db.code() == 19011 => format!(
            "{} An XMLTYPE past 4,000 bytes reads as xmlserialize(document … as clob).",
            unhelped(db.message())
        ),
        Some(db) => unhelped(db.message()),
        None => unreadable(why.to_string()),
    }
}

/// A column of a type the driver cannot fetch fails the whole query, so
/// what to select instead is the half of the message worth reading.
fn unreadable(message: String) -> String {
    let (what, instead) = if message.ends_with("Oracle type JSON") {
        ("JSON", "json_serialize(… returning clob)")
    } else if message.ends_with("Oracle type number 2033") {
        ("VECTOR", "vector_serialize(… returning clob)")
    } else if message.starts_with("unknown Oracle type number") {
        ("REF or other", "reftohex(…) for a REF, or any cast to text")
    } else {
        return message;
    };
    format!("a {what} column the driver cannot read ({message}): select {instead} instead")
}

/// 23ai ends every message with `Help: https://docs.oracle.com/…` on a line
/// of its own, which a footer has no room for and a person has a search
/// engine for.
fn unhelped(message: &str) -> String {
    message
        .lines()
        .filter(|line| !line.starts_with("Help: http"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
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
            path: PathBuf::new(),
        }
    }

    #[test]
    fn a_year_before_1000_is_four_digits() {
        assert_eq!(
            year_of_four("5-03-04 00:00:00".to_owned()),
            "0005-03-04 00:00:00"
        );
        assert_eq!(
            year_of_four("-5-01-01 00:00:00".to_owned()),
            "-0005-01-01 00:00:00"
        );
        assert_eq!(
            year_of_four("2026-09-25 12:00:00".to_owned()),
            "2026-09-25 12:00:00"
        );
        assert_eq!(
            year_of_four("-4712-01-01 00:00:00".to_owned()),
            "-4712-01-01 00:00:00"
        );
    }

    /// The driver's own words for a JSON or VECTOR column were all a query
    /// that selected one got, and not what to select instead.
    #[test]
    fn a_column_the_driver_cannot_read_says_what_to_select_instead() {
        assert_eq!(
            unreadable("unsupported Oracle type JSON".to_owned()),
            "a JSON column the driver cannot read (unsupported Oracle type JSON): \
             select json_serialize(… returning clob) instead"
        );
        assert!(
            unreadable("unknown Oracle type number 2033".to_owned()).contains("vector_serialize")
        );
        assert_eq!(unreadable("ORA-1".to_owned()), "ORA-1");
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
        assert_eq!(
            failure.to_string(),
            format!(
                "cannot connect: Oracle client library not found in {}; set [oracle] \
                 client_lib_dir or SQL_BENCH_ORACLE_CLIENT_DIR to where it is (see README)",
                empty.path().display()
            ),
            "and where it was looked for"
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
    fn a_stored_program_keeps_its_terminator_and_a_table_does_not() {
        for sql in [
            "create or replace procedure p is begin null; end;",
            "CREATE FUNCTION f return number is begin return 1; end;",
            "create or replace editionable package body bench.pk as end;",
            "create trigger t before insert on x begin null; end;",
            "create or replace type body tb as end;",
        ] {
            assert_eq!(statement(sql), sql);
        }
        assert_eq!(
            statement("create table zz (id number);"),
            "create table zz (id number)"
        );
        assert_eq!(
            statement("create or replace view v as select 1 a from dual;"),
            "create or replace view v as select 1 a from dual"
        );
    }

    #[test]
    fn a_create_says_what_it_made_and_where_the_keyword_is() {
        let sql = "create or replace\n  package body bench.pk as end;";
        assert_eq!(created(sql), Some(("PACKAGE BODY", 20, "bench.pk")));
        assert_eq!(
            created("create procedure p(a number) is begin null; end;"),
            Some(("PROCEDURE", 7, "p(a"))
        );
        assert_eq!(created("create package"), None, "no name");
        assert_eq!(created("select 1 from dual"), None);
    }

    #[test]
    fn the_help_link_23ai_appends_is_left_behind() {
        assert_eq!(
            unhelped(
                "ORA-01476: divisor is equal to zero\n\
                 Help: https://docs.oracle.com/error-help/db/ora-01476/"
            ),
            "ORA-01476: divisor is equal to zero"
        );
        assert_eq!(
            unhelped("ORA-06550: line 1, column 7:\nPLS-00201: identifier 'X' must be declared\n"),
            "ORA-06550: line 1, column 7:\nPLS-00201: identifier 'X' must be declared",
            "the lines that are the message stay"
        );
    }

    #[test]
    fn a_note_in_front_of_a_block_or_a_program_keeps_its_terminator() {
        for sql in [
            "-- run it\nbegin\n  null;\nend;",
            "/* why */ declare n number; begin null; end;",
            "-- mine\n-- really\ncreate or replace procedure p is begin null; end;",
        ] {
            assert_eq!(statement(sql), sql);
        }
        assert_eq!(
            statement("-- all of them\nselect 1 from dual;"),
            "-- all of them\nselect 1 from dual"
        );
        // The keyword's place is counted in the text as sent, note and all,
        // which is what puts a compile error on the line it is on.
        let sql = "-- mine\ncreate procedure p is begin nope; end;";
        assert_eq!(created(sql), Some(("PROCEDURE", 15, "p")));
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
        // Bytes, not characters: the error is on line 2 after all of them.
        let wide = "select '李李李李李李' a,\n  nope b,\n  1 c\nfrom dual";
        assert_eq!(line_of(wide, wide.find("nope").unwrap()), 2);
        assert_eq!(line_of(wide, 10_000), 4, "past the end is the last line");
    }

    /// A LOB read the way a locator is: up to the size it reports.
    fn read(data: &[u8]) -> (Vec<u8>, bool) {
        let size = data.len() as u64;
        lob(
            &mut std::io::Cursor::new(data),
            size,
            Seek::stream_position,
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_lob_is_read_to_its_size_and_no_further() {
        // The reader has more than the locator's size: only an empty read
        // could have found that end, and the size saves making it.
        let mut reader = std::io::Cursor::new(b"abcdef".to_vec());
        let read = lob(&mut reader, 3, |_| Ok(3), None).unwrap();
        assert_eq!(read, (Vec::new(), false));
        assert_eq!(reader.position(), 0, "nothing read past the size");
    }

    /// A CLOB whose prefetch ended on the first half of a pair, the way OCI
    /// has it: a read from before the half stops short of it, and one that
    /// starts on it fails with ORA-22831.
    struct Straddle {
        data: Vec<u8>,
        at: usize,
        half: usize,
    }

    impl Read for Straddle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.at == self.half {
                #[allow(deprecated)]
                let why = oracle::Error::OciError(oracle::DbError::new(
                    22831,
                    0,
                    "ORA-22831: Offset or offset+amount does not land on character boundary",
                    "",
                    "",
                ));
                return Err(std::io::Error::other(why));
            }
            let end = if self.at < self.half {
                self.half
            } else {
                self.data.len()
            };
            let read = buf.len().min(end - self.at);
            buf[..read].copy_from_slice(&self.data[self.at..self.at + read]);
            self.at += read;
            Ok(read)
        }
    }

    #[test]
    fn an_emoji_the_prefetch_cut_in_half_is_one_replacement_and_not_a_lost_result() {
        let mut clob = Straddle {
            data: b"aaaaXXtail".to_vec(),
            at: 0,
            half: 4,
        };
        let (bytes, truncated) = lob(
            &mut clob,
            10,
            |clob| Ok(clob.at as u64),
            Some(|clob| {
                clob.at += 2;
                Ok(clob.at as u64)
            }),
        )
        .expect("the rest of it");
        assert_eq!(String::from_utf8(bytes).unwrap(), "aaaa\u{fffd}tail");
        assert!(!truncated);

        // A BLOB has no characters to step over: the failure is the answer.
        let mut blob = Straddle {
            data: b"aaaaXXtail".to_vec(),
            at: 0,
            half: 4,
        };
        let failed = lob(&mut blob, 10, |blob| Ok(blob.at as u64), None).unwrap_err();
        assert_eq!(
            failed.to_string(),
            "ORA-22831: Offset or offset+amount does not land on character boundary"
        );
    }

    #[test]
    fn a_lob_stops_at_the_ceiling_and_says_so() {
        let short = b"short".to_vec();
        assert_eq!(read(&short), (short.clone(), false));

        let exact = vec![b'x'; LOB_LIMIT];
        assert_eq!(read(&exact), (exact, false));

        let long = vec![b'x'; LOB_LIMIT + 1];
        let (bytes, truncated) = read(&long);
        assert_eq!(bytes.len(), LOB_LIMIT);
        assert!(truncated);
    }

    #[test]
    fn a_long_blob_keeps_its_first_mebibyte_whatever_the_bytes_are() {
        // 0xff is never UTF-8: a character-boundary cut would eat all of it.
        let long = vec![0xff; LOB_LIMIT * 2];
        let (bytes, truncated) = read(&long);
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
        let Cell::Text(text) = text(read(&source)) else {
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
