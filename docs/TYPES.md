# What a database type becomes

The driver conformance table *(T2.5)*: every column type the two servers can
send, the [`Cell`](../src/db/model.rs) variant it arrives as, and what that
cell reads as on screen.

Both tables are real output, not a design. The types are what
`sql-bench source --conn <conn> <table>` reports (`ALL_TAB_COLUMNS` /
`sys.columns`, the same query the Objects pane uses); the examples are the
row `scripts/seed` writes into `bench.all_types` — except SQL Server's
`nvarchar(max)` and `varbinary(max)`, which that table has no column for:
those two are row 1 of `bench.big_text` and of `bench.binary_blobs`. Printed
with

```
SQL_BENCH_CONFIG=config.local.toml sql-bench query --conn local-mssql \
    --format table --full 'select * from bench.all_types'
```

and the `Cell` variants are the ones `every_type_the_server_has_maps_to_a_cell`
in `tests/mssql.rs` and `tests/oracle.rs` assert against the same row.

## SQL Server

| database type | `Cell` | example display |
|---|---|---|
| `int` | `Int` | `2147483647` |
| `bit` | `Bool` | `true` |
| `tinyint` | `Int` | `255` |
| `smallint` | `Int` | `32767` |
| `bigint` | `Int` | `9223372036854775807` |
| `decimal(18,4)` | `Decimal` | `12345.6789` |
| `numeric(10,2)` | `Decimal` | `123.45` |
| `money` | `Decimal` | `1234.5678` |
| `float` | `Float` | `12345678901.234` |
| `real` | `Float` | `1.25` |
| `char(5)` | `Text` | `abcde` |
| `varchar(20)` | `Text` | `varchar value` |
| `nvarchar(20)` | `Text` | `nvarchar value` |
| `nvarchar(max)` | `Text` | `short body` |
| `text` | `Text` | `text value` |
| `date` | `DateTime` | `2024-05-17` |
| `time(7)` | `DateTime` | `13:45:30.1234567` |
| `datetime` | `DateTime` | `2024-05-17T13:45:30.000` |
| `datetime2(7)` | `DateTime` | `2024-05-17T13:45:30.1234567` |
| `datetimeoffset(7)` | `DateTime` | `2024-05-17T13:45:30.1234567+02:00` |
| `uniqueidentifier` | `Text` | `6f9619ff-8b86-d011-b42d-00c04fc964ff` |
| `varbinary(8)` | `Bytes` | `0x0102030405060708` |
| `varbinary(max)` | `Bytes` | `0x0102030405` |
| `xml` | `Text` | `<root><a id="1">x</a></root>` |

The grid's own column header shows the type the wire reports, which is the
declared type without its width: `decimal`, `time`, `varbinary`. TDS lumps
every width of an integer into one wire type, so `tinyint`, `smallint` and
`int` all arrive as `int` there and only the value says which it was.

## Oracle

| database type | `Cell` | example display |
|---|---|---|
| `NUMBER(10)` | `Int` | `1` |
| `NUMBER` | `Decimal` | `12345.6789` |
| `NUMBER(10,2)` | `Decimal` | `123.45` |
| `BINARY_FLOAT` | `Float` | `1.25` |
| `BINARY_DOUBLE` | `Float` | `1.2345678901234` |
| `CHAR(5 BYTE)` | `Text` | `abcde` |
| `VARCHAR2(20 BYTE)` | `Text` | `varchar2 value` |
| `NVARCHAR2(20)` | `Text` | `nvarchar2 value` |
| `DATE` | `DateTime` | `2024-05-17 00:00:00` |
| `TIMESTAMP(6)` | `DateTime` | `2024-05-17 13:45:30.123456` |
| `TIMESTAMP(6) WITH TIME ZONE` | `DateTime` | `2024-05-17 13:45:30.123456 +02:00` |
| `INTERVAL DAY(2) TO SECOND(6)` | `Text` | `+02 03:04:05.600000` |
| `RAW(8)` | `Bytes` | `0x0102030405060708` |
| `CLOB` | `Text` | `clob value` |
| `BLOB` | `Bytes` | `0xaabbcc` |

A `NUMBER` of 1 to 18 digits and no scale is an `Int`; anything wider, or
with a scale, is a `Decimal` carrying the server's own digits. An unqualified
`NUMBER` is 38 digits, so `select 1 from dual` is a `Decimal("1")` and not an
`Int` — the literal was never declared narrow enough to be one.

Oracle upper-cases an unquoted column name, so `select 1 as one from dual`
comes back as `ONE`. A quoted name keeps its case and may hold anything,
spaces and non-ASCII letters included (`"Größe des Kunden"`).

## The same cell in every format

`Cell::display` is the one text form, so a value reads the same in the grid
as in an export. The three formats differ only where the format has a word
of its own:

| | `--format table` | `--format csv` | `--format json` |
|---|---|---|---|
| `Null` | `NULL` | *(empty field)* | `null` |
| `Int`, `Float` | the digits | the digits | a JSON number |
| `Decimal` | the digits | the digits | a **string**, so no digit is rounded away — unless it is a whole number a double holds exactly (up to 2^53), like Oracle's `count(*)`, which is a JSON number |
| `Bool` | `true` | `true` | `true`, a JSON boolean |
| `Bytes` | `0x…` | `0x…` | `"0x…"` |
| a repeated column name | both columns, twice the header | both columns, twice the header | `a`, then `a_2`: a JSON object cannot hold one name twice without losing a column |

Without `--full` a cell is cut at 60 terminal columns and the last of them
is `…`. Columns and not characters, because a CJK glyph is drawn two cells
wide: `李雷` is four columns in a table and in the grid alike *(T5.4)*.

## Ceilings

An Oracle `CLOB`, `NCLOB` or `BLOB` is read through a locator and stops at
1 MiB (`LOB_LIMIT` in `src/db/oracle.rs`); a cut `CLOB` ends in `…`. SQL
Server has no locator in this driver: `nvarchar(max)` and `varbinary(max)`
come down the wire whole, so a row holding a gigabyte would be a gigabyte in
this process. Nothing in the seed is near either ceiling — the 100 KB body in
`bench.big_text` arrives whole on both — and raising the SQL Server side
would mean a memory budget, not just a constant.
