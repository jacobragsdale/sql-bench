//! `config.toml`: the connections sql-bench can open, and where the Oracle
//! Instant Client is.
//!
//! It lives at `~/.config/sql-bench/config.toml`, or wherever
//! `$SQL_BENCH_CONFIG` or `--config` says. A missing default file is not an
//! error — it is an empty configuration, which is what a first run has — but
//! a file somebody named has to be there, or a typo in the name would look
//! like a file with no connections in it. So does a key the file does not
//! know: `pasword` is refused, not ignored.
//!
//! ```toml
//! [oracle]
//! client_lib_dir = "~/.local/opt/oracle/instantclient_23_26"  # optional
//!
//! [[connection]]
//! name = "local-mssql"   # unique; this is what --conn takes
//! kind = "mssql"         # or "oracle"
//! host = "localhost"
//! port = 1433            # left out: 1433 for mssql, 1521 for oracle
//! database = "bench"     # mssql only
//! user = "sa"
//! password = "..."       # or password_env = "VAR", or password_cmd = "pass show x"
//! trust_cert = true      # mssql only, default false
//! encrypt = true         # mssql only, default true
//! ```
//!
//! Everything the file says is checked when it is read, except the
//! passwords: those are resolved when a connection is opened, so a file
//! with ten `password_cmd`s does not run ten commands to answer `--help`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// A whole `config.toml`, validated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub oracle: Oracle,
    pub connections: Vec<Connection>,
    /// The file this was read from, for a message that has to say where the
    /// connections were looked for.
    pub path: PathBuf,
}

impl Config {
    /// The connection `--conn` names, if the file has one.
    #[must_use]
    pub fn connection(&self, name: &str) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|connection| connection.name == name)
    }
}

/// The `[oracle]` table: what the ODPI-C driver needs to find at runtime.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    /// The Instant Client directory; a leading `~` is your home directory.
    #[serde(default)]
    pub client_lib_dir: Option<PathBuf>,
}

/// Which driver a connection is opened with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mssql,
    Oracle,
}

impl Kind {
    /// What the vendor's installer listens on, for a file that says no port.
    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Self::Mssql => 1433,
            Self::Oracle => 1521,
        }
    }
}

/// Where a password comes from. One source or none: a file that names two
/// is a file whose author disagrees with themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Password {
    Literal(String),
    /// An environment variable to read.
    Env(String),
    /// A command to run through `sh -c`, whose stdout is the password.
    Command(String),
}

/// One `[[connection]]`, with the defaults filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    /// Always set for `mssql`, never meaningful for `oracle`.
    pub database: Option<String>,
    /// Always set for `oracle`, never meaningful for `mssql`.
    pub service: Option<String>,
    pub user: String,
    pub password: Option<Password>,
    pub trust_cert: bool,
    pub encrypt: bool,
}

impl Connection {
    /// The password, resolved now — when the connection is being opened,
    /// never when the file is read.
    pub fn password(&self) -> Result<Option<String>> {
        let resolved = match self.password.as_ref() {
            None => return Ok(None),
            Some(Password::Literal(text)) => text.clone(),
            Some(Password::Env(variable)) => std::env::var(variable)
                .with_context(|| format!("reading ${variable}"))
                .with_context(|| format!("connection {:?}", self.name))?,
            Some(Password::Command(command)) => {
                run_password_cmd(command).with_context(|| format!("connection {:?}", self.name))?
            }
        };
        Ok(Some(resolved))
    }
}

/// `sh -c`, so the file can write a pipeline. A trailing newline is the
/// shell's, not the password's.
fn run_password_cmd(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("running password_cmd {command:?}"))?;
    if !output.status.success() {
        let complaint = String::from_utf8_lossy(&output.stderr);
        bail!(
            "password_cmd {command:?} failed ({}): {}",
            output.status,
            complaint.trim()
        );
    }
    let password = String::from_utf8(output.stdout)
        .with_context(|| format!("password_cmd {command:?} wrote something that is not text"))?;
    Ok(password.trim_end_matches(['\r', '\n']).to_owned())
}

/// The file as it is written, before anything is checked. Kept separate
/// from [`Connection`] so that a bad `kind` is reported against the
/// connection's name rather than against a line number.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    oracle: Oracle,
    #[serde(default, rename = "connection")]
    connections: Vec<RawConnection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    name: String,
    kind: String,
    host: String,
    port: Option<u16>,
    database: Option<String>,
    service: Option<String>,
    user: String,
    password: Option<String>,
    password_env: Option<String>,
    password_cmd: Option<String>,
    trust_cert: Option<bool>,
    encrypt: Option<bool>,
}

impl RawConnection {
    fn check(self) -> Result<Connection> {
        let kind = match self.kind.as_str() {
            "mssql" => Kind::Mssql,
            "oracle" => Kind::Oracle,
            other => bail!("unknown kind {other:?}; kind is \"mssql\" or \"oracle\""),
        };
        if self.name.trim().is_empty() {
            bail!("name is empty; a connection is picked by its name");
        }
        if self.host.trim().is_empty() {
            bail!("host is empty");
        }
        // A key the other kind reads, here, would be a setting that looks
        // like it does something and does nothing.
        let foreign = match kind {
            Kind::Mssql => vec![("service", self.service.is_some())],
            Kind::Oracle => vec![
                ("database", self.database.is_some()),
                ("trust_cert", self.trust_cert.is_some()),
                ("encrypt", self.encrypt.is_some()),
            ],
        };
        if let Some((key, _)) = foreign.iter().find(|(_, set)| *set) {
            bail!("{key} is not a setting of kind {:?}", self.kind);
        }
        match kind {
            Kind::Mssql if self.database.is_none() => {
                bail!("kind is \"mssql\", which needs a database");
            }
            Kind::Oracle if self.service.is_none() => {
                bail!("kind is \"oracle\", which needs a service");
            }
            _ => {}
        }
        let port = match self.port {
            Some(0) => bail!(
                "port 0 is not a port; leave port out for the default {}",
                kind.default_port()
            ),
            Some(port) => port,
            None => kind.default_port(),
        };
        let password = match (self.password, self.password_env, self.password_cmd) {
            (None, None, None) => None,
            (Some(literal), None, None) => Some(Password::Literal(literal)),
            (None, Some(variable), None) => Some(Password::Env(variable)),
            (None, None, Some(command)) => Some(Password::Command(command)),
            _ => bail!("more than one of password, password_env and password_cmd; give one"),
        };
        Ok(Connection {
            name: self.name,
            kind,
            host: self.host,
            port,
            database: self.database,
            service: self.service,
            user: self.user,
            password,
            trust_cert: self.trust_cert.unwrap_or(false),
            encrypt: self.encrypt.unwrap_or(true),
        })
    }
}

/// The file `$SQL_BENCH_CONFIG` names, else
/// `~/.config/sql-bench/config.toml`.
#[must_use]
pub fn default_path() -> PathBuf {
    resolve_path(
        std::env::var_os("SQL_BENCH_CONFIG"),
        std::env::var_os("HOME"),
    )
}

/// Whether `$SQL_BENCH_CONFIG` names a file — which then has to exist.
#[must_use]
pub fn named_by_env() -> bool {
    std::env::var_os("SQL_BENCH_CONFIG").is_some_and(|path| !path.is_empty())
}

fn resolve_path(named: Option<OsString>, home: Option<OsString>) -> PathBuf {
    let home = home.map(PathBuf::from);
    match named.filter(|path| !path.is_empty()) {
        Some(path) => expand_home(Path::new(&path), home.as_deref()),
        None => home
            .unwrap_or_default()
            .join(".config")
            .join("sql-bench")
            .join("config.toml"),
    }
}

/// A path a person typed, a leading `~` their home directory.
#[must_use]
pub fn expand(path: &Path) -> PathBuf {
    expand_home(path, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

/// A leading `~/` — or a bare `~` — is the home directory. Nothing else is
/// expanded: this is a file written by hand, not a shell.
fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    match (home, path.strip_prefix("~")) {
        (Some(home), Ok(rest)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Reads and checks the file. A missing one is an empty configuration
/// unless it is `required` — named by `--config` or `$SQL_BENCH_CONFIG`;
/// anything else wrong names the file, and the connection or the line.
pub fn load(path: &Path, required: bool) -> Result<Config> {
    let config = match std::fs::read_to_string(path) {
        Ok(source) => parse(
            &source,
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        )
        .with_context(|| path.display().to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {
            Config::default()
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", path.display()));
        }
    };
    Ok(Config {
        path: path.to_path_buf(),
        ..config
    })
}

fn parse(source: &str, home: Option<&Path>) -> Result<Config> {
    let file: File = toml::from_str(source)?;
    let mut connections: Vec<Connection> = Vec::with_capacity(file.connections.len());
    for raw in file.connections {
        let name = raw.name.clone();
        if connections.iter().any(|earlier| earlier.name == name) {
            bail!("connection {name:?}: named twice; connection names are how --conn picks one");
        }
        connections.push(
            raw.check()
                .with_context(|| format!("connection {name:?}"))?,
        );
    }
    Ok(Config {
        oracle: Oracle {
            client_lib_dir: file
                .oracle
                .client_lib_dir
                .map(|path| expand_home(&path, home)),
        },
        connections,
        path: PathBuf::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/tester";

    fn read(source: &str) -> Result<Config> {
        parse(source, Some(Path::new(HOME)))
    }

    /// The failure as `main` prints it: every context joined with ": ".
    fn failure(source: &str) -> String {
        format!("{:#}", read(source).unwrap_err())
    }

    const LOCAL_MSSQL: &str = r#"
[[connection]]
name = "local-mssql"
kind = "mssql"
host = "localhost"
database = "bench"
user = "sa"
password = "Bench_Pass1!"
trust_cert = true
"#;

    #[test]
    fn an_empty_file_is_an_empty_configuration() {
        assert_eq!(read("").unwrap(), Config::default());
    }

    #[test]
    fn a_missing_file_is_an_empty_configuration_unless_it_was_named() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("nothing-here.toml");
        let config = load(&missing, false).unwrap();
        assert!(config.connections.is_empty());
        assert_eq!(config.path, missing, "and it says where it looked");
        let failure = format!("{:#}", load(&missing, true).unwrap_err());
        assert!(failure.starts_with("reading "), "{failure}");
        assert!(failure.contains("nothing-here.toml"), "{failure}");
    }

    #[test]
    fn a_key_the_file_does_not_know_is_an_error_not_a_default() {
        let failure = failure(&LOCAL_MSSQL.replace("password =", "pasword ="));
        assert!(failure.contains("unknown field `pasword`"), "{failure}");
        assert!(failure.contains("line 8"), "{failure}");
    }

    #[test]
    fn a_connection_keeps_what_the_file_says_and_defaults_the_rest() {
        let config = read(LOCAL_MSSQL).unwrap();
        let connection = config.connection("local-mssql").unwrap();
        assert_eq!(connection.kind, Kind::Mssql);
        assert_eq!(connection.host, "localhost");
        assert_eq!(connection.port, 1433);
        assert_eq!(connection.database.as_deref(), Some("bench"));
        assert_eq!(connection.user, "sa");
        assert!(connection.trust_cert);
        assert!(connection.encrypt, "encrypt defaults to on");
        assert_eq!(
            connection.password,
            Some(Password::Literal("Bench_Pass1!".to_owned()))
        );
    }

    #[test]
    fn a_port_left_out_is_the_kinds_own() {
        let config = read(
            r#"
[[connection]]
name = "o"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
"#,
        )
        .unwrap();
        assert_eq!(config.connection("o").unwrap().port, 1521);
        assert_eq!(read(LOCAL_MSSQL).unwrap().connections[0].port, 1433);
    }

    #[test]
    fn a_port_of_zero_is_an_error_naming_the_connection() {
        assert_eq!(
            failure(
                r#"
[[connection]]
name = "reports"
kind = "mssql"
host = "localhost"
port = 0
database = "bench"
user = "sa"
"#
            ),
            "connection \"reports\": port 0 is not a port; leave port out for the default 1433"
        );
    }

    #[test]
    fn a_name_with_spaces_is_a_name_like_any_other() {
        let config = read(
            r#"
[[connection]]
name = "prod reporting"
kind = "mssql"
host = "localhost"
database = "bench"
user = "sa"
"#,
        )
        .unwrap();
        assert_eq!(config.connection("prod reporting").unwrap().port, 1433);
    }

    #[test]
    fn the_same_name_twice_is_an_error_naming_it() {
        let doubled = format!("{LOCAL_MSSQL}{LOCAL_MSSQL}");
        assert_eq!(
            failure(&doubled),
            "connection \"local-mssql\": named twice; connection names are how --conn picks one"
        );
    }

    #[test]
    fn an_unknown_kind_is_an_error_naming_the_connection() {
        let failure = failure(
            r#"
[[connection]]
name = "warehouse"
kind = "mysql"
host = "localhost"
user = "root"
"#,
        );
        assert_eq!(
            failure,
            "connection \"warehouse\": unknown kind \"mysql\"; kind is \"mssql\" or \"oracle\""
        );
    }

    #[test]
    fn mssql_without_a_database_is_an_error_naming_the_connection() {
        assert_eq!(
            failure(
                r#"
[[connection]]
name = "reports"
kind = "mssql"
host = "localhost"
user = "sa"
"#
            ),
            "connection \"reports\": kind is \"mssql\", which needs a database"
        );
    }

    #[test]
    fn oracle_without_a_service_is_an_error_naming_the_connection() {
        assert_eq!(
            failure(
                r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
user = "bench"
"#
            ),
            "connection \"ledger\": kind is \"oracle\", which needs a service"
        );
    }

    #[test]
    fn a_setting_of_the_other_kind_or_an_empty_name_is_an_error_not_ignored() {
        let oracle = "[[connection]]\nname = \"o\"\nkind = \"oracle\"\nhost = \"h\"\n\
                      service = \"s\"\nuser = \"u\"\n";
        assert_eq!(
            failure(&format!("{oracle}trust_cert = true\n")),
            "connection \"o\": trust_cert is not a setting of kind \"oracle\""
        );
        assert_eq!(
            failure(&format!("{oracle}database = \"d\"\n")),
            "connection \"o\": database is not a setting of kind \"oracle\""
        );
        assert_eq!(
            failure(&LOCAL_MSSQL.replace("user = ", "service = \"s\"\nuser = ")),
            "connection \"local-mssql\": service is not a setting of kind \"mssql\""
        );
        assert_eq!(
            failure(&LOCAL_MSSQL.replace("\"local-mssql\"", "\"\"")),
            "connection \"\": name is empty; a connection is picked by its name"
        );
        assert_eq!(
            failure(&LOCAL_MSSQL.replace("\"localhost\"", "\" \"")),
            "connection \"local-mssql\": host is empty"
        );
    }

    #[test]
    fn two_password_sources_is_an_error_naming_the_connection() {
        assert_eq!(
            failure(
                r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
password = "bench"
password_cmd = "pass show bench"
"#
            ),
            "connection \"ledger\": more than one of password, password_env and password_cmd; give one"
        );
    }

    #[test]
    fn a_broken_file_names_the_file_and_the_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "[[connection]]\nname = \"x\"\nkind = mssql\n").unwrap();
        let failure = format!("{:#}", load(&path, true).unwrap_err());
        assert!(failure.contains("config.toml"), "{failure}");
        assert!(failure.contains("line 3"), "{failure}");
    }

    #[test]
    fn a_tilde_in_the_client_directory_is_the_home_directory() {
        let config = read("[oracle]\nclient_lib_dir = \"~/.local/opt/oracle/ic\"\n").unwrap();
        assert_eq!(
            config.oracle.client_lib_dir,
            Some(PathBuf::from("/home/tester/.local/opt/oracle/ic"))
        );
    }

    #[test]
    fn a_client_directory_that_is_not_a_tilde_is_left_alone() {
        let config = read("[oracle]\nclient_lib_dir = \"/opt/oracle/ic\"\n").unwrap();
        assert_eq!(
            config.oracle.client_lib_dir,
            Some(PathBuf::from("/opt/oracle/ic"))
        );
    }

    #[test]
    fn a_file_with_only_oracle_is_a_configuration_with_no_connections() {
        let config = read("[oracle]\nclient_lib_dir = \"/opt/oracle/ic\"\n").unwrap();
        assert!(config.connections.is_empty(), "{config:?}");
    }

    #[test]
    fn the_environment_names_the_file_and_home_holds_the_default() {
        assert_eq!(
            resolve_path(Some("/etc/sql-bench.toml".into()), Some(HOME.into())),
            PathBuf::from("/etc/sql-bench.toml")
        );
        assert_eq!(
            resolve_path(Some("~/elsewhere.toml".into()), Some(HOME.into())),
            PathBuf::from("/home/tester/elsewhere.toml")
        );
        assert_eq!(
            resolve_path(None, Some(HOME.into())),
            PathBuf::from("/home/tester/.config/sql-bench/config.toml")
        );
        assert_eq!(
            resolve_path(Some(OsString::new()), Some(HOME.into())),
            PathBuf::from("/home/tester/.config/sql-bench/config.toml"),
            "an empty variable is not a file name"
        );
    }

    #[test]
    fn no_password_source_resolves_to_no_password() {
        let config = read(
            r#"
[[connection]]
name = "trusted"
kind = "mssql"
host = "localhost"
database = "bench"
user = "sa"
"#,
        )
        .unwrap();
        assert_eq!(config.connections[0].password().unwrap(), None);
    }

    #[test]
    fn a_password_command_is_run_and_its_trailing_newline_dropped() {
        let config = read(
            r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
password_cmd = "printf 'from the vault\n'"
"#,
        )
        .unwrap();
        assert_eq!(
            config.connections[0].password().unwrap().as_deref(),
            Some("from the vault")
        );
    }

    #[test]
    fn a_password_command_that_fails_says_what_it_complained_about() {
        let config = read(
            r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
password_cmd = "echo 'no such secret' >&2; exit 3"
"#,
        )
        .unwrap();
        let failure = format!("{:#}", config.connections[0].password().unwrap_err());
        assert!(failure.contains("connection \"ledger\""), "{failure}");
        assert!(failure.contains("no such secret"), "{failure}");
    }

    #[test]
    fn a_password_variable_is_read_from_the_environment() {
        // $HOME rather than a variable of our own: setting one is `unsafe` in
        // edition 2024, and every test process already has this one.
        let config = read(
            r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
password_env = "HOME"
"#,
        )
        .unwrap();
        assert_eq!(
            config.connections[0].password().unwrap(),
            std::env::var("HOME").ok()
        );
    }

    #[test]
    fn a_password_variable_that_is_not_set_names_the_connection_and_the_variable() {
        let config = read(
            r#"
[[connection]]
name = "ledger"
kind = "oracle"
host = "localhost"
service = "FREEPDB1"
user = "bench"
password_env = "SQL_BENCH_NO_SUCH_VARIABLE"
"#,
        )
        .unwrap();
        let failure = format!("{:#}", config.connections[0].password().unwrap_err());
        assert!(failure.contains("connection \"ledger\""), "{failure}");
        assert!(failure.contains("SQL_BENCH_NO_SUCH_VARIABLE"), "{failure}");
    }

    #[test]
    fn the_committed_files_are_the_ones_the_dev_loop_uses() {
        let local = read(include_str!("../config.local.toml")).unwrap();
        assert_eq!(
            local
                .connections
                .iter()
                .map(|connection| connection.name.as_str())
                .collect::<Vec<_>>(),
            ["local-mssql", "local-oracle"]
        );
        assert_eq!(
            local.oracle.client_lib_dir,
            Some(PathBuf::from(
                "/home/tester/.local/opt/oracle/instantclient_23_26"
            ))
        );
        read(include_str!("../config.example.toml")).unwrap();
    }
}
