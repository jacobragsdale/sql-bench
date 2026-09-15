//! The command line: the flags every run takes, and the subcommands that do
//! one thing without opening the TUI.
//!
//! A bare invocation opens the TUI; each subcommand is how the same feature
//! is verified headlessly, which is rule 3 in `CLAUDE.md`. Nothing here
//! formats a row or writes a catalog query — [`crate::export`] and
//! [`crate::db::catalog`] do that, so the TUI reaches the same code — this
//! file only turns flags into calls and failures into exit codes.
//!
//! Exit codes: 0 is a run that did what it was asked, 1 is a database or a
//! configuration saying no (and then stdout is empty, so a pipeline never
//! reads half an answer as a whole one), 2 is a verb this build knows and
//! cannot do yet.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum, ValueHint};

use crate::config::Config;
use crate::db::catalog::{self, ObjectKind};
use crate::db::model::{Cell, Column, DbError, QueryEvent, QueryOptions};
use crate::db::{self, Connection};
use crate::export;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Config file to read instead of $SQL_BENCH_CONFIG or
    /// ~/.config/sql-bench/config.toml
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub config: Option<PathBuf>,
    /// Drive the real loop with the keys in this file instead of the
    /// keyboard, and exit when they run out
    #[arg(long, global = true, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub replay: Option<PathBuf>,
    /// Terminal size a replay pretends to have, like 120x40
    #[arg(long, global = true, value_name = "COLSxROWS")]
    pub size: Option<Size>,
    /// Directory a replay writes one text frame per redraw into
    #[arg(long, global = true, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub frames_dir: Option<PathBuf>,
    /// Write <name>.styles.txt beside every frame: the colours, run by run
    #[arg(long, global = true)]
    pub frame_styles: bool,
    /// Connect this connection once the first frame is up; repeatable
    #[arg(long, global = true, value_name = "NAME")]
    pub connect: Vec<String>,
    /// Connect every connection in the config at startup
    #[arg(long, global = true)]
    pub connect_all: bool,
    /// Stop a query in the TUI after this many rows; `m` in the grid asks
    /// for ten thousand more. Not global: `query` and `bench` have their own
    #[arg(long, value_name = "N", default_value_t = 10_000)]
    pub max_rows: usize,
    /// Panic this many milliseconds into the loop, so QA can check the
    /// terminal is given back. Debug builds only.
    #[cfg(debug_assertions)]
    #[arg(long, global = true, value_name = "MS", hide = true)]
    pub panic_after_ms: Option<u64>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// One thing to do, and then exit.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Run SQL on a connection and print the rows
    Query {
        /// Connection name from config.toml
        #[arg(long, value_name = "NAME")]
        conn: String,
        /// How to print the rows
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
        /// Stop after this many rows
        #[arg(long, value_name = "N", default_value_t = 10_000)]
        max_rows: usize,
        /// Give up and cancel after this many seconds
        #[arg(long, value_name = "SECONDS", default_value_t = 30)]
        timeout: u64,
        /// Print whole cells instead of cutting them at 60 characters
        #[arg(long)]
        full: bool,
        /// The statement to run, or `-` to read it from stdin
        #[arg(value_name = "SQL")]
        sql: String,
    },
    /// List the tables, views and procedures of a connection
    Objects {
        /// Connection name from config.toml
        #[arg(long, value_name = "NAME")]
        conn: String,
        /// Only this schema
        #[arg(long, value_name = "SCHEMA")]
        schema: Option<String>,
        /// Only this kind: table, view, procedure, function, package, sequence
        #[arg(long, value_name = "KIND")]
        kind: Option<ObjectKind>,
        /// Only objects whose name contains this
        #[arg(value_name = "PATTERN")]
        pattern: Option<String>,
    },
    /// Print the source of a procedure, view or function
    Source {
        /// Connection name from config.toml
        #[arg(long, value_name = "NAME")]
        conn: String,
        /// Object to print, schema-qualified
        #[arg(value_name = "OBJECT")]
        object: String,
    },
    /// Time a statement over several runs
    Bench {
        /// Connection name from config.toml
        #[arg(long, value_name = "NAME")]
        conn: String,
        /// How many times to run it
        #[arg(long, value_name = "N", default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        runs: u32,
        /// Stop after this many rows
        #[arg(long, value_name = "M")]
        max_rows: Option<usize>,
        /// The statement to time, or `-` to read it from stdin
        #[arg(value_name = "SQL")]
        sql: String,
    },
}

impl Command {
    /// What to call this one in a message.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Query { .. } => "query",
            Self::Objects { .. } => "objects",
            Self::Source { .. } => "source",
            Self::Bench { .. } => "bench",
        }
    }
}

/// How `query` writes a result set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Format {
    #[default]
    Table,
    Csv,
    Json,
}

/// A terminal size, as `--size` writes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl FromStr for Size {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const SHAPE: &str = "expected COLSxROWS, like 120x40";
        let (cols, rows) = text.split_once('x').ok_or(SHAPE)?;
        let number = |part: &str| part.parse::<u16>().ok().filter(|n| *n > 0).ok_or(SHAPE);
        Ok(Self {
            cols: number(cols)?,
            rows: number(rows)?,
        })
    }
}

/// Runs the subcommand. The [`Err`] side is for a run that could not be
/// attempted at all — unreadable stdin, say; a database that answered with a
/// complaint has already said so on stderr and is an exit code, not an
/// `anyhow` chain.
pub fn run(cli: &Cli, config: &Config) -> Result<ExitCode> {
    match cli.command.as_ref() {
        Some(Command::Query {
            conn,
            format,
            max_rows,
            timeout,
            full,
            sql,
        }) => query(config, conn, sql, *format, *max_rows, *timeout, *full),
        Some(Command::Objects {
            conn,
            schema,
            kind,
            pattern,
        }) => Ok(objects(
            config,
            conn,
            schema.as_deref(),
            *kind,
            pattern.as_deref(),
        )),
        Some(Command::Source { conn, object }) => Ok(source(config, conn, object)),
        Some(Command::Bench {
            conn,
            runs,
            max_rows,
            sql,
        }) => bench(config, conn, sql, *runs, *max_rows),
        None => Ok(ExitCode::SUCCESS),
    }
}

fn query(
    config: &Config,
    conn: &str,
    sql: &str,
    format: Format,
    max_rows: usize,
    timeout: u64,
    full: bool,
) -> Result<ExitCode> {
    let sql = statement(sql)?;
    let Some(connection) = open(config, conn) else {
        return Ok(ExitCode::FAILURE);
    };
    let options = QueryOptions {
        max_rows: Some(max_rows),
        ..QueryOptions::default()
    };
    let answer = match collect(&connection, &sql, options, Duration::from_secs(timeout)) {
        Ok(answer) => answer,
        Err(DbError::Timeout) => {
            eprintln!("query timed out after {timeout}s");
            return Ok(ExitCode::FAILURE);
        }
        Err(error) => {
            eprintln!("{error}");
            return Ok(ExitCode::FAILURE);
        }
    };

    let limit = (!full).then_some(export::CELL_LIMIT);
    match format {
        Format::Table => print_sets(&answer.sets, |set| export::table(&set.0, &set.1, limit)),
        Format::Csv => print_sets(&answer.sets, |set| export::csv(&set.0, &set.1)),
        // Two result sets are two arrays; one is the array everything else
        // expects, so a `select` piped to a parser is never wrapped.
        Format::Json if answer.sets.len() > 1 => {
            let sets: Vec<String> = answer
                .sets
                .iter()
                .map(|set| export::json(&set.0, &set.1).trim_end().to_owned())
                .collect();
            print!("[\n{}\n]\n", sets.join(",\n"));
        }
        Format::Json => print!(
            "{}",
            answer
                .sets
                .first()
                .map_or_else(|| "[]\n".to_owned(), |set| export::json(&set.0, &set.1))
        ),
    }

    for affected in &answer.affected {
        eprintln!("{affected} rows affected");
    }
    let cap = if answer.truncated {
        format!(" (truncated at {max_rows})")
    } else {
        String::new()
    };
    eprintln!("{} rows{cap} in {} ms", answer.rows, answer.total_ms);
    Ok(ExitCode::SUCCESS)
}

/// Long enough for the slowest thing anyone benches on purpose — a million
/// row scan is budgeted at eight seconds — and short enough that a wedged
/// server is not a wedged afternoon.
const BENCH_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs the statement `runs` times on one connection and prints what each
/// phase cost. Sequential and through the ordinary [`Connection`], so what is
/// measured is what the TUI does, warm cache and all.
fn bench(
    config: &Config,
    conn: &str,
    sql: &str,
    runs: u32,
    max_rows: Option<usize>,
) -> Result<ExitCode> {
    let sql = statement(sql)?;
    let connecting = Instant::now();
    let Some(connection) = open(config, conn) else {
        return Ok(ExitCode::FAILURE);
    };
    // The one connect there is: every later run reuses this connection, which
    // is why the phase has a single sample.
    let mut connect = vec![millis(connecting)];
    let options = QueryOptions {
        max_rows,
        ..QueryOptions::default()
    };
    let mut first_row = Vec::new();
    let mut total = Vec::new();
    let mut rows = 0;
    for _ in 0..runs {
        match collect(&connection, &sql, options, BENCH_TIMEOUT) {
            Ok(answer) => {
                rows = answer.rows;
                first_row.push(answer.first_row_ms);
                total.push(answer.total_ms);
            }
            Err(error) => {
                eprintln!("{error}");
                return Ok(ExitCode::FAILURE);
            }
        }
    }

    let phases = vec![
        phase("connect", &mut connect),
        phase("first_row", &mut first_row),
        phase("total", &mut total),
    ];
    print!(
        "{}",
        export::table(
            &headings(&["phase", "min", "p50", "p95", "max"]),
            &phases,
            None
        )
    );
    // Over the whole run rather than off the median, so a query too fast to
    // register a millisecond still divides by something.
    let elapsed: u64 = total.iter().map(|ms| u64::from(*ms)).sum::<u64>().max(1);
    let per_second = rows as u64 * u64::from(runs) * 1000 / elapsed;
    println!("{rows} rows, {per_second} rows/s over {runs} runs");
    Ok(ExitCode::SUCCESS)
}

/// One row of the phase table. `samples` is sorted in place, which is what
/// the percentiles want anyway.
fn phase(name: &str, samples: &mut [u32]) -> Vec<Cell> {
    samples.sort_unstable();
    let at = |p| i64::from(percentile(samples, p));
    vec![
        Cell::Text(name.to_owned()),
        Cell::Int(at(0)),
        Cell::Int(at(50)),
        Cell::Int(at(95)),
        Cell::Int(at(100)),
    ]
}

/// Nearest-rank: the sample at `ceil(p/100 * n)`, counting from one. No
/// interpolation, no floats, and at twenty runs the answer is a number that
/// was actually measured.
///
/// Panics on no samples, which `--runs` will not allow.
fn percentile(sorted: &[u32], p: u32) -> u32 {
    let rank = (p as usize * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn millis(since: Instant) -> u32 {
    u32::try_from(since.elapsed().as_millis()).unwrap_or(u32::MAX)
}

fn print_sets(sets: &[ResultSet], format: impl Fn(&ResultSet) -> String) {
    for (index, set) in sets.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print!("{}", format(set));
    }
}

fn objects(
    config: &Config,
    conn: &str,
    schema: Option<&str>,
    kind: Option<ObjectKind>,
    pattern: Option<&str>,
) -> ExitCode {
    let Some(connection) = open(config, conn) else {
        return ExitCode::FAILURE;
    };
    let objects = match catalog::list_objects(&connection, schema, kind) {
        Ok(objects) => objects,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let pattern = pattern.map(str::to_lowercase);
    let rows: Vec<Vec<Cell>> = objects
        .iter()
        .filter(|object| {
            pattern
                .as_ref()
                .is_none_or(|pattern| object.name.to_lowercase().contains(pattern))
        })
        .map(|object| {
            vec![
                Cell::Text(object.schema.clone()),
                Cell::Text(object.kind.to_string()),
                Cell::Text(object.name.clone()),
                object.modified.clone().map_or(Cell::Null, Cell::Text),
            ]
        })
        .collect();
    print!(
        "{}",
        export::table(
            &headings(&["schema", "kind", "name", "modified"]),
            &rows,
            None
        )
    );
    ExitCode::SUCCESS
}

fn source(config: &Config, conn: &str, object: &str) -> ExitCode {
    let Some((schema, name)) = object.split_once('.') else {
        eprintln!("expected SCHEMA.NAME, got {object:?}");
        return ExitCode::FAILURE;
    };
    let Some(connection) = open(config, conn) else {
        return ExitCode::FAILURE;
    };
    let found = catalog::list_objects(&connection, Some(schema), None).map(|objects| {
        // Exact first: on a case-sensitive collation two objects can differ
        // by nothing else, and the one that was asked for is the one meant.
        objects
            .iter()
            .find(|object| object.name == name)
            .or_else(|| {
                objects
                    .iter()
                    .find(|object| object.name.eq_ignore_ascii_case(name))
            })
            .cloned()
    });
    let object = match found {
        Ok(Some(object)) => object,
        Ok(None) => {
            eprintln!("no such object: {schema}.{name}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    // A table has no source; its columns are what there is to know about it.
    let printed = if object.kind == ObjectKind::Table {
        catalog::list_columns(&connection, &object.schema, &object.name).map(|columns| {
            let rows: Vec<Vec<Cell>> = columns
                .iter()
                .map(|column| {
                    vec![
                        Cell::Text(column.name.clone()),
                        Cell::Text(column.type_text.clone()),
                        Cell::Text(yes(column.nullable)),
                        Cell::Text(yes(column.is_pk)),
                    ]
                })
                .collect();
            export::table(&headings(&["column", "type", "null", "pk"]), &rows, None)
        })
    } else {
        catalog::object_source(&connection, &object.schema, &object.name, object.kind)
            .map(|source| format!("{}\n", source.trim_end()))
    };
    match printed {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn yes(flag: bool) -> String {
    if flag { "yes" } else { "no" }.to_owned()
}

fn headings(names: &[&str]) -> Vec<Column> {
    names
        .iter()
        .map(|name| Column {
            name: (*name).to_owned(),
            type_name: String::new(),
        })
        .collect()
}

/// The connection `--conn` names, opened. [`None`] means it has already been
/// explained on stderr.
fn open(config: &Config, name: &str) -> Option<Connection> {
    let Some(spec) = config.connection(name) else {
        let configured: Vec<&str> = config
            .connections
            .iter()
            .map(|connection| connection.name.as_str())
            .collect();
        eprintln!(
            "unknown connection '{name}'; configured: {}",
            configured.join(", ")
        );
        return None;
    };
    match db::Connection::open(spec, config) {
        Ok(connection) => Some(connection),
        Err(error) => {
            eprintln!("{error}");
            None
        }
    }
}

/// `-` is the statement on stdin, which is how a file or a heredoc gets in.
fn statement(sql: &str) -> Result<String> {
    if sql != "-" {
        return Ok(sql.to_owned());
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the statement from stdin")?;
    Ok(text)
}

/// One result set: its columns and every row of it.
type ResultSet = (Vec<Column>, Vec<Vec<Cell>>);

/// Everything one statement said.
struct Answer {
    sets: Vec<ResultSet>,
    affected: Vec<u64>,
    rows: usize,
    truncated: bool,
    first_row_ms: u32,
    total_ms: u32,
}

/// Runs the statement and waits for it, cancelling if it takes longer than
/// `timeout`. Held in memory rather than streamed: the table format needs
/// every row before it knows how wide a column is, and `--max-rows` is what
/// keeps that honest.
fn collect(
    connection: &Connection,
    sql: &str,
    options: QueryOptions,
    timeout: Duration,
) -> Result<Answer, DbError> {
    let events = connection.query(sql, options);
    let deadline = Instant::now() + timeout;
    let mut answer = Answer {
        sets: Vec::new(),
        affected: Vec::new(),
        rows: 0,
        truncated: false,
        first_row_ms: 0,
        total_ms: 0,
    };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(QueryEvent::Columns(columns)) => answer.sets.push((columns, Vec::new())),
            Ok(QueryEvent::Rows(batch)) => match answer.sets.last_mut() {
                Some(set) => set.1.extend(batch),
                None => answer.sets.push((Vec::new(), batch)),
            },
            Ok(QueryEvent::RowsAffected(rows)) => answer.affected.push(rows),
            Ok(QueryEvent::Done {
                rows,
                truncated,
                first_row_ms,
                total_ms,
                ..
            }) => {
                answer.rows = rows;
                answer.truncated = truncated;
                answer.first_row_ms = first_row_ms;
                answer.total_ms = total_ms;
                return Ok(answer);
            }
            Ok(QueryEvent::Error(error)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => {
                // The handle's own cancel, so the worker stops talking to a
                // server nobody is waiting for any more.
                connection.cancel();
                return Err(DbError::Timeout);
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(DbError::Connect(
                    "the connection's worker has stopped".to_owned(),
                ));
            }
        }
    }
}

/// Exit 2 is "this build knows that verb but cannot do it yet", as opposed
/// to 1 for a run that genuinely failed: a script can tell the two apart.
pub fn not_implemented(what: &str) -> ! {
    eprintln!("sql-bench: {what} is not implemented yet");
    std::process::exit(2)
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_query_takes_a_connection_and_a_statement() {
        let cli = Cli::parse_from(["sql-bench", "query", "--conn", "x", "select 1"]);
        let Some(Command::Query {
            conn,
            sql,
            format,
            max_rows,
            timeout,
            full,
        }) = cli.command
        else {
            panic!("expected a query, got {:?}", cli.command);
        };
        assert_eq!(conn, "x");
        assert_eq!(sql, "select 1");
        assert_eq!(format, Format::Table);
        assert_eq!(max_rows, 10_000);
        assert_eq!(timeout, 30);
        assert!(!full);
    }

    #[test]
    fn a_query_takes_a_format_a_cap_a_timeout_and_whole_cells() {
        let cli = Cli::parse_from([
            "sql-bench",
            "query",
            "--conn",
            "x",
            "--format",
            "json",
            "--max-rows",
            "5",
            "--timeout",
            "2",
            "--full",
            "-",
        ]);
        let Some(Command::Query {
            format,
            max_rows,
            timeout,
            full,
            sql,
            ..
        }) = cli.command
        else {
            panic!("expected a query, got {:?}", cli.command);
        };
        assert_eq!(format, Format::Json);
        assert_eq!(max_rows, 5);
        assert_eq!(timeout, 2);
        assert!(full);
        assert_eq!(sql, "-", "the statement comes from stdin");
    }

    #[test]
    fn a_bench_takes_a_run_count_and_a_cap_and_has_defaults() {
        let cli = Cli::parse_from(["sql-bench", "bench", "--conn", "x", "select 1"]);
        let Some(Command::Bench {
            conn,
            runs,
            max_rows,
            sql,
        }) = cli.command
        else {
            panic!("expected a bench, got {:?}", cli.command);
        };
        assert_eq!(
            (conn.as_str(), runs, max_rows, sql.as_str()),
            ("x", 20, None, "select 1")
        );

        let cli = Cli::parse_from([
            "sql-bench",
            "bench",
            "--conn",
            "x",
            "--runs",
            "5",
            "--max-rows",
            "100",
            "select 1",
        ]);
        let Some(Command::Bench { runs, max_rows, .. }) = cli.command else {
            panic!("expected a bench");
        };
        assert_eq!((runs, max_rows), (5, Some(100)));
        assert!(
            Cli::try_parse_from([
                "sql-bench",
                "bench",
                "--conn",
                "x",
                "--runs",
                "0",
                "select 1"
            ])
            .is_err(),
            "zero runs has nothing to report"
        );
    }

    #[test]
    fn a_percentile_is_the_nearest_rank_sample() {
        let five = [10, 20, 30, 40, 50];
        assert_eq!(percentile(&five, 0), 10, "the minimum");
        assert_eq!(percentile(&five, 50), 30, "ceil(2.5) is the third");
        assert_eq!(percentile(&five, 95), 50, "ceil(4.75) is the fifth");
        assert_eq!(percentile(&five, 100), 50, "the maximum");
        assert_eq!(percentile(&[7], 95), 7, "one sample is every percentile");
        let twenty: Vec<u32> = (1..=20).collect();
        assert_eq!(percentile(&twenty, 50), 10);
        assert_eq!(percentile(&twenty, 95), 19);
    }

    #[test]
    fn a_phase_row_is_the_name_and_its_four_numbers() {
        let mut samples = [4, 1, 9, 2, 3];
        assert_eq!(
            phase("total", &mut samples),
            vec![
                Cell::Text("total".to_owned()),
                Cell::Int(1),
                Cell::Int(3),
                Cell::Int(9),
                Cell::Int(9),
            ]
        );
    }

    #[test]
    fn a_listing_takes_a_schema_and_a_kind() {
        let cli = Cli::parse_from([
            "sql-bench",
            "objects",
            "--conn",
            "x",
            "--schema",
            "BENCH",
            "--kind",
            "package",
        ]);
        let Some(Command::Objects {
            schema,
            kind,
            pattern,
            ..
        }) = cli.command
        else {
            panic!("expected a listing, got {:?}", cli.command);
        };
        assert_eq!(schema.as_deref(), Some("BENCH"));
        assert_eq!(kind, Some(ObjectKind::Package));
        assert_eq!(pattern, None);
    }

    #[test]
    fn an_unknown_kind_is_refused_by_the_parser() {
        assert!(
            Cli::try_parse_from(["sql-bench", "objects", "--conn", "x", "--kind", "trigger"])
                .is_err()
        );
    }

    #[test]
    fn an_unknown_connection_names_the_ones_there_are() {
        let config = Config {
            connections: vec![],
            ..Config::default()
        };
        assert!(open(&config, "nope").is_none());
    }

    #[test]
    fn the_global_flags_may_be_written_either_side_of_a_subcommand() {
        let cli = Cli::parse_from([
            "sql-bench",
            "--config",
            "config.local.toml",
            "objects",
            "--conn",
            "local-mssql",
        ]);
        assert_eq!(cli.config, Some(PathBuf::from("config.local.toml")));
        assert_eq!(cli.command.as_ref().map(Command::name), Some("objects"));
    }

    #[test]
    fn no_subcommand_is_the_tui() {
        let cli = Cli::parse_from(["sql-bench"]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn a_replay_says_which_keys_at_which_size_and_where_the_frames_go() {
        let cli = Cli::parse_from([
            "sql-bench",
            "--replay",
            "keys.txt",
            "--size",
            "120x40",
            "--frames-dir",
            "/tmp/f",
            "--frame-styles",
        ]);
        assert_eq!(cli.replay, Some(PathBuf::from("keys.txt")));
        assert_eq!(
            cli.size,
            Some(Size {
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(cli.frames_dir, Some(PathBuf::from("/tmp/f")));
        assert!(cli.frame_styles);
    }

    #[test]
    fn a_size_is_two_numbers_and_an_x() {
        assert_eq!("80x24".parse(), Ok(Size { cols: 80, rows: 24 }));
        for wrong in ["80", "80x", "x24", "80 x 24", "0x24", "80x0", "-1x24"] {
            assert!(wrong.parse::<Size>().is_err(), "{wrong} parsed");
        }
    }
}
