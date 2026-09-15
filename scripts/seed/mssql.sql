/*
  sql-bench seed data -- SQL Server dialect. Re-runnable: every CREATE is
  guarded and the inserts are skipped once bench.customers has rows.

  Licensing: the container this loads into runs with MSSQL_PID=Developer,
  the free SQL Server Developer edition, licensed by Microsoft for
  development and test use only -- never for production.
*/
SET NOCOUNT ON;
GO

IF DB_ID('bench') IS NULL CREATE DATABASE bench;
GO
ALTER DATABASE bench SET RECOVERY SIMPLE;
GO
USE bench;
GO
IF SCHEMA_ID('bench') IS NULL EXEC('CREATE SCHEMA bench');
GO

------------------------------------------------------------------ tables
IF OBJECT_ID('bench.customers') IS NULL
CREATE TABLE bench.customers (
    id           int           NOT NULL PRIMARY KEY,
    name         nvarchar(100) NOT NULL,
    email        varchar(200)  NOT NULL UNIQUE,
    country      char(2)       NOT NULL,
    created_at   datetime2     NOT NULL,
    credit_limit decimal(12,2) NULL
);
GO
IF OBJECT_ID('bench.orders') IS NULL
CREATE TABLE bench.orders (
    id          int           NOT NULL PRIMARY KEY,
    customer_id int           NOT NULL REFERENCES bench.customers(id),
    ordered_at  datetime2     NOT NULL,
    status      varchar(20)   NOT NULL,
    total       decimal(12,2) NOT NULL
);
GO
IF OBJECT_ID('bench.order_items') IS NULL
CREATE TABLE bench.order_items (
    id         int           NOT NULL PRIMARY KEY,
    order_id   int           NOT NULL REFERENCES bench.orders(id),
    sku        varchar(40)   NOT NULL,
    qty        int           NOT NULL,
    unit_price decimal(10,2) NOT NULL
);
GO
IF OBJECT_ID('bench.events') IS NULL
CREATE TABLE bench.events (
    id          bigint       NOT NULL PRIMARY KEY,
    customer_id int          NOT NULL,
    kind        varchar(30)  NOT NULL,
    payload     varchar(200) NOT NULL,
    at          datetime2    NOT NULL
);
GO
IF OBJECT_ID('bench.big_text') IS NULL
CREATE TABLE bench.big_text (
    id   int NOT NULL PRIMARY KEY,
    body nvarchar(max) NULL
);
GO
IF OBJECT_ID('bench.binary_blobs') IS NULL
CREATE TABLE bench.binary_blobs (
    id   int NOT NULL PRIMARY KEY,
    data varbinary(max) NULL
);
GO
IF OBJECT_ID('bench.all_types') IS NULL
CREATE TABLE bench.all_types (
    id                 int NOT NULL PRIMARY KEY,
    c_bit              bit NULL,
    c_tinyint          tinyint NULL,
    c_smallint         smallint NULL,
    c_int              int NULL,
    c_bigint           bigint NULL,
    c_decimal          decimal(18,4) NULL,
    c_numeric          numeric(10,2) NULL,
    c_money            money NULL,
    c_float            float NULL,
    c_real             real NULL,
    c_char             char(5) NULL,
    c_varchar          varchar(20) NULL,
    c_nvarchar         nvarchar(20) NULL,
    c_text             text NULL,
    c_date             date NULL,
    c_time             time NULL,
    c_datetime         datetime NULL,
    c_datetime2        datetime2 NULL,
    c_datetimeoffset   datetimeoffset NULL,
    c_uniqueidentifier uniqueidentifier NULL,
    c_varbinary        varbinary(8) NULL,
    c_xml              xml NULL
);
GO

------------------------------------------------------------------- data
IF NOT EXISTS (SELECT 1 FROM bench.customers)
BEGIN
    ;WITH n AS (
        SELECT TOP (50) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects
    )
    INSERT bench.customers (id, name, email, country, created_at, credit_limit)
    SELECT i,
           CHOOSE(i % 10 + 1, N'山田太郎', N'Zoë Bauer', N'Ægir Nilsen', N'李雷',
                  N'José Álvarez', N'Björk Þórsdóttir', N'Анна Иванова',
                  N'محمد الفارسي', N'Mary O''Neill', N'Šimon Novák'),
           CONCAT('customer', i, '@example.com'),
           CHOOSE(i % 10 + 1, 'JP', 'DE', 'NO', 'CN', 'ES', 'IS', 'RU', 'SA', 'IE', 'CZ'),
           DATEADD(day, i, CAST('2024-01-01T08:30:00' AS datetime2)),
           CASE WHEN i % 7 = 0 THEN NULL ELSE 100.50 * i END
    FROM n;
END;
GO

IF NOT EXISTS (SELECT 1 FROM bench.orders)
BEGIN
    ;WITH n AS (
        SELECT TOP (500) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects
    )
    INSERT bench.orders (id, customer_id, ordered_at, status, total)
    SELECT i,
           (i - 1) % 50 + 1,
           DATEADD(hour, i, CAST('2024-03-01T09:00:00' AS datetime2)),
           CHOOSE(i % 4 + 1, 'NEW', 'PAID', 'SHIPPED', 'CANCELLED'),
           3.25 * i
    FROM n;
END;
GO

IF NOT EXISTS (SELECT 1 FROM bench.order_items)
BEGIN
    ;WITH n AS (
        SELECT TOP (2000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects
    )
    INSERT bench.order_items (id, order_id, sku, qty, unit_price)
    SELECT i,
           (i - 1) % 500 + 1,
           CONCAT('SKU-', RIGHT(CONCAT('00000', i), 5)),
           i % 5 + 1,
           9.99 + i % 20
    FROM n;
END;
GO

-- 1,000,000 rows from a cross-joined tally; ~10^4 x 10^2 rows.
IF NOT EXISTS (SELECT 1 FROM bench.events)
BEGIN
    ;WITH e1(n) AS (SELECT 1 UNION ALL SELECT 1 UNION ALL SELECT 1 UNION ALL SELECT 1 UNION ALL
                    SELECT 1 UNION ALL SELECT 1 UNION ALL SELECT 1 UNION ALL SELECT 1 UNION ALL
                    SELECT 1 UNION ALL SELECT 1),
          e2(n) AS (SELECT 1 FROM e1 a CROSS JOIN e1 b),
          e4(n) AS (SELECT 1 FROM e2 a CROSS JOIN e2 b),
          nums AS (SELECT TOP (1000000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i
                   FROM e4 a CROSS JOIN e2 b)
    INSERT bench.events WITH (TABLOCK) (id, customer_id, kind, payload, at)
    SELECT i,
           i % 50 + 1,
           CHOOSE(i % 5 + 1, 'login', 'view', 'click', 'purchase', 'logout'),
           CONCAT('payload for event ', i),
           DATEADD(second, i, CAST('2024-01-01T00:00:00' AS datetime2))
    FROM nums;
END;
GO

IF NOT EXISTS (SELECT 1 FROM bench.big_text)
BEGIN
    INSERT bench.big_text (id, body) VALUES
        (1, N'short body'),
        (2, NULL),
        (3, REPLICATE(CAST(N'Lorem ipsu' AS nvarchar(max)), 10240));  -- 102400 chars
END;
GO

IF NOT EXISTS (SELECT 1 FROM bench.binary_blobs)
BEGIN
    INSERT bench.binary_blobs (id, data) VALUES
        (1, 0x0102030405),
        (2, CONVERT(varbinary(max), REPLICATE(CAST('AB' AS varchar(max)), 1000)));
END;
GO

IF NOT EXISTS (SELECT 1 FROM bench.all_types)
BEGIN
    INSERT bench.all_types VALUES
        (1, 1, 255, 32767, 2147483647, 9223372036854775807,
         12345.6789, 123.45, 1234.5678, 1.2345678901234e10, 1.25,
         'abcde', 'varchar value', N'nvarchar value', 'text value',
         '2024-05-17', '13:45:30.1234567', '2024-05-17T13:45:30',
         '2024-05-17T13:45:30.1234567', '2024-05-17T13:45:30.1234567+02:00',
         '6F9619FF-8B86-D011-B42D-00C04FC964FF', 0x0102030405060708,
         '<root><a id="1">x</a></root>'),
        (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
         NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
END;
GO

--------------------------------------------------- views, procs, functions
CREATE OR ALTER VIEW bench.v_customer_totals AS
SELECT c.id AS customer_id,
       c.name,
       COUNT(o.id) AS order_count,
       ISNULL(SUM(o.total), 0) AS total_amount
FROM bench.customers c
LEFT JOIN bench.orders o ON o.customer_id = c.id
GROUP BY c.id, c.name;
GO

CREATE OR ALTER VIEW bench.v_recent_orders AS
SELECT TOP (100) o.id, o.ordered_at, o.status, o.total, c.name AS customer_name
FROM bench.orders o
JOIN bench.customers c ON c.id = o.customer_id
ORDER BY o.ordered_at DESC, o.id DESC;
GO

CREATE OR ALTER PROCEDURE bench.sp_customer_orders @customer_id int AS
BEGIN
    SET NOCOUNT ON;
    SELECT id, ordered_at, status, total
    FROM bench.orders
    WHERE customer_id = @customer_id
    ORDER BY ordered_at;
END;
GO

CREATE OR ALTER PROCEDURE bench.sp_mark_shipped @order_id int AS
BEGIN
    SET NOCOUNT ON;
    UPDATE bench.orders SET status = 'SHIPPED' WHERE id = @order_id;
END;
GO

CREATE OR ALTER FUNCTION bench.fn_order_total (@order_id int)
RETURNS decimal(12,2) AS
BEGIN
    RETURN (SELECT ISNULL(SUM(qty * unit_price), 0)
            FROM bench.order_items WHERE order_id = @order_id);
END;
GO

CREATE OR ALTER FUNCTION bench.tvf_orders_by_status (@status varchar(20))
RETURNS TABLE AS
RETURN SELECT id, customer_id, ordered_at, total
       FROM bench.orders WHERE status = @status;
GO
