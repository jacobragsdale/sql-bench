//! What a subcommand leaves behind: an exit code, a message on stderr, and —
//! when it failed — nothing at all on stdout, so a pipeline never reads half
//! an answer as a whole one.

use std::path::Path;
use std::process::{Command, Output};

/// The built binary, pointed at the committed `config.local.toml`.
fn sql_bench(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sql-bench"))
        .args(arguments)
        .env(
            "SQL_BENCH_CONFIG",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml"),
        )
        .output()
        .expect("the binary this test was built alongside")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_unknown_connection_lists_the_ones_there_are() {
    let output = sql_bench(&["query", "--conn", "x", "select 1"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output).trim(),
        "unknown connection 'x'; configured: local-mssql, local-oracle"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn a_bench_prints_one_row_per_phase_and_a_rate() {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return;
    }
    let output = sql_bench(&["bench", "--conn", "local-mssql", "--runs", "3", "select 1"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let phases: Vec<&str> = stdout
        .lines()
        .skip(2)
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(phases[..3], ["connect", "first_row", "total"], "{stdout}");
    assert!(stdout.ends_with("rows/s over 3 runs\n"), "{stdout}");
}

#[test]
fn a_query_the_server_refuses_prints_its_complaint_and_no_rows() {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return;
    }
    let output = sql_bench(&[
        "query",
        "--conn",
        "local-mssql",
        "select from bench.customers",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Incorrect syntax near the keyword 'from'"),
        "{}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty(), "stdout stays empty on a failure");
}
