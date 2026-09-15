//! `sql-bench`: a terminal workbench for SQL Server and Oracle.

use anyhow::Result;
use clap::Parser;
use sql_bench::cli::{self, Cli};
use sql_bench::{config, run};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    // Read before anything is dispatched: a config that cannot be read is a
    // clear error now rather than a surprise on the first connection.
    let config = config::load(&path)?;
    match cli.command.as_ref() {
        Some(command) => cli::not_implemented(command.name()),
        None if cli.replay.is_some() => cli::not_implemented("replay"),
        None => run::run(&config),
    }
}
