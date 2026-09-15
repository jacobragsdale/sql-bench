//! SQL Server, over tiberius.
//!
//! Every future in sql-bench lives in this file: the worker thread owns a
//! current-thread runtime and blocks on it, so nothing above `db` has to know
//! the driver is async. Values are read out of `ColumnData` rather than
//! `try_get::<T>` — one match covers every type the server can send, including
//! the ones with no Rust equivalent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt as _;
use tiberius::error::Error as TiberiusError;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, EncryptionLevel, QueryItem};
use time::Date as CalendarDate;
use tokio::net::TcpStream;
use tokio::runtime::{Builder, Runtime};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};

use super::model::{Cell, Column, DbError};
use super::{CANCEL_POLL_MS, Flow, Sink};
use crate::config;

/// Long enough for a container that is still waking up, short enough that a
/// wrong host does not look like a hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Driver = Client<Compat<TcpStream>>;

pub(super) struct Backend {
    config: tiberius::Config,
    runtime: Runtime,
    /// `None` after a cancel or a truncated scan: the next query reconnects.
    client: Option<Driver>,
}

impl Backend {
    pub(super) fn open(
        spec: &config::Connection,
        password: Option<String>,
    ) -> Result<Self, DbError> {
        let mut config = tiberius::Config::new();
        config.host(&spec.host);
        config.port(spec.port);
        if let Some(database) = &spec.database {
            config.database(database);
        }
        config.authentication(AuthMethod::sql_server(
            &spec.user,
            password.unwrap_or_default(),
        ));
        config.encryption(if spec.encrypt {
            EncryptionLevel::Required
        } else {
            EncryptionLevel::NotSupported
        });
        if spec.trust_cert {
            config.trust_cert();
        }
        config.application_name("sql-bench");

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|why| DbError::Connect(format!("no driver runtime: {why}")))?;
        let mut backend = Self {
            config,
            runtime,
            client: None,
        };
        backend.connect()?;
        Ok(backend)
    }

    pub(super) fn run(&mut self, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
        if sink.cancelled() {
            return Err(DbError::Cancelled);
        }
        let connecting = Instant::now();
        if self.client.is_none() {
            self.connect()?;
        }
        sink.connected(super::millis(connecting));

        let Self {
            runtime, client, ..
        } = self;
        let driver = client.as_mut().expect("a connect leaves a client behind");
        let flag = sink.flag();
        let outcome = runtime.block_on(async {
            // A cancel has to reach a `waitfor delay` that will send nothing
            // for ten seconds, so the whole query races a watcher rather than
            // being polled between rows. Dropping the query half way through a
            // value is safe: the connection is dropped with it.
            let work = std::pin::pin!(stream(driver, sql, sink));
            let watch = std::pin::pin!(wanted_cancelled(&flag));
            match futures_util::future::select(work, watch).await {
                futures_util::future::Either::Left((outcome, _)) => outcome,
                futures_util::future::Either::Right(((), _)) => Err(DbError::Cancelled),
            }
        });

        // tiberius sends no attention packet, so the only way to stop a server
        // that is halfway through a million rows is to drop the socket — that
        // is what cancel does, and what a truncated scan wants too: draining
        // the rest would cost more than reconnecting. A server-side complaint
        // (a syntax error) leaves the connection perfectly good.
        if sink.truncated() || !matches!(outcome, Ok(()) | Err(DbError::Query { .. })) {
            self.client = None;
        }
        outcome
    }

    fn connect(&mut self) -> Result<(), DbError> {
        let Self {
            config,
            runtime,
            client,
        } = self;
        let opened = runtime.block_on(async {
            match tokio::time::timeout(CONNECT_TIMEOUT, open_driver(config)).await {
                Ok(result) => result,
                // A `Timeout` here would print as a bare "timed out" — and, on
                // the reconnect a truncated scan forces, would be read as the
                // *query* timing out. Saying which host stopped answering is
                // the same thing Oracle's own ORA-12170 says.
                Err(_elapsed) => Err(DbError::Connect(format!(
                    "no answer from {} within {}s",
                    config.get_addr(),
                    CONNECT_TIMEOUT.as_secs()
                ))),
            }
        })?;
        *client = Some(opened);
        Ok(())
    }
}

async fn open_driver(config: &tiberius::Config) -> Result<Driver, DbError> {
    let tcp = TcpStream::connect(config.get_addr())
        .await
        .map_err(|why| DbError::Connect(why.to_string()))?;
    // Row batches are small and frequent; Nagle would sit on them.
    tcp.set_nodelay(true)
        .map_err(|why| DbError::Connect(why.to_string()))?;
    Client::connect(config.clone(), tcp.compat_write())
        .await
        .map_err(|why| DbError::Connect(why.to_string()))
}

/// Runs one batch: `simple_query` because a scratch pad sends whole batches,
/// with several statements and several result sets.
async fn stream(driver: &mut Driver, sql: &str, sink: &mut Sink) -> Result<(), DbError> {
    let sets = {
        let mut query = driver.simple_query(sql).await.map_err(failure)?;
        let mut sets = 0usize;
        while let Some(item) = query.try_next().await.map_err(failure)? {
            let flow = match item {
                QueryItem::Metadata(metadata) => {
                    sets += 1;
                    sink.columns(metadata.columns().iter().map(column).collect())
                }
                QueryItem::Row(row) => sink.row(
                    row.cells()
                        .map(|(column, data)| cell(column.column_type(), data))
                        .collect(),
                ),
            };
            if flow == Flow::Stop {
                return Ok(());
            }
        }
        sets
    };

    // A batch with no result set at all is an UPDATE, a DELETE or an EXEC that
    // returned nothing. tiberius throws away the DONE token that carries the
    // count, so the session is asked instead: @@rowcount outlives the batch
    // boundary, and this round trip only happens when there were no rows.
    if sets == 0 {
        let affected = driver
            .simple_query("select @@rowcount")
            .await
            .map_err(failure)?
            .into_row()
            .await
            .map_err(failure)?
            .and_then(|row| row.get::<i32, _>(0))
            .unwrap_or_default();
        sink.rows_affected(affected.max(0).unsigned_abs().into());
    }
    Ok(())
}

/// Finishes once the handle has asked for a cancel.
async fn wanted_cancelled(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(CANCEL_POLL_MS)).await;
    }
}

fn column(column: &tiberius::Column) -> Column {
    Column {
        name: column.name().to_owned(),
        type_name: type_name(column.column_type()).to_owned(),
    }
}

/// What the server would call the column. tiberius reports the wire type,
/// which lumps every width of an integer into `Intn`; the row data says which
/// one it really was, so this is the name the catalog and the grid show.
fn type_name(column_type: ColumnType) -> &'static str {
    match column_type {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money | ColumnType::Money4 => "money",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigBinary => "binary",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::Text => "text",
        ColumnType::Image => "image",
        ColumnType::NText => "ntext",
        ColumnType::SSVariant => "sql_variant",
    }
}

/// One value. The column type only matters for money, which arrives as a
/// float because that is all the protocol says about it.
fn cell(column_type: ColumnType, data: &ColumnData<'static>) -> Cell {
    match data {
        ColumnData::Bit(Some(value)) => Cell::Bool(*value),
        ColumnData::U8(Some(value)) => Cell::Int(i64::from(*value)),
        ColumnData::I16(Some(value)) => Cell::Int(i64::from(*value)),
        ColumnData::I32(Some(value)) => Cell::Int(i64::from(*value)),
        ColumnData::I64(Some(value)) => Cell::Int(*value),
        ColumnData::F32(Some(value)) => Cell::Float(f64::from(*value)),
        ColumnData::F64(Some(value)) => match column_type {
            // money is a fixed four places, and rounding it into a float for
            // display is how money goes missing.
            ColumnType::Money | ColumnType::Money4 => Cell::Decimal(format!("{value:.4}")),
            _ => Cell::Float(*value),
        },
        ColumnData::Numeric(Some(value)) => Cell::Decimal(decimal(*value)),
        ColumnData::String(Some(value)) => Cell::Text(value.to_string()),
        ColumnData::Guid(Some(value)) => Cell::Text(value.to_string()),
        ColumnData::Xml(Some(value)) => Cell::Text(value.to_string()),
        ColumnData::Binary(Some(value)) => Cell::Bytes(value.to_vec()),
        ColumnData::Date(Some(value)) => Cell::DateTime(day(SQL_EPOCH, i64::from(value.days()))),
        ColumnData::Time(Some(value)) => Cell::DateTime(clock(value.increments(), value.scale())),
        ColumnData::DateTime(Some(value)) => Cell::DateTime(format!(
            "{}T{}",
            day(DATETIME_EPOCH, i64::from(value.days())),
            // The wire counts 1/300 of a second; SQL Server shows milliseconds,
            // rounded to the nearest of .000, .003 and .007.
            clock((u64::from(value.seconds_fragments()) * 10 + 1) / 3, 3)
        )),
        ColumnData::SmallDateTime(Some(value)) => Cell::DateTime(format!(
            "{}T{}",
            day(DATETIME_EPOCH, i64::from(value.days())),
            // smalldatetime counts whole minutes.
            clock(u64::from(value.seconds_fragments()) * 60, 0)
        )),
        ColumnData::DateTime2(Some(value)) => Cell::DateTime(format!(
            "{}T{}",
            day(SQL_EPOCH, i64::from(value.date().days())),
            clock(value.time().increments(), value.time().scale())
        )),
        ColumnData::DateTimeOffset(Some(value)) => Cell::DateTime(offset_datetime(*value)),
        // Every remaining arm is that variant's `None`.
        _ => Cell::Null,
    }
}

/// `date` and `datetime2` count from year one; `datetime` counts from 1900.
const SQL_EPOCH: CalendarDate = time::macros::date!(0001 - 01 - 01);
const DATETIME_EPOCH: CalendarDate = time::macros::date!(1900 - 01 - 01);

fn day(epoch: CalendarDate, days: i64) -> String {
    match epoch.checked_add(time::Duration::days(days)) {
        Some(date) => format!(
            "{:04}-{:02}-{:02}",
            date.year(),
            u8::from(date.month()),
            date.day()
        ),
        // Unreachable for anything SQL Server can store, and not worth a panic
        // inside a driver if some day it is.
        None => format!("<day {days}>"),
    }
}

/// `increments` is a count of 10^-`scale` seconds since midnight.
fn clock(increments: u64, scale: u8) -> String {
    let unit = 10u64.pow(u32::from(scale));
    let (seconds, fraction) = (increments / unit, increments % unit);
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    if scale == 0 {
        time
    } else {
        format!("{time}.{fraction:0width$}", width = usize::from(scale))
    }
}

/// The wire carries a `datetimeoffset` as UTC plus the offset it was written
/// with. Putting the offset back is reading the value as stored, not choosing
/// a timezone for it.
fn offset_datetime(value: tiberius::time::DateTimeOffset) -> String {
    let time = value.datetime2().time();
    let scale = time.scale();
    let unit = i128::from(10u64.pow(u32::from(scale)));
    let per_day = 86_400 * unit;
    let local = i128::from(time.increments()) + i128::from(value.offset()) * 60 * unit;
    let days = i64::from(value.datetime2().date().days()) + (local.div_euclid(per_day) as i64);
    let increments = local.rem_euclid(per_day) as u64;
    let minutes = value.offset().abs();
    format!(
        "{}T{}{}{:02}:{:02}",
        day(SQL_EPOCH, days),
        clock(increments, scale),
        if value.offset() < 0 { '-' } else { '+' },
        minutes / 60,
        minutes % 60
    )
}

/// tiberius' own `Display` writes the integer and decimal parts separately,
/// which prints `-1234.-5678` for a negative value.
fn decimal(value: tiberius::numeric::Numeric) -> String {
    let scale = usize::from(value.scale());
    if scale == 0 {
        return value.value().to_string();
    }
    let unit = 10u128.pow(value.scale().into());
    let magnitude = value.value().unsigned_abs();
    format!(
        "{}{}.{:0scale$}",
        if value.value() < 0 { "-" } else { "" },
        magnitude / unit,
        magnitude % unit,
    )
}

/// The severity at which SQL Server closes the connection after saying what
/// went wrong, rather than carrying on. Books Online: 20 and up is fatal.
const FATAL_CLASS: u8 = 20;

fn failure(why: TiberiusError) -> DbError {
    match why {
        // The session did not come through this one, so it is a `Connect` and
        // not a complaint: `run` throws the client away for everything that is
        // not a complaint, and a dead client would answer nothing for ever.
        TiberiusError::Server(token) if token.class() >= FATAL_CLASS => {
            DbError::Connect(token.message().to_owned())
        }
        TiberiusError::Server(token) => DbError::Query {
            message: token.message().to_owned(),
            line: Some(token.line()),
        },
        // Not the server saying no but the socket, the handshake or the
        // protocol going wrong — a database stopped under a running query
        // arrives here — and none of those leave anything to run on.
        why @ (TiberiusError::Io { .. } | TiberiusError::Protocol(_) | TiberiusError::Tls(_)) => {
            DbError::Connect(why.to_string())
        }
        other => DbError::Query {
            message: other.to_string(),
            line: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use tiberius::numeric::Numeric;
    use tiberius::time::{Date, DateTime, DateTime2, DateTimeOffset, Time};

    use super::*;

    #[test]
    fn a_date_counts_from_year_one() {
        assert_eq!(day(SQL_EPOCH, 0), "0001-01-01");
        assert_eq!(
            cell(
                ColumnType::Daten,
                &ColumnData::Date(Some(Date::new(739_022)))
            ),
            Cell::DateTime("2024-05-17".to_owned())
        );
    }

    #[test]
    fn a_datetime_counts_from_1900_in_three_hundredths() {
        assert_eq!(
            cell(
                ColumnType::Datetimen,
                &ColumnData::DateTime(Some(DateTime::new(45_427, 14_859_000)))
            ),
            Cell::DateTime("2024-05-17T13:45:30.000".to_owned())
        );
    }

    #[test]
    fn a_datetime2_keeps_every_digit_of_its_scale() {
        let value = DateTime2::new(Date::new(739_022), Time::new(495_301_234_567, 7));
        assert_eq!(
            cell(ColumnType::Datetime2, &ColumnData::DateTime2(Some(value))),
            Cell::DateTime("2024-05-17T13:45:30.1234567".to_owned())
        );
    }

    #[test]
    fn an_offset_is_put_back_on_the_utc_the_wire_carries() {
        // 13:45:30.1234567 +02:00, which the server sends as 11:45:30 UTC.
        let utc = DateTime2::new(Date::new(739_022), Time::new(423_301_234_567, 7));
        let value = DateTimeOffset::new(utc, 120);
        assert_eq!(
            cell(
                ColumnType::DatetimeOffsetn,
                &ColumnData::DateTimeOffset(Some(value))
            ),
            Cell::DateTime("2024-05-17T13:45:30.1234567+02:00".to_owned())
        );
        // An offset that carries the value back over midnight.
        let midnight = DateTime2::new(Date::new(739_022), Time::new(0, 0));
        assert_eq!(
            cell(
                ColumnType::DatetimeOffsetn,
                &ColumnData::DateTimeOffset(Some(DateTimeOffset::new(midnight, -300)))
            ),
            Cell::DateTime("2024-05-16T19:00:00-05:00".to_owned())
        );
    }

    #[test]
    fn a_decimal_keeps_its_scale_and_its_sign() {
        assert_eq!(
            decimal(Numeric::new_with_scale(123_456_789, 4)),
            "12345.6789"
        );
        assert_eq!(
            decimal(Numeric::new_with_scale(-123_456_789, 4)),
            "-12345.6789"
        );
        assert_eq!(decimal(Numeric::new_with_scale(-5, 1)), "-0.5");
        assert_eq!(decimal(Numeric::new_with_scale(42, 0)), "42");
    }

    #[test]
    fn money_is_a_decimal_of_four_places_and_a_float_is_not() {
        assert_eq!(
            cell(ColumnType::Money, &ColumnData::F64(Some(1234.5678))),
            Cell::Decimal("1234.5678".to_owned())
        );
        assert_eq!(
            cell(ColumnType::Float8, &ColumnData::F64(Some(1234.5678))),
            Cell::Float(1234.5678)
        );
    }

    #[test]
    fn a_null_of_any_type_is_null() {
        assert_eq!(cell(ColumnType::Intn, &ColumnData::I32(None)), Cell::Null);
        assert_eq!(cell(ColumnType::Bitn, &ColumnData::Bit(None)), Cell::Null);
        assert_eq!(
            cell(ColumnType::BigVarChar, &ColumnData::String(None)),
            Cell::Null
        );
        assert_eq!(
            cell(ColumnType::Datetime2, &ColumnData::DateTime2(None)),
            Cell::Null
        );
    }
}
