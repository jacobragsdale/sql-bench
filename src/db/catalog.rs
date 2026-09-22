//! What a database says it holds: schemas, objects, columns, source.
//!
//! Ordinary SQL down the same [`Connection::query`] path as anything a user
//! types — no metadata API, no second protocol. The two dialects ask
//! different catalogs the same four questions, and every answer comes back
//! in the types below, so nothing above this file knows which vendor it is
//! talking to.
//!
//! Identifiers are folded the way each server folds them: Oracle stores
//! unquoted names upper case, so `bench.order_pkg` is looked up as
//! `BENCH.ORDER_PKG`; SQL Server keeps the case it was created with, so a
//! name is compared case-insensitively and an exact match wins when two
//! differ only by case (which needs a case-sensitive collation to happen at
//! all).

use std::fmt;
use std::str::FromStr;

use super::Connection;
use super::model::{Cell, DbError, QueryEvent, QueryOptions};
use crate::config::Kind;

/// What kind of thing an object is. `Package` is Oracle's alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    View,
    Procedure,
    Function,
    Package,
    Sequence,
}

impl ObjectKind {
    /// Every kind, in the order a listing shows them.
    pub const ALL: [Self; 6] = [
        Self::Table,
        Self::View,
        Self::Procedure,
        Self::Function,
        Self::Package,
        Self::Sequence,
    ];

    /// How the tree names the branch that holds them.
    #[must_use]
    pub fn plural(self) -> &'static str {
        match self {
            Self::Table => "Tables",
            Self::View => "Views",
            Self::Procedure => "Procedures",
            Self::Function => "Functions",
            Self::Package => "Packages",
            Self::Sequence => "Sequences",
        }
    }

    /// Every kind this backend has: only Oracle has packages.
    #[must_use]
    pub fn all_for(backend: Kind) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|kind| backend == Kind::Oracle || *kind != Self::Package)
            .collect()
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::Procedure => "procedure",
            Self::Function => "function",
            Self::Package => "package",
            Self::Sequence => "sequence",
        }
    }

    /// The `sys.objects.type` codes this kind covers. A SQL Server function
    /// is three of them: scalar, inline table-valued, multi-statement.
    fn mssql_types(self) -> &'static [&'static str] {
        match self {
            Self::Table => &["U"],
            Self::View => &["V"],
            Self::Procedure => &["P"],
            Self::Function => &["FN", "IF", "TF"],
            Self::Sequence => &["SO"],
            Self::Package => &[],
        }
    }

    fn from_mssql(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.mssql_types().contains(&code))
    }

    /// The `ALL_OBJECTS.OBJECT_TYPE` this kind is. `PACKAGE BODY` is not
    /// listed: a package is one object, and [`object_source`] fetches both
    /// halves of it.
    fn oracle_type(self) -> &'static str {
        match self {
            Self::Table => "TABLE",
            Self::View => "VIEW",
            Self::Procedure => "PROCEDURE",
            Self::Function => "FUNCTION",
            Self::Package => "PACKAGE",
            Self::Sequence => "SEQUENCE",
        }
    }

    fn from_oracle(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.oracle_type() == code)
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ObjectKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(text))
            .ok_or_else(|| {
                let known: Vec<&str> = Self::ALL.iter().map(|kind| kind.as_str()).collect();
                format!("unknown kind {text:?}; one of {}", known.join(", "))
            })
    }
}

/// One row of an object listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    /// When the server last changed it, as the server formats it.
    pub modified: Option<String>,
}

/// One column of a table or a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// The declared type, spelled the way the vendor's own tools spell it:
    /// SQL Server `nvarchar(100)`, `varbinary(max)`, `decimal(12,2)`,
    /// `datetime2(7)`; Oracle `VARCHAR2(200 BYTE)`, `NVARCHAR2(100)`,
    /// `NUMBER(12,2)`, `TIMESTAMP(6)`.
    pub type_text: String,
    pub nullable: bool,
    pub is_pk: bool,
}

/// What the object tree asks the catalog for, and what came back.
///
/// One request is one query down the ordinary [`Connection::query`] path, so
/// the tree loads the way a user's statement runs: the runtime issues
/// [`CatalogRequest::sql`], collects the rows and hands them to
/// [`CatalogRequest::answer`]. The blocking calls below are the same two
/// halves with a `for` loop between them, for the CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogRequest {
    Schemas,
    /// Every object in every schema worth showing, in one query: what the
    /// finder searches and what the tree fills its branches from.
    Index,
    Objects {
        schema: String,
        kind: ObjectKind,
    },
    /// `show` is `i` asking for them in the results pane as well as the tree.
    Columns {
        schema: String,
        table: String,
        show: bool,
    },
    Source {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
}

/// The rows of a [`CatalogRequest`], parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogAnswer {
    Schemas(Vec<String>),
    Index(Vec<DbObject>),
    Objects(Vec<DbObject>),
    Columns(Vec<ColumnInfo>),
    Source(String),
}

impl CatalogRequest {
    /// The one query that answers it.
    #[must_use]
    pub fn sql(&self, backend: Kind) -> String {
        match self {
            Self::Schemas => schemas_sql(backend),
            Self::Index => objects_sql(backend, None, None),
            Self::Objects { schema, kind } => objects_sql(backend, Some(schema), Some(*kind)),
            Self::Columns { schema, table, .. } => columns_sql(backend, schema, table),
            Self::Source {
                schema,
                name,
                kind: ObjectKind::Table,
            } => columns_sql(backend, schema, name),
            Self::Source { schema, name, kind } => source_sql(backend, schema, name, *kind),
        }
    }

    /// What those rows mean.
    pub fn answer(&self, backend: Kind, rows: Vec<Vec<Cell>>) -> Result<CatalogAnswer, DbError> {
        Ok(match self {
            Self::Schemas => CatalogAnswer::Schemas(parse_schemas(&rows)),
            Self::Index => CatalogAnswer::Index(parse_objects(backend, rows)),
            Self::Objects { .. } => CatalogAnswer::Objects(parse_objects(backend, rows)),
            Self::Columns { .. } => CatalogAnswer::Columns(parse_columns(backend, &rows)),
            Self::Source {
                schema,
                name,
                kind: ObjectKind::Table,
            } => CatalogAnswer::Source(table_ddl(
                &fold(backend, schema),
                &fold(backend, name),
                &parse_columns(backend, &rows),
            )?),
            Self::Source { schema, name, .. } => {
                CatalogAnswer::Source(parse_source(backend, schema, name, &rows)?)
            }
        })
    }
}

/// The schemas worth showing: on SQL Server everything but `sys`,
/// `INFORMATION_SCHEMA`, `guest` and the fixed `db_*` roles; on Oracle every
/// account Oracle did not create, plus the one we are logged in as.
pub fn list_schemas(connection: &Connection) -> Result<Vec<String>, DbError> {
    let rows = rows(connection, &schemas_sql(connection.kind()))?;
    Ok(parse_schemas(&rows))
}

fn schemas_sql(backend: Kind) -> String {
    match backend {
        Kind::Mssql => "select s.name from sys.schemas s \
             where s.name not in ('sys', 'INFORMATION_SCHEMA', 'guest') \
               and s.name not like 'db[_]%' \
             order by s.name"
            .to_owned(),
        Kind::Oracle => format!("select username from all_users where {OWNERS} order by username"),
    }
}

fn parse_schemas(rows: &[Vec<Cell>]) -> Vec<String> {
    rows.iter().map(|row| text(row, 0)).collect()
}

/// Everything of `kind` in `schema`, or everything in every schema worth
/// showing when either is [`None`].
pub fn list_objects(
    connection: &Connection,
    schema: Option<&str>,
    kind: Option<ObjectKind>,
) -> Result<Vec<DbObject>, DbError> {
    let backend = connection.kind();
    let rows = rows(connection, &objects_sql(backend, schema, kind))?;
    Ok(parse_objects(backend, rows))
}

fn objects_sql(backend: Kind, schema: Option<&str>, kind: Option<ObjectKind>) -> String {
    match backend {
        Kind::Mssql => {
            let types = kind.map_or_else(
                || {
                    ObjectKind::ALL
                        .iter()
                        .flat_map(|kind| kind.mssql_types())
                        .copied()
                        .collect::<Vec<_>>()
                },
                |kind| kind.mssql_types().to_vec(),
            );
            format!(
                "select s.name, o.name, rtrim(o.type), \
                        convert(varchar(19), o.modify_date, 120) \
                 from sys.objects o \
                 join sys.schemas s on s.schema_id = o.schema_id \
                 where o.is_ms_shipped = 0 and rtrim(o.type) in ({types}) \
                   and s.name not in ('sys', 'INFORMATION_SCHEMA', 'guest') {schema} \
                 order by s.name, o.type, o.name",
                types = list(&types),
                schema = schema.map_or_else(String::new, |schema| format!(
                    "and lower(s.name) = lower({})",
                    quoted(schema)
                )),
            )
        }
        Kind::Oracle => {
            let types: Vec<&str> = kind.map_or_else(
                || {
                    ObjectKind::ALL
                        .iter()
                        .map(|kind| kind.oracle_type())
                        .collect()
                },
                |kind| vec![kind.oracle_type()],
            );
            format!(
                "select o.owner, o.object_name, o.object_type, \
                        to_char(o.last_ddl_time, 'YYYY-MM-DD HH24:MI:SS') \
                 from all_objects o \
                 where o.object_type in ({types}) and {owner} \
                 order by o.owner, o.object_type, o.object_name",
                types = list(&types),
                owner = schema.map_or_else(
                    || format!("o.owner in (select username from all_users where {OWNERS})"),
                    |schema| format!("o.owner = {}", owner(schema))
                ),
            )
        }
    }
}

fn parse_objects(backend: Kind, rows: Vec<Vec<Cell>>) -> Vec<DbObject> {
    let from_code = match backend {
        Kind::Mssql => ObjectKind::from_mssql,
        Kind::Oracle => ObjectKind::from_oracle,
    };
    rows.into_iter()
        .filter_map(|row| {
            Some(DbObject {
                schema: text(&row, 0),
                name: text(&row, 1),
                kind: from_code(&text(&row, 2))?,
                modified: maybe(&row, 3),
            })
        })
        .collect()
}

/// The columns of a table or a view, in declaration order.
pub fn list_columns(
    connection: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, DbError> {
    let backend = connection.kind();
    let rows = rows(connection, &columns_sql(backend, schema, table))?;
    Ok(parse_columns(backend, &rows))
}

fn columns_sql(backend: Kind, schema: &str, table: &str) -> String {
    match backend {
        Kind::Mssql => format!(
            "select c.name, t.name, c.max_length, c.precision, c.scale, \
                    case when c.is_nullable = 1 then 'Y' else 'N' end, \
                    case when pk.column_id is null then 'N' else 'Y' end \
             from sys.columns c \
             join sys.objects o on o.object_id = c.object_id \
             join sys.schemas s on s.schema_id = o.schema_id \
             join sys.types t on t.user_type_id = c.user_type_id \
             left join (select ic.object_id, ic.column_id \
                        from sys.index_columns ic \
                        join sys.key_constraints kc \
                          on kc.parent_object_id = ic.object_id \
                         and kc.unique_index_id = ic.index_id \
                        where kc.type = 'PK') pk \
                    on pk.object_id = c.object_id and pk.column_id = c.column_id \
             where o.object_id = (select top 1 o2.object_id from sys.objects o2 \
                                  join sys.schemas s2 on s2.schema_id = o2.schema_id \
                                  where lower(s2.name) = lower({schema}) \
                                    and lower(o2.name) = lower({table}) \
                                  order by case when s2.name = {schema} \
                                                 and o2.name = {table} then 0 else 1 end) \
             order by c.column_id",
            schema = quoted(schema),
            table = quoted(table),
        ),
        Kind::Oracle => format!(
            "select c.column_name, c.data_type, c.data_length, c.data_precision, \
                    c.data_scale, c.char_used, c.char_length, c.nullable, \
                    case when pk.column_name is null then 'N' else 'Y' end \
             from all_tab_columns c \
             left join (select cc.owner, cc.table_name, cc.column_name \
                        from all_constraints k \
                        join all_cons_columns cc \
                          on cc.owner = k.owner and cc.constraint_name = k.constraint_name \
                        where k.constraint_type = 'P') pk \
                    on pk.owner = c.owner and pk.table_name = c.table_name \
                   and pk.column_name = c.column_name \
             where c.owner = {schema} and c.table_name = {table} \
             order by c.column_id",
            schema = owner(schema),
            table = object(schema, table),
        ),
    }
}

fn parse_columns(backend: Kind, rows: &[Vec<Cell>]) -> Vec<ColumnInfo> {
    rows.iter()
        .map(|row| match backend {
            Kind::Mssql => ColumnInfo {
                name: text(row, 0),
                type_text: mssql_type_text(&text(row, 1), int(row, 2), int(row, 3), int(row, 4)),
                nullable: text(row, 5) == "Y",
                is_pk: text(row, 6) == "Y",
            },
            Kind::Oracle => ColumnInfo {
                name: text(row, 0),
                type_text: oracle_type_text(
                    &text(row, 1),
                    int(row, 2),
                    maybe(row, 3).and_then(|text| text.parse().ok()),
                    maybe(row, 4).and_then(|text| text.parse().ok()),
                    &text(row, 5),
                    int(row, 6),
                ),
                nullable: text(row, 7) == "Y",
                is_pk: text(row, 8) == "Y",
            },
        })
        .collect()
}

/// The text that made an object. A table's is written from its columns,
/// since neither server keeps the statement that created one.
pub fn object_source(
    connection: &Connection,
    schema: &str,
    name: &str,
    kind: ObjectKind,
) -> Result<String, DbError> {
    if kind == ObjectKind::Sequence {
        return Err(DbError::Unsupported(format!("a {kind} has no source text")));
    }
    let backend = connection.kind();
    if kind == ObjectKind::Table {
        let columns = list_columns(connection, schema, name)?;
        return table_ddl(&fold(backend, schema), &fold(backend, name), &columns);
    }
    let rows = rows(connection, &source_sql(backend, schema, name, kind))?;
    parse_source(backend, schema, name, &rows)
}

/// `OBJECT_DEFINITION` is the whole batch that created the object on SQL
/// Server; Oracle keeps a line per row in `ALL_SOURCE`, and a view's text in
/// `ALL_VIEWS`. Either way one query answers, so the tree can run it the way
/// it runs everything else.
fn source_sql(backend: Kind, schema: &str, name: &str, kind: ObjectKind) -> String {
    match backend {
        // Ordered so that an exact match wins over one that differs by case,
        // which needs a case-sensitive collation to happen at all.
        Kind::Mssql => format!(
            "select object_definition(o.object_id) \
             from sys.objects o \
             join sys.schemas s on s.schema_id = o.schema_id \
             where lower(s.name) = lower({schema}) and lower(o.name) = lower({name}) \
             order by case when s.name = {schema} and o.name = {name} then 0 else 1 end",
            schema = quoted(schema),
            name = quoted(name),
        ),
        Kind::Oracle if kind == ObjectKind::View => format!(
            "select 'VIEW', text from all_views where owner = {} and view_name = {}",
            owner(schema),
            object(schema, name),
        ),
        // A package is two objects wearing one name; `PACKAGE` sorts before
        // `PACKAGE BODY`, so the spec comes back first.
        Kind::Oracle => format!(
            "select type, text from all_source \
             where owner = {} and name = {} and type in ({}) order by type, line",
            owner(schema),
            object(schema, name),
            list(kind_source_types(kind)),
        ),
    }
}

fn parse_source(
    backend: Kind,
    schema: &str,
    name: &str,
    rows: &[Vec<Cell>],
) -> Result<String, DbError> {
    let (schema, name) = (fold(backend, schema), fold(backend, name));
    let Some(first) = rows.first() else {
        return Err(missing(&schema, &name));
    };
    if backend == Kind::Mssql {
        // NULL is an object created `WITH ENCRYPTION`, which is not a
        // failure, just a door somebody locked.
        return Ok(
            maybe(first, 0).unwrap_or_else(|| "-- source not available (encrypted)".to_owned())
        );
    }
    if text(first, 0) == "VIEW" {
        return Ok(format!(
            "CREATE OR REPLACE VIEW {schema}.{name} AS\n{}",
            text(first, 1)
        ));
    }
    let mut parts: Vec<(String, String)> = Vec::new();
    for row in rows {
        let (object, line) = (text(row, 0), text(row, 1));
        match parts.last_mut() {
            Some((last, body)) if *last == object => body.push_str(&line),
            _ => parts.push((object, line)),
        }
    }
    Ok(parts
        .iter()
        .map(|(_, body)| format!("CREATE OR REPLACE {}", body.trim_end()))
        .collect::<Vec<_>>()
        .join("\n/\n\n"))
}

/// A `CREATE TABLE` from the columns: names, types, nullability and the
/// primary key.
///
/// ponytail: no defaults, identity, checks, foreign keys or indexes; ask
/// `sys.default_constraints` / `ALL_CONSTRAINTS` for them if the sketch
/// stops being enough. Oracle's `DBMS_METADATA.GET_DDL` has it all but
/// buries it under storage clauses, and SQL Server has no equivalent.
fn table_ddl(schema: &str, name: &str, columns: &[ColumnInfo]) -> Result<String, DbError> {
    if columns.is_empty() {
        return Err(missing(schema, name));
    }
    let mut lines: Vec<String> = columns
        .iter()
        .map(|column| {
            format!(
                "    {} {}{}",
                column.name,
                column.type_text,
                if column.nullable { "" } else { " NOT NULL" }
            )
        })
        .collect();
    let key: Vec<&str> = columns
        .iter()
        .filter(|column| column.is_pk)
        .map(|column| column.name.as_str())
        .collect();
    if !key.is_empty() {
        lines.push(format!("    PRIMARY KEY ({})", key.join(", ")));
    }
    Ok(format!(
        "CREATE TABLE {schema}.{name} (\n{}\n)",
        lines.join(",\n")
    ))
}

/// A package is stored as its spec and its body, under two `ALL_SOURCE`
/// types; everything else is one.
fn kind_source_types(kind: ObjectKind) -> &'static [&'static str] {
    match kind {
        ObjectKind::Package => &["PACKAGE", "PACKAGE BODY"],
        ObjectKind::Function => &["FUNCTION"],
        _ => &["PROCEDURE"],
    }
}

fn missing(schema: &str, name: &str) -> DbError {
    DbError::Query {
        message: format!("no such object: {schema}.{name}"),
        line: None,
    }
}

/// The `ALL_USERS` predicate for "a schema somebody here made". Oracle ships
/// forty accounts of its own and none of them are anybody's work.
const OWNERS: &str = "(oracle_maintained = 'N' \
     or username = sys_context('userenv', 'current_schema'))";

/// SQL Server `sys.types` gives a name, a length in bytes, a precision and a
/// scale; which of those belong in the printed type depends on the type, and
/// this is the rule SSMS prints by. `max_length` is -1 for `(max)` and is
/// bytes, so the two-byte-per-character types are halved.
fn mssql_type_text(name: &str, max_length: i64, precision: i64, scale: i64) -> String {
    match name {
        "nchar" | "nvarchar" => sized(name, if max_length < 0 { -1 } else { max_length / 2 }),
        "char" | "varchar" | "binary" | "varbinary" => sized(name, max_length),
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "datetime2" | "datetimeoffset" | "time" => format!("{name}({scale})"),
        _ => name.to_owned(),
    }
}

fn sized(name: &str, length: i64) -> String {
    if length < 0 {
        format!("{name}(max)")
    } else {
        format!("{name}({length})")
    }
}

/// Oracle already spells the scale into the type name for the timestamps and
/// the intervals, so `TIMESTAMP(6)` arrives whole. What it does not spell is
/// the length of a character type — where `CHAR_USED` says whether the
/// declaration counted characters or bytes, the way `DBMS_METADATA` prints
/// it — nor the precision of a `NUMBER`.
fn oracle_type_text(
    data_type: &str,
    data_length: i64,
    precision: Option<i64>,
    scale: Option<i64>,
    char_used: &str,
    char_length: i64,
) -> String {
    match data_type {
        // The national types are character-semantic by definition; Oracle
        // prints them without a unit and so does this.
        "NCHAR" | "NVARCHAR2" => format!("{data_type}({char_length})"),
        "CHAR" | "VARCHAR2" | "VARCHAR" => match char_used {
            "C" => format!("{data_type}({char_length} CHAR)"),
            _ => format!("{data_type}({data_length} BYTE)"),
        },
        "NUMBER" | "FLOAT" => match (precision, scale) {
            (None, None) => data_type.to_owned(),
            // An INTEGER column: no precision, but a scale that pins it to
            // whole numbers.
            (None, Some(scale)) => format!("{data_type}(*,{scale})"),
            (Some(precision), None | Some(0)) => format!("{data_type}({precision})"),
            (Some(precision), Some(scale)) => format!("{data_type}({precision},{scale})"),
        },
        "RAW" => format!("RAW({data_length})"),
        _ => data_type.to_owned(),
    }
}

/// Oracle folds an unquoted identifier to upper case when it stores it, so
/// that is what its catalog is asked for. SQL Server stores what it was
/// given, and the queries compare case-insensitively instead.
fn fold(backend: Kind, name: &str) -> String {
    match backend {
        Kind::Oracle => name.to_uppercase(),
        Kind::Mssql => name.to_owned(),
    }
}

/// An Oracle schema as the catalog spells it: exactly as given when a user of
/// that name exists — the tree hands back names as stored, and a quoted
/// `"MixedCase"` is stored mixed — else folded the way an unquoted name is.
fn owner(schema: &str) -> String {
    format!(
        "coalesce((select max(username) from all_users where username = {}), {})",
        quoted(schema),
        quoted(&schema.to_uppercase()),
    )
}

/// An Oracle object name, spelled the way [`owner`] spells a schema.
fn object(schema: &str, name: &str) -> String {
    format!(
        "coalesce((select max(object_name) from all_objects \
                   where owner = {} and object_name = {}), {})",
        owner(schema),
        quoted(name),
        quoted(&name.to_uppercase()),
    )
}

/// Every row of a catalog query. No cap: a schema with more than ten
/// thousand objects would be a surprise, and half a listing is worse than a
/// slow one.
fn rows(connection: &Connection, sql: &str) -> Result<Vec<Vec<Cell>>, DbError> {
    let mut rows = Vec::new();
    for event in connection.query(
        sql,
        QueryOptions {
            max_rows: None,
            ..QueryOptions::default()
        },
    ) {
        match event {
            QueryEvent::Rows(batch) => rows.extend(batch),
            QueryEvent::Error(error) => return Err(error),
            _ => {}
        }
    }
    Ok(rows)
}

/// A SQL string literal: the only way a quote gets into one is doubled.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| quoted(value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text(row: &[Cell], index: usize) -> String {
    row.get(index)
        .map(|cell| cell.display().into_owned())
        .unwrap_or_default()
}

fn maybe(row: &[Cell], index: usize) -> Option<String> {
    match row.get(index) {
        None | Some(Cell::Null) => None,
        Some(cell) => Some(cell.display().into_owned()),
    }
}

fn int(row: &[Cell], index: usize) -> i64 {
    text(row, index).parse().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_is_written_the_way_the_flag_takes_it() {
        assert_eq!("table".parse(), Ok(ObjectKind::Table));
        assert_eq!("PACKAGE".parse(), Ok(ObjectKind::Package));
        assert_eq!(ObjectKind::Sequence.to_string(), "sequence");
        let complaint = "thing".parse::<ObjectKind>().unwrap_err();
        assert_eq!(
            complaint,
            "unknown kind \"thing\"; one of table, view, procedure, function, package, sequence"
        );
    }

    #[test]
    fn every_sql_server_type_code_maps_back_to_a_kind() {
        assert_eq!(ObjectKind::from_mssql("U"), Some(ObjectKind::Table));
        assert_eq!(ObjectKind::from_mssql("V"), Some(ObjectKind::View));
        assert_eq!(ObjectKind::from_mssql("P"), Some(ObjectKind::Procedure));
        for function in ["FN", "IF", "TF"] {
            assert_eq!(ObjectKind::from_mssql(function), Some(ObjectKind::Function));
        }
        assert_eq!(ObjectKind::from_mssql("SO"), Some(ObjectKind::Sequence));
        assert_eq!(ObjectKind::from_mssql("D"), None, "a default constraint");
    }

    #[test]
    fn every_oracle_object_type_maps_back_to_a_kind() {
        assert_eq!(ObjectKind::from_oracle("TABLE"), Some(ObjectKind::Table));
        assert_eq!(
            ObjectKind::from_oracle("PACKAGE"),
            Some(ObjectKind::Package)
        );
        assert_eq!(
            ObjectKind::from_oracle("PACKAGE BODY"),
            None,
            "a body is half of the package that is already listed"
        );
    }

    #[test]
    fn a_table_is_written_as_the_create_that_would_make_it() {
        let column = |name: &str, type_text: &str, nullable, is_pk| ColumnInfo {
            name: name.to_owned(),
            type_text: type_text.to_owned(),
            nullable,
            is_pk,
        };
        let columns = [
            column("order_id", "int", false, true),
            column("line_no", "int", false, true),
            column("note", "nvarchar(max)", true, false),
        ];
        assert_eq!(
            table_ddl("dbo", "order_lines", &columns).unwrap(),
            "CREATE TABLE dbo.order_lines (\n    order_id int NOT NULL,\n    \
             line_no int NOT NULL,\n    note nvarchar(max),\n    \
             PRIMARY KEY (order_id, line_no)\n)"
        );
        assert!(table_ddl("dbo", "gone", &[]).is_err());
    }

    #[test]
    fn a_quote_in_an_identifier_is_doubled_and_not_a_way_out_of_the_literal() {
        assert_eq!(quoted("bench"), "'bench'");
        assert_eq!(quoted("o'brien"), "'o''brien'");
        assert_eq!(quoted("'; drop table x --"), "'''; drop table x --'");
    }

    #[test]
    fn a_sql_server_type_is_spelled_the_way_ssms_spells_it() {
        assert_eq!(mssql_type_text("int", 4, 10, 0), "int");
        assert_eq!(mssql_type_text("nvarchar", 200, 0, 0), "nvarchar(100)");
        assert_eq!(mssql_type_text("nvarchar", -1, 0, 0), "nvarchar(max)");
        assert_eq!(mssql_type_text("varchar", 200, 0, 0), "varchar(200)");
        assert_eq!(mssql_type_text("varbinary", -1, 0, 0), "varbinary(max)");
        assert_eq!(mssql_type_text("char", 2, 0, 0), "char(2)");
        assert_eq!(mssql_type_text("decimal", 9, 12, 2), "decimal(12,2)");
        assert_eq!(mssql_type_text("datetime2", 8, 27, 7), "datetime2(7)");
        assert_eq!(mssql_type_text("datetime", 8, 23, 3), "datetime");
    }

    #[test]
    fn an_oracle_type_is_spelled_the_way_dbms_metadata_spells_it() {
        assert_eq!(
            oracle_type_text("VARCHAR2", 200, None, None, "B", 200),
            "VARCHAR2(200 BYTE)"
        );
        assert_eq!(
            oracle_type_text("VARCHAR2", 400, None, None, "C", 100),
            "VARCHAR2(100 CHAR)"
        );
        assert_eq!(
            oracle_type_text("NVARCHAR2", 200, None, None, "C", 100),
            "NVARCHAR2(100)"
        );
        assert_eq!(
            oracle_type_text("CHAR", 2, None, None, "B", 2),
            "CHAR(2 BYTE)"
        );
        assert_eq!(
            oracle_type_text("NUMBER", 22, Some(12), Some(2), "", 0),
            "NUMBER(12,2)"
        );
        assert_eq!(
            oracle_type_text("NUMBER", 22, Some(10), Some(0), "", 0),
            "NUMBER(10)"
        );
        assert_eq!(oracle_type_text("NUMBER", 22, None, None, "", 0), "NUMBER");
        assert_eq!(
            oracle_type_text("NUMBER", 22, None, Some(0), "", 0),
            "NUMBER(*,0)"
        );
        assert_eq!(
            oracle_type_text("TIMESTAMP(6)", 11, None, Some(6), "", 0),
            "TIMESTAMP(6)"
        );
        assert_eq!(oracle_type_text("RAW", 8, None, None, "", 0), "RAW(8)");
        assert_eq!(oracle_type_text("CLOB", 4000, None, None, "", 0), "CLOB");
    }
}
