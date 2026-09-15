//! The catalog queries against both containers `scripts/db-up.sh` seeds.
//!
//! Skipped unless `SQL_BENCH_TEST_DBS` is set, like the driver tests, and
//! reading the same committed `config.local.toml`. What is asserted is what
//! the seed created: the objects of every kind, the columns of `customers`
//! down to the spelling of their types, and a line out of each procedure.

use std::path::Path;

use sql_bench::config;
use sql_bench::db::Connection;
use sql_bench::db::catalog::{self, ColumnInfo, DbObject, ObjectKind};

/// The named connection from `config.local.toml`, or nothing when the
/// databases are not wanted.
fn connect(name: &str) -> Option<Connection> {
    if std::env::var_os("SQL_BENCH_TEST_DBS").is_none() {
        eprintln!("skipped: set SQL_BENCH_TEST_DBS=1 with the containers up");
        return None;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.local.toml");
    let config = config::load(&path).expect("config.local.toml is readable");
    let spec = config
        .connection(name)
        .unwrap_or_else(|| panic!("config.local.toml names {name}"))
        .clone();
    Some(Connection::open(&spec, &config).expect("the container is up"))
}

macro_rules! connection {
    ($name:literal) => {
        match connect($name) {
            Some(connection) => connection,
            None => return,
        }
    };
}

/// The names of every object of one kind, as the listing has them.
fn named(objects: &[DbObject], kind: ObjectKind) -> Vec<&str> {
    objects
        .iter()
        .filter(|object| object.kind == kind)
        .map(|object| object.name.as_str())
        .collect()
}

/// One column, flattened to what a test cares about.
fn column(columns: &[ColumnInfo], name: &str) -> (String, bool, bool) {
    let found = columns
        .iter()
        .find(|column| column.name == name)
        .unwrap_or_else(|| panic!("no column {name} in {columns:?}"));
    (found.type_text.clone(), found.nullable, found.is_pk)
}

mod mssql {
    use super::*;

    #[test]
    fn the_seeded_schema_is_among_the_schemas() {
        let connection = connection!("local-mssql");
        let schemas = catalog::list_schemas(&connection).unwrap();
        assert!(schemas.contains(&"bench".to_owned()), "{schemas:?}");
        assert!(schemas.contains(&"dbo".to_owned()), "{schemas:?}");
        assert!(
            !schemas.iter().any(|schema| schema.starts_with("db_")),
            "the fixed database roles are not schemas anyone works in: {schemas:?}"
        );
    }

    #[test]
    fn every_kind_the_seed_created_is_listed() {
        let connection = connection!("local-mssql");
        let objects = catalog::list_objects(&connection, Some("bench"), None).unwrap();
        assert_eq!(
            named(&objects, ObjectKind::Table),
            [
                "all_types",
                "big_text",
                "binary_blobs",
                "customers",
                "events",
                "order_items",
                "orders"
            ]
        );
        assert_eq!(
            named(&objects, ObjectKind::View),
            ["v_customer_totals", "v_recent_orders"]
        );
        assert_eq!(
            named(&objects, ObjectKind::Procedure),
            ["sp_customer_orders", "sp_mark_shipped"]
        );
        assert_eq!(
            named(&objects, ObjectKind::Function),
            ["fn_order_total", "tvf_orders_by_status"],
            "a table-valued function is a function"
        );
        assert_eq!(named(&objects, ObjectKind::Sequence), ["order_seq"]);
        assert!(
            objects
                .iter()
                .all(|object| object.schema == "bench" && object.modified.is_some())
        );
    }

    #[test]
    fn a_kind_narrows_the_listing_and_a_schema_is_case_insensitive() {
        let connection = connection!("local-mssql");
        let views =
            catalog::list_objects(&connection, Some("BENCH"), Some(ObjectKind::View)).unwrap();
        assert_eq!(
            named(&views, ObjectKind::View),
            ["v_customer_totals", "v_recent_orders"]
        );
        assert_eq!(views.len(), 2, "and nothing else: {views:?}");
    }

    #[test]
    fn the_columns_of_customers_are_the_ones_the_seed_declared() {
        let connection = connection!("local-mssql");
        let columns = catalog::list_columns(&connection, "bench", "customers").unwrap();
        assert_eq!(
            columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            [
                "id",
                "name",
                "email",
                "country",
                "created_at",
                "credit_limit"
            ]
        );
        assert_eq!(column(&columns, "id"), ("int".to_owned(), false, true));
        assert_eq!(
            column(&columns, "name"),
            ("nvarchar(100)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "email"),
            ("varchar(200)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "country"),
            ("char(2)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "created_at"),
            ("datetime2(7)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "credit_limit"),
            ("decimal(12,2)".to_owned(), true, false),
            "the one nullable column"
        );
    }

    #[test]
    fn a_varbinary_max_says_max() {
        let connection = connection!("local-mssql");
        let columns = catalog::list_columns(&connection, "bench", "binary_blobs").unwrap();
        assert_eq!(
            column(&columns, "data"),
            ("varbinary(max)".to_owned(), true, false)
        );
    }

    #[test]
    fn a_procedure_comes_back_as_the_batch_that_made_it() {
        let connection = connection!("local-mssql");
        let source = catalog::object_source(
            &connection,
            "bench",
            "sp_customer_orders",
            ObjectKind::Procedure,
        )
        .unwrap();
        assert!(
            source.contains("PROCEDURE bench.sp_customer_orders @customer_id int"),
            "{source}"
        );
        assert!(
            source.contains("WHERE customer_id = @customer_id"),
            "{source}"
        );
    }

    #[test]
    fn a_function_and_a_view_have_source_too() {
        let connection = connection!("local-mssql");
        let function =
            catalog::object_source(&connection, "bench", "fn_order_total", ObjectKind::Function)
                .unwrap();
        assert!(function.contains("RETURNS decimal(12,2)"), "{function}");
        let view =
            catalog::object_source(&connection, "bench", "v_recent_orders", ObjectKind::View)
                .unwrap();
        assert!(view.contains("SELECT TOP (100)"), "{view}");
    }

    #[test]
    fn a_name_in_the_wrong_case_still_finds_the_object() {
        let connection = connection!("local-mssql");
        let source = catalog::object_source(
            &connection,
            "BENCH",
            "SP_MARK_SHIPPED",
            ObjectKind::Procedure,
        )
        .unwrap();
        assert!(
            source.contains("UPDATE bench.orders SET status"),
            "{source}"
        );
    }

    #[test]
    fn an_object_that_is_not_there_says_so() {
        let connection = connection!("local-mssql");
        let failure =
            catalog::object_source(&connection, "bench", "sp_nope", ObjectKind::Procedure)
                .unwrap_err();
        assert_eq!(failure.to_string(), "no such object: bench.sp_nope");
    }

    #[test]
    fn a_table_has_no_source_text() {
        let connection = connection!("local-mssql");
        let failure = catalog::object_source(&connection, "bench", "customers", ObjectKind::Table)
            .unwrap_err();
        assert_eq!(
            failure.to_string(),
            "not supported: a table has no source text"
        );
    }
}

mod oracle {
    use super::*;

    #[test]
    fn the_seeded_schema_is_among_the_schemas() {
        let connection = connection!("local-oracle");
        let schemas = catalog::list_schemas(&connection).unwrap();
        assert!(schemas.contains(&"BENCH".to_owned()), "{schemas:?}");
        assert!(
            !schemas.contains(&"SYS".to_owned()),
            "the accounts Oracle maintains are not anybody's work: {schemas:?}"
        );
    }

    #[test]
    fn every_kind_the_seed_created_is_listed() {
        let connection = connection!("local-oracle");
        let objects = catalog::list_objects(&connection, Some("BENCH"), None).unwrap();
        assert_eq!(
            named(&objects, ObjectKind::Table),
            [
                "ALL_TYPES",
                "BIG_TEXT",
                "BINARY_BLOBS",
                "CUSTOMERS",
                "EVENTS",
                "ORDERS",
                "ORDER_ITEMS"
            ]
        );
        assert_eq!(
            named(&objects, ObjectKind::View),
            ["V_CUSTOMER_TOTALS", "V_RECENT_ORDERS"]
        );
        assert_eq!(
            named(&objects, ObjectKind::Procedure),
            ["CUSTOMER_ORDERS", "MARK_SHIPPED"]
        );
        assert_eq!(named(&objects, ObjectKind::Function), ["ORDER_TOTAL"]);
        assert_eq!(named(&objects, ObjectKind::Package), ["ORDER_PKG"]);
        assert_eq!(named(&objects, ObjectKind::Sequence), ["ORDER_SEQ"]);
        assert!(
            objects
                .iter()
                .all(|object| object.schema == "BENCH" && object.modified.is_some())
        );
    }

    #[test]
    fn a_schema_in_the_wrong_case_is_the_same_schema() {
        let connection = connection!("local-oracle");
        let packages =
            catalog::list_objects(&connection, Some("bench"), Some(ObjectKind::Package)).unwrap();
        assert_eq!(named(&packages, ObjectKind::Package), ["ORDER_PKG"]);
        assert_eq!(
            packages.len(),
            1,
            "a package body is half of the package, not a second object: {packages:?}"
        );
    }

    #[test]
    fn the_columns_of_customers_are_the_ones_the_seed_declared() {
        let connection = connection!("local-oracle");
        let columns = catalog::list_columns(&connection, "bench", "customers").unwrap();
        assert_eq!(
            columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            [
                "ID",
                "NAME",
                "EMAIL",
                "COUNTRY",
                "CREATED_AT",
                "CREDIT_LIMIT"
            ]
        );
        assert_eq!(
            column(&columns, "ID"),
            ("NUMBER(10)".to_owned(), false, true)
        );
        assert_eq!(
            column(&columns, "NAME"),
            ("NVARCHAR2(100)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "EMAIL"),
            ("VARCHAR2(200 BYTE)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "COUNTRY"),
            ("CHAR(2 BYTE)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "CREATED_AT"),
            ("TIMESTAMP(6)".to_owned(), false, false)
        );
        assert_eq!(
            column(&columns, "CREDIT_LIMIT"),
            ("NUMBER(12,2)".to_owned(), true, false),
            "the one nullable column"
        );
    }

    #[test]
    fn a_procedure_and_a_function_come_back_from_all_source() {
        let connection = connection!("local-oracle");
        let procedure = catalog::object_source(
            &connection,
            "bench",
            "customer_orders",
            ObjectKind::Procedure,
        )
        .unwrap();
        assert!(
            procedure.starts_with("CREATE OR REPLACE PROCEDURE customer_orders"),
            "{procedure}"
        );
        assert!(
            procedure.contains("WHERE customer_id = p_customer_id"),
            "{procedure}"
        );

        let function =
            catalog::object_source(&connection, "BENCH", "ORDER_TOTAL", ObjectKind::Function)
                .unwrap();
        assert!(function.contains("RETURN NUMBER IS"), "{function}");
    }

    #[test]
    fn a_package_comes_back_as_its_spec_and_then_its_body() {
        let connection = connection!("local-oracle");
        let source =
            catalog::object_source(&connection, "bench", "order_pkg", ObjectKind::Package).unwrap();
        let spec = source
            .find("CREATE OR REPLACE PACKAGE order_pkg AS")
            .unwrap_or_else(|| panic!("no spec in {source}"));
        let body = source
            .find("CREATE OR REPLACE PACKAGE BODY order_pkg AS")
            .unwrap_or_else(|| panic!("no body in {source}"));
        assert!(spec < body, "the spec comes first");
        assert!(
            source[spec..body].contains("\n/\n"),
            "and a divider comes between them: {source}"
        );
        assert!(source.contains("SELECT COUNT(*) INTO v_count"), "{source}");
    }

    #[test]
    fn a_view_comes_back_from_its_long_column() {
        let connection = connection!("local-oracle");
        let source =
            catalog::object_source(&connection, "bench", "v_recent_orders", ObjectKind::View)
                .unwrap();
        assert!(source.contains("FETCH FIRST 100 ROWS ONLY"), "{source}");
    }

    #[test]
    fn an_object_that_is_not_there_says_so() {
        let connection = connection!("local-oracle");
        let failure = catalog::object_source(&connection, "bench", "nope", ObjectKind::Procedure)
            .unwrap_err();
        assert_eq!(failure.to_string(), "no such object: BENCH.NOPE");
    }
}
