//! `sql-bench`: a terminal workbench for SQL Server and Oracle.

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use sql_bench::cli::{self, Cli};
use sql_bench::{config, run};

fn main() -> ExitCode {
    run().unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        ExitCode::FAILURE
    })
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    // Read before anything is dispatched: a config that cannot be read is a
    // clear error now rather than a surprise on the first connection.
    let config = config::load(&path)?;
    match cli.command {
        Some(_) => cli::run(&cli, &config),
        None if cli.replay.is_some() => run::replay(&config, &cli),
        None => run::run(&config, &cli).map(|()| ExitCode::SUCCESS),
    }
}
