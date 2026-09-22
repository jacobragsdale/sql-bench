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

/// The 100 KB value `scripts/seed` puts in `big_text`, as an `nvarchar(max)`
/// on one server and a `CLOB` on the other.
const BIG: usize = 102_400;

#[test]
fn a_hundred_kilobyte_value_survives_every_format() {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return;
    }
    for conn in ["local-mssql", "local-oracle"] {
        let sql = "select body from bench.big_text where id = 3";
        let run = |format: &str| {
            let output = sql_bench(&["query", "--conn", conn, "--format", format, "--full", sql]);
            assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
            String::from_utf8(output.stdout).expect("every format is UTF-8")
        };

        // One header line, one record, and the record is the whole value:
        // nothing here quotes, because the body has no comma and no quote.
        let text = run("csv");
        let csv: Vec<&str> = text.lines().collect();
        assert_eq!(csv.len(), 2, "{conn}: csv is a header and one record");
        assert_eq!(csv[1].chars().count(), BIG, "{conn}");

        // `[`, the one object, `]` — and the object holds the whole value.
        let json = run("json");
        let lines: Vec<&str> = json.lines().collect();
        assert_eq!(lines.len(), 3, "{conn}: {}", &json[..80.min(json.len())]);
        assert_eq!(lines[0], "[");
        assert_eq!(lines[2], "]");
        let value = lines[1]
            .split_once(": \"")
            .and_then(|(_, rest)| rest.strip_suffix("\"}"))
            .unwrap_or_else(|| panic!("{conn}: no string value in {}", &lines[1][..80]));
        assert_eq!(value.chars().count(), BIG, "{conn}");

        // Header, rule, row — and the rule is as wide as the value, which is
        // what `--full` means.
        let table: Vec<String> = run("table").lines().map(str::to_owned).collect();
        assert_eq!(table.len(), 3, "{conn}");
        assert_eq!(table[1].chars().count(), BIG, "{conn}");
        assert_eq!(table[2].chars().count(), BIG, "{conn}");
    }
}

#[test]
fn a_statement_that_returns_no_columns_prints_nothing_and_succeeds() {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return;
    }
    // Both leave the seed data as it was: one rolls its transaction back, the
    // other puts the status back itself, because the procedure commits.
    for (conn, sql) in [
        (
            "local-mssql",
            "begin transaction; exec bench.sp_mark_shipped 1; rollback transaction",
        ),
        (
            "local-oracle",
            "begin bench.mark_shipped(1); \
             update bench.orders set status = 'PAID' where id = 1; commit; end;",
        ),
    ] {
        let table = sql_bench(&["query", "--conn", conn, sql]);
        assert_eq!(table.status.code(), Some(0), "{}", stderr(&table));
        assert!(table.stdout.is_empty(), "{conn}: a table of no columns");

        let json = sql_bench(&["query", "--conn", conn, "--format", "json", sql]);
        assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
        assert_eq!(
            String::from_utf8_lossy(&json.stdout),
            "[]\n",
            "{conn}: an empty array is still JSON"
        );
    }
}

#[test]
fn two_columns_of_the_same_name_are_two_json_keys() {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return;
    }
    for (conn, sql, keys) in [
        (
            "local-mssql",
            "select 1 as a, 2 as a",
            "\"a\": 1, \"a_2\": 2",
        ),
        (
            "local-oracle",
            "select 1 as a, 2 as a from dual",
            "\"A\": 1, \"A_2\": 2",
        ),
    ] {
        let output = sql_bench(&["query", "--conn", conn, "--format", "json", sql]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("[\n  {{{keys}}}\n]\n"),
            "{conn}: neither column may be lost to the other"
        );
    }
}
