//! `sql-bench`: a terminal workbench for SQL Server and Oracle.

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use sql_bench::cli::{self, Cli};
use sql_bench::trace::Trace;
use sql_bench::{config, run};

fn main() -> ExitCode {
    run().unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        ExitCode::FAILURE
    })
}

fn run() -> Result<ExitCode> {
    // The first thing a traced run writes, before the command line has even
    // been parsed: startup is the first `frame` line minus this one, and a
    // clock the shell started itself is the only one both ends share.
    Trace::from_env().event("start", &[]);
    let cli = Cli::parse();
    // Read before anything is dispatched: a config that cannot be read is a
    // clear error now rather than a surprise on the first connection.
    let config = config::load(
        &cli.config_path(),
        cli.config.is_some() || config::named_by_env(),
    )?;
    match cli.command {
        Some(_) => {
            run::restore_only_from_main();
            cli::run(&cli, &config)
        }
        None if cli.replay.is_some() => {
            run::restore_only_from_main();
            run::replay(&config, &cli)
        }
        None => run::run(&config, &cli, panic_after(&cli)).map(|()| ExitCode::SUCCESS),
    }
}

/// `--panic-after-ms`, which a release build has no flag for at all.
fn panic_after(cli: &Cli) -> Option<std::time::Duration> {
    #[cfg(debug_assertions)]
    return cli.panic_after_ms.map(std::time::Duration::from_millis);
    #[cfg(not(debug_assertions))]
    {
        let _ = cli;
        None
    }
}
