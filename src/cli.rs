//! The command line: the flags every run takes, and the subcommands that do
//! one thing without opening the TUI.
//!
//! A bare invocation opens the TUI; each subcommand is how the same feature
//! is verified headlessly, which is rule 3 in `CLAUDE.md`. The shape is
//! settled here in T1.2 so the docs and the scripts can use it — the work
//! behind each subcommand lands with its own ticket, and until then they
//! say so and exit 2.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Parser, Subcommand, ValueHint};

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
        /// The statement to run
        #[arg(value_name = "SQL")]
        sql: String,
    },
    /// List the tables, views and procedures of a connection
    Objects {
        /// Connection name from config.toml
        #[arg(long, value_name = "NAME")]
        conn: String,
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
        /// The statement to time
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
        let Some(Command::Query { conn, sql }) = cli.command else {
            panic!("expected a query, got {:?}", cli.command);
        };
        assert_eq!(conn, "x");
        assert_eq!(sql, "select 1");
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
    }

    #[test]
    fn a_size_is_two_numbers_and_an_x() {
        assert_eq!("80x24".parse(), Ok(Size { cols: 80, rows: 24 }));
        for wrong in ["80", "80x", "x24", "80 x 24", "0x24", "80x0", "-1x24"] {
            assert!(wrong.parse::<Size>().is_err(), "{wrong} parsed");
        }
    }
}
