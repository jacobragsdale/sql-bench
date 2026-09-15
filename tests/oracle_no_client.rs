//! What opening an Oracle connection says when there is no Instant Client.
//!
//! In its own file because ODPI-C loads the client once per process: the
//! `OnceLock` behind it has to be untouched for this to be the first attempt,
//! and `cargo test` gives every integration file a process of its own.
//!
//! No database needed, so this one is not behind `SQL_BENCH_TEST_DBS`.

use std::path::PathBuf;

use sql_bench::config;
use sql_bench::db::Connection;
use sql_bench::db::model::DbError;

fn ledger() -> config::Connection {
    config::Connection {
        name: "ledger".to_owned(),
        kind: config::Kind::Oracle,
        host: "localhost".to_owned(),
        port: 1521,
        database: None,
        service: Some("FREEPDB1".to_owned()),
        user: "bench".to_owned(),
        password: Some(config::Password::Literal("bench".to_owned())),
        trust_cert: false,
        encrypt: true,
    }
}

#[test]
fn an_empty_client_directory_says_what_to_set() {
    let empty = tempfile::tempdir().expect("a temporary directory");

    // The environment, not the config, because that is the path the README
    // tells people to use and the one nothing else in the suite covers.
    // SAFETY: nothing else in this process reads the environment — it holds
    // this one test, which has not started a thread.
    unsafe { std::env::set_var("SQL_BENCH_ORACLE_CLIENT_DIR", empty.path()) };
    let config = config::Config {
        oracle: config::Oracle {
            client_lib_dir: None,
        },
        connections: Vec::new(),
    };

    let failure = Connection::open(&ledger(), &config).expect_err("the directory is empty");
    assert_eq!(
        failure,
        DbError::Connect(
            "Oracle client library not found; set [oracle] client_lib_dir or \
             SQL_BENCH_ORACLE_CLIENT_DIR (see README)"
                .to_owned()
        )
    );

    // And once the process has decided, it has decided: a second connection
    // gets the same answer rather than a half-initialised client library.
    let named = config::Config {
        oracle: config::Oracle {
            client_lib_dir: Some(PathBuf::from("/nowhere/at/all")),
        },
        connections: Vec::new(),
    };
    assert_eq!(
        Connection::open(&ledger(), &named).expect_err("still no client"),
        failure
    );
}
