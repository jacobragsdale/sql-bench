/*
  sql-bench hand-testing demo -- SQL Server. A small online shop spread over
  six schemas: sales, inventory, hr, finance, audit, reporting. Dropped and
  rebuilt on every run by scripts/demo-up.sh; the test fixture in the bench
  database is not touched.
*/
SET NOCOUNT ON;
SET QUOTED_IDENTIFIER ON;
SET ANSI_NULLS ON;
GO
USE master;
GO
IF DB_ID('shop') IS NOT NULL
BEGIN
    ALTER DATABASE shop SET SINGLE_USER WITH ROLLBACK IMMEDIATE;
    DROP DATABASE shop;
END;
GO
CREATE DATABASE shop;
GO
ALTER DATABASE shop SET RECOVERY SIMPLE;
GO
USE shop;
GO
CREATE SCHEMA sales;
GO
CREATE SCHEMA inventory;
GO
CREATE SCHEMA hr;
GO
CREATE SCHEMA finance;
GO
CREATE SCHEMA audit;
GO
CREATE SCHEMA reporting;
GO

---------------------------------------------------- types and sequences
CREATE TYPE dbo.Email FROM varchar(254) NOT NULL;
GO
CREATE TYPE sales.OrderLineList AS TABLE (
    product_id int NOT NULL PRIMARY KEY,
    qty        int NOT NULL CHECK (qty > 0)
);
GO
CREATE SEQUENCE sales.order_id_seq AS int START WITH 120001 INCREMENT BY 1 CACHE 50;
CREATE SEQUENCE finance.invoice_seq AS int START WITH 950001 INCREMENT BY 1;
CREATE SEQUENCE sales.ticket_seq AS bigint START WITH 1 INCREMENT BY 10
    MINVALUE 1 MAXVALUE 999999 CYCLE;
GO

------------------------------------------------------------------ tables
CREATE TABLE dbo.numbers (n int NOT NULL CONSTRAINT pk_numbers PRIMARY KEY);

CREATE TABLE hr.departments (
    department_id        smallint     NOT NULL CONSTRAINT pk_departments PRIMARY KEY,
    name                 nvarchar(60) NOT NULL CONSTRAINT uq_departments_name UNIQUE,
    parent_department_id smallint     NULL
        CONSTRAINT fk_departments_parent REFERENCES hr.departments (department_id),
    cost_center          char(6)      NOT NULL
);

CREATE TABLE hr.employees (
    employee_id   int IDENTITY(1, 1) NOT NULL CONSTRAINT pk_employees PRIMARY KEY,
    first_name    nvarchar(50)  NOT NULL,
    last_name     nvarchar(50)  NOT NULL,
    email         dbo.Email     CONSTRAINT uq_employees_email UNIQUE,
    title         nvarchar(80)  NOT NULL,
    department_id smallint      NOT NULL
        CONSTRAINT fk_employees_department REFERENCES hr.departments (department_id),
    manager_id    int           NULL
        CONSTRAINT fk_employees_manager REFERENCES hr.employees (employee_id),
    hired_on      date          NOT NULL,
    terminated_on date          NULL,
    salary        decimal(10,2) NOT NULL CONSTRAINT ck_employees_salary CHECK (salary > 0),
    full_name AS (first_name + N' ' + last_name),
    CONSTRAINT ck_employees_dates CHECK (terminated_on IS NULL OR terminated_on >= hired_on)
);

CREATE TABLE hr.salary_history (
    history_id   int IDENTITY NOT NULL CONSTRAINT pk_salary_history PRIMARY KEY,
    employee_id  int           NOT NULL
        CONSTRAINT fk_salary_history_employee REFERENCES hr.employees (employee_id),
    effective_on date          NOT NULL,
    salary       decimal(10,2) NOT NULL,
    reason       varchar(20)   NOT NULL CONSTRAINT ck_salary_history_reason
        CHECK (reason IN ('hire', 'merit', 'promotion', 'market', 'correction'))
);

CREATE TABLE inventory.categories (
    category_id int IDENTITY NOT NULL CONSTRAINT pk_categories PRIMARY KEY,
    parent_id   int NULL CONSTRAINT fk_categories_parent REFERENCES inventory.categories (category_id),
    name        nvarchar(80) NOT NULL,
    CONSTRAINT uq_categories_parent_name UNIQUE (parent_id, name)
);

CREATE TABLE inventory.suppliers (
    supplier_id   int IDENTITY NOT NULL CONSTRAINT pk_suppliers PRIMARY KEY,
    name          nvarchar(120) NOT NULL CONSTRAINT uq_suppliers_name UNIQUE,
    country       char(2)  NOT NULL,
    contact_email dbo.Email,
    rating        tinyint  NULL CONSTRAINT ck_suppliers_rating CHECK (rating BETWEEN 1 AND 5),
    is_active     bit      NOT NULL CONSTRAINT df_suppliers_active DEFAULT 1
);

CREATE TABLE inventory.products (
    product_id   int IDENTITY NOT NULL CONSTRAINT pk_products PRIMARY KEY,
    sku          varchar(20)   NOT NULL CONSTRAINT uq_products_sku UNIQUE,
    name         nvarchar(120) NOT NULL,
    category_id  int NOT NULL CONSTRAINT fk_products_category REFERENCES inventory.categories (category_id),
    supplier_id  int NOT NULL CONSTRAINT fk_products_supplier REFERENCES inventory.suppliers (supplier_id),
    list_price   decimal(10,2) NOT NULL CONSTRAINT ck_products_price CHECK (list_price >= 0),
    unit_cost    decimal(10,2) NOT NULL CONSTRAINT ck_products_cost CHECK (unit_cost >= 0),
    weight_kg    decimal(7,3)  NULL,
    discontinued bit NOT NULL CONSTRAINT df_products_discontinued DEFAULT 0,
    attributes   nvarchar(max) NULL CONSTRAINT ck_products_attributes_json
        CHECK (attributes IS NULL OR ISJSON(attributes) = 1),
    created_at   datetime2(0) NOT NULL CONSTRAINT df_products_created DEFAULT SYSUTCDATETIME(),
    margin_pct AS (CASE WHEN list_price = 0 THEN NULL
                        ELSE CAST((list_price - unit_cost) * 100 / list_price AS decimal(5,1)) END),
    row_version  rowversion
);

CREATE TABLE inventory.warehouses (
    warehouse_id smallint     NOT NULL CONSTRAINT pk_warehouses PRIMARY KEY,
    code         char(3)      NOT NULL CONSTRAINT uq_warehouses_code UNIQUE,
    city         nvarchar(60) NOT NULL,
    country      char(2)      NOT NULL,
    opened_on    date         NOT NULL,
    capacity_m3  int          NOT NULL
);

CREATE TABLE inventory.stock (
    warehouse_id  smallint NOT NULL CONSTRAINT fk_stock_warehouse REFERENCES inventory.warehouses (warehouse_id),
    product_id    int      NOT NULL CONSTRAINT fk_stock_product REFERENCES inventory.products (product_id),
    on_hand       int      NOT NULL CONSTRAINT df_stock_on_hand DEFAULT 0
                                    CONSTRAINT ck_stock_on_hand CHECK (on_hand >= 0),
    reserved      int      NOT NULL CONSTRAINT df_stock_reserved DEFAULT 0
                                    CONSTRAINT ck_stock_reserved CHECK (reserved >= 0),
    reorder_level int      NOT NULL CONSTRAINT df_stock_reorder DEFAULT 10,
    updated_at    datetime2(0) NOT NULL CONSTRAINT df_stock_updated DEFAULT SYSUTCDATETIME(),
    available AS (on_hand - reserved),
    CONSTRAINT pk_stock PRIMARY KEY (warehouse_id, product_id)
);

CREATE TABLE inventory.stock_movements (
    movement_id  bigint IDENTITY NOT NULL CONSTRAINT pk_stock_movements PRIMARY KEY,
    warehouse_id smallint    NOT NULL,
    product_id   int         NOT NULL,
    qty          int         NOT NULL CONSTRAINT ck_movements_qty CHECK (qty <> 0),
    reason       varchar(12) NOT NULL CONSTRAINT ck_movements_reason
        CHECK (reason IN ('receipt', 'sale', 'return', 'adjustment', 'transfer')),
    order_id     int         NULL,
    moved_at     datetime2(0) NOT NULL CONSTRAINT df_movements_moved DEFAULT SYSUTCDATETIME(),
    CONSTRAINT fk_movements_stock FOREIGN KEY (warehouse_id, product_id)
        REFERENCES inventory.stock (warehouse_id, product_id)
);

CREATE TABLE sales.customers (
    customer_id      int IDENTITY NOT NULL CONSTRAINT pk_customers PRIMARY KEY,
    first_name       nvarchar(50) NOT NULL,
    last_name        nvarchar(50) NOT NULL,
    email            dbo.Email    CONSTRAINT uq_customers_email UNIQUE,
    phone            varchar(20)  NULL,
    tier             varchar(10)  NOT NULL CONSTRAINT df_customers_tier DEFAULT 'bronze'
        CONSTRAINT ck_customers_tier CHECK (tier IN ('bronze', 'silver', 'gold', 'platinum')),
    marketing_opt_in bit          NOT NULL CONSTRAINT df_customers_opt_in DEFAULT 0,
    created_at       datetime2(0) NOT NULL CONSTRAINT df_customers_created DEFAULT SYSUTCDATETIME(),
    notes            nvarchar(max) NULL
);

CREATE TABLE sales.addresses (
    address_id  int IDENTITY NOT NULL CONSTRAINT pk_addresses PRIMARY KEY,
    customer_id int NOT NULL CONSTRAINT fk_addresses_customer
        REFERENCES sales.customers (customer_id) ON DELETE CASCADE,
    kind        varchar(8)    NOT NULL CONSTRAINT ck_addresses_kind CHECK (kind IN ('billing', 'shipping')),
    line1       nvarchar(120) NOT NULL,
    city        nvarchar(60)  NOT NULL,
    region      nvarchar(60)  NULL,
    postal_code varchar(12)   NOT NULL,
    country     char(2)       NOT NULL,
    is_default  bit           NOT NULL CONSTRAINT df_addresses_default DEFAULT 0
);

CREATE TABLE sales.promotions (
    promo_code  varchar(20)   NOT NULL CONSTRAINT pk_promotions PRIMARY KEY,
    description nvarchar(200) NOT NULL,
    pct_off     decimal(5,2)  NOT NULL CONSTRAINT ck_promotions_pct CHECK (pct_off > 0 AND pct_off <= 90),
    starts_on   date          NOT NULL,
    ends_on     date          NOT NULL,
    CONSTRAINT ck_promotions_window CHECK (ends_on >= starts_on)
);

CREATE TABLE sales.orders (
    order_id        int NOT NULL CONSTRAINT pk_orders PRIMARY KEY
                    CONSTRAINT df_orders_id DEFAULT (NEXT VALUE FOR sales.order_id_seq),
    customer_id     int NOT NULL CONSTRAINT fk_orders_customer REFERENCES sales.customers (customer_id),
    sales_rep_id    int NULL CONSTRAINT fk_orders_sales_rep REFERENCES hr.employees (employee_id),
    ship_address_id int NULL CONSTRAINT fk_orders_address REFERENCES sales.addresses (address_id),
    warehouse_id    smallint NOT NULL CONSTRAINT fk_orders_warehouse REFERENCES inventory.warehouses (warehouse_id),
    promo_code      varchar(20) NULL CONSTRAINT fk_orders_promo REFERENCES sales.promotions (promo_code),
    status          varchar(10) NOT NULL CONSTRAINT df_orders_status DEFAULT 'pending'
        CONSTRAINT ck_orders_status CHECK (status IN
            ('pending', 'paid', 'picking', 'shipped', 'delivered', 'cancelled', 'refunded')),
    ordered_at      datetime2(0)  NOT NULL CONSTRAINT df_orders_ordered DEFAULT SYSUTCDATETIME(),
    shipped_at      datetime2(0)  NULL,
    subtotal        decimal(12,2) NOT NULL CONSTRAINT df_orders_subtotal DEFAULT 0,
    discount        decimal(12,2) NOT NULL CONSTRAINT df_orders_discount DEFAULT 0,
    tax             decimal(12,2) NOT NULL CONSTRAINT df_orders_tax DEFAULT 0,
    total AS (subtotal - discount + tax) PERSISTED,
    order_no AS ('SO-' + CAST(order_id AS varchar(10))),
    CONSTRAINT ck_orders_shipped CHECK (shipped_at IS NULL OR shipped_at >= ordered_at)
);

CREATE TABLE sales.order_lines (
    order_id     int NOT NULL CONSTRAINT fk_order_lines_order
                 REFERENCES sales.orders (order_id) ON DELETE CASCADE,
    line_no      smallint NOT NULL,
    product_id   int NOT NULL CONSTRAINT fk_order_lines_product REFERENCES inventory.products (product_id),
    qty          int NOT NULL CONSTRAINT ck_order_lines_qty CHECK (qty > 0),
    unit_price   decimal(10,2) NOT NULL,
    discount_pct decimal(5,2)  NOT NULL CONSTRAINT df_order_lines_discount DEFAULT 0,
    -- ISNULL makes the column NOT NULL, which the indexed view needs to SUM it.
    line_total AS ISNULL(CAST(qty * unit_price * (100 - discount_pct) / 100 AS decimal(12,2)), 0) PERSISTED,
    CONSTRAINT pk_order_lines PRIMARY KEY (order_id, line_no)
);

CREATE TABLE finance.invoices (
    invoice_id  int NOT NULL CONSTRAINT pk_invoices PRIMARY KEY
                CONSTRAINT df_invoices_id DEFAULT (NEXT VALUE FOR finance.invoice_seq),
    order_id    int NOT NULL CONSTRAINT uq_invoices_order UNIQUE
                CONSTRAINT fk_invoices_order REFERENCES sales.orders (order_id),
    issued_on   date          NOT NULL,
    due_on      date          NOT NULL,
    amount      decimal(12,2) NOT NULL,
    paid_amount decimal(12,2) NOT NULL CONSTRAINT df_invoices_paid DEFAULT 0,
    currency    char(3)       NOT NULL CONSTRAINT df_invoices_currency DEFAULT 'USD',
    CONSTRAINT ck_invoices_due CHECK (due_on >= issued_on),
    CONSTRAINT ck_invoices_paid CHECK (paid_amount BETWEEN 0 AND amount)
);

CREATE TABLE finance.payments (
    payment_id   bigint IDENTITY NOT NULL CONSTRAINT pk_payments PRIMARY KEY,
    invoice_id   int NOT NULL CONSTRAINT fk_payments_invoice REFERENCES finance.invoices (invoice_id),
    method       varchar(8)    NOT NULL CONSTRAINT ck_payments_method
        CHECK (method IN ('card', 'paypal', 'wire', 'giftcard')),
    amount       decimal(12,2) NOT NULL CONSTRAINT ck_payments_amount CHECK (amount > 0),
    paid_at      datetime2(3)  NOT NULL,
    card_last4   char(4)       NULL,
    external_ref uniqueidentifier NOT NULL CONSTRAINT df_payments_ref DEFAULT NEWID()
);

CREATE TABLE finance.exchange_rates (
    currency     char(3)       NOT NULL,
    rate_date    date          NOT NULL,
    usd_per_unit decimal(18,8) NOT NULL,
    CONSTRAINT pk_exchange_rates PRIMARY KEY (currency, rate_date)
);

CREATE TABLE audit.change_log (
    change_id   bigint IDENTITY NOT NULL CONSTRAINT pk_change_log PRIMARY KEY,
    table_name  sysname       NOT NULL,
    key_value   nvarchar(100) NOT NULL,
    column_name sysname       NULL,
    old_value   nvarchar(400) NULL,
    new_value   nvarchar(400) NULL,
    changed_by  sysname       NOT NULL CONSTRAINT df_change_log_by DEFAULT SUSER_SNAME(),
    changed_at  datetime2(3)  NOT NULL CONSTRAINT df_change_log_at DEFAULT SYSUTCDATETIME()
);

CREATE TABLE audit.ddl_events (
    event_id    int IDENTITY NOT NULL CONSTRAINT pk_ddl_events PRIMARY KEY,
    event_type  nvarchar(64)  NOT NULL,
    object_name nvarchar(256) NULL,
    login_name  sysname       NOT NULL,
    tsql        nvarchar(max) NULL,
    happened_at datetime2(3)  NOT NULL CONSTRAINT df_ddl_events_at DEFAULT SYSUTCDATETIME()
);

-- System-versioned: every change to a setting lands in app_settings_history.
CREATE TABLE dbo.app_settings (
    setting_key   varchar(60)   NOT NULL CONSTRAINT pk_app_settings PRIMARY KEY,
    setting_value nvarchar(400) NOT NULL,
    valid_from datetime2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL,
    valid_to   datetime2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL,
    PERIOD FOR SYSTEM_TIME (valid_from, valid_to)
) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.app_settings_history));

-- A heap with awkward column names, the way old imports usually look.
CREATE TABLE dbo.[Legacy Import 2019] (
    [Row #]          int           NULL,
    [Customer Name]  nvarchar(100) NULL,
    [Amount (USD)]   money         NULL,
    [Imported?]      bit           NULL,
    [Notes/Comments] nvarchar(max) NULL
);
GO

------------------------------------------------ functions the seed uses
CREATE FUNCTION sales.fn_promo_pct (@promo_code varchar(20), @on date)
RETURNS decimal(5,2)
AS
BEGIN
    RETURN ISNULL((SELECT pct_off FROM sales.promotions
                   WHERE promo_code = @promo_code AND @on BETWEEN starts_on AND ends_on), 0);
END;
GO

CREATE FUNCTION sales.fn_tax_rate (@warehouse_id smallint)
RETURNS decimal(5,4)
AS
BEGIN
    RETURN (SELECT CASE country WHEN 'US' THEN 0.0825 WHEN 'NL' THEN 0.21 WHEN 'SG' THEN 0.09 ELSE 0 END
            FROM inventory.warehouses WHERE warehouse_id = @warehouse_id);
END;
GO

CREATE FUNCTION sales.fn_customer_tier (@lifetime_value decimal(14,2))
RETURNS varchar(10)
WITH SCHEMABINDING
AS
BEGIN
    RETURN CASE WHEN @lifetime_value >= 60000 THEN 'platinum'
                WHEN @lifetime_value >= 25000 THEN 'gold'
                WHEN @lifetime_value >= 10000 THEN 'silver'
                ELSE 'bronze' END;
END;
GO

-- @order_id NULL recalculates every order; the seed uses that once.
CREATE PROCEDURE sales.usp_recalculate_order @order_id int = NULL
AS
BEGIN
    SET NOCOUNT ON;
    UPDATE o
    SET subtotal = s.subtotal,
        discount = d.discount,
        tax      = CAST((s.subtotal - d.discount) * sales.fn_tax_rate(o.warehouse_id) AS decimal(12,2))
    FROM sales.orders o
    CROSS APPLY (SELECT ISNULL(SUM(ol.line_total), 0) AS subtotal
                 FROM sales.order_lines ol WHERE ol.order_id = o.order_id) s
    CROSS APPLY (SELECT CAST(s.subtotal * sales.fn_promo_pct(o.promo_code, CAST(o.ordered_at AS date)) / 100
                             AS decimal(12,2)) AS discount) d
    WHERE @order_id IS NULL OR o.order_id = @order_id;
END;
GO

------------------------------------------------------------------- data
INSERT dbo.numbers (n)
SELECT TOP (100000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL))
FROM sys.all_objects a CROSS JOIN sys.all_objects b;

INSERT hr.departments (department_id, name, parent_department_id, cost_center) VALUES
    (1, N'Executive', NULL, 'CC1000'),
    (2, N'Sales', 1, 'CC2000'),
    (3, N'Sales EMEA', 2, 'CC2100'),
    (4, N'Sales Americas', 2, 'CC2200'),
    (5, N'Operations', 1, 'CC3000'),
    (6, N'Warehouse', 5, 'CC3100'),
    (7, N'Finance', 1, 'CC4000'),
    (8, N'Engineering', 1, 'CC5000'),
    (9, N'Customer Support', 5, 'CC3200'),
    (10, N'Marketing', 1, 'CC6000');

-- Each department head's employee_id is the department_id, so the
-- generated staff below can take their manager straight from it.
SET IDENTITY_INSERT hr.employees ON;
INSERT hr.employees (employee_id, first_name, last_name, email, title, department_id, manager_id, hired_on, salary) VALUES
    (1, N'Margaret', N'Okonkwo', 'margaret.okonkwo@shop.test', N'Chief Executive Officer', 1, NULL, '2016-02-01', 310000),
    (2, N'Daniel', N'Hernández', 'daniel.hernandez@shop.test', N'VP Sales', 2, 1, '2017-05-15', 225000),
    (3, N'Sophie', N'Laurent', 'sophie.laurent@shop.test', N'Director, Sales EMEA', 3, 2, '2018-09-03', 178000),
    (4, N'Marcus', N'Webb', 'marcus.webb@shop.test', N'Director, Sales Americas', 4, 2, '2018-11-12', 181000),
    (5, N'Yuki', N'Watanabe', 'yuki.watanabe@shop.test', N'Chief Operating Officer', 5, 1, '2016-08-22', 255000),
    (6, N'Tomás', N'Oliveira', 'tomas.oliveira@shop.test', N'Warehouse Manager', 6, 5, '2019-03-04', 98000),
    (7, N'Hannah', N'Schmidt', 'hannah.schmidt@shop.test', N'Chief Financial Officer', 7, 1, '2017-01-09', 240000),
    (8, N'Ravi', N'Krishnan', 'ravi.krishnan@shop.test', N'Chief Technology Officer', 8, 1, '2017-10-30', 262000),
    (9, N'Aoife', N'Byrne', 'aoife.byrne@shop.test', N'Support Lead', 9, 5, '2020-06-01', 89000),
    (10, N'Lena', N'Johansson', 'lena.johansson@shop.test', N'Chief Marketing Officer', 10, 1, '2018-04-16', 215000);

INSERT hr.employees (employee_id, first_name, last_name, email, title, department_id, manager_id,
                     hired_on, terminated_on, salary)
SELECT 10 + n,
       CHOOSE(n * 7 % 20 + 1, N'Olivia', N'Liam', N'Emma', N'Noah', N'Sofía', N'Mateo', N'Chloé',
              N'Lukas', N'Aiko', N'Wei', N'Priya', N'Arjun', N'Fatima', N'Omar', N'Ingrid',
              N'Björn', N'Zoë', N'Dmitri', N'Grace', N'Kwame'),
       CHOOSE(n * 3 % 20 + 1, N'Smith', N'García', N'Müller', N'Rossi', N'Tanaka', N'Chen', N'Patel',
              N'Okafor', N'Johansson', N'Kowalski', N'Nguyen', N'O''Brien', N'Dubois', N'Silva',
              N'Kim', N'Haddad', N'Novák', N'Andersen', N'Ivanova', N'Moreau'),
       CONCAT('emp', 10 + n, '@shop.test'),
       CHOOSE(d.dept - 1, N'Account Executive', N'Account Executive', N'Account Executive',
              N'Operations Analyst', N'Warehouse Associate', N'Accountant', N'Software Engineer',
              N'Support Specialist', N'Marketing Specialist'),
       d.dept,
       d.dept,
       h.hired_on,
       CASE WHEN n % 17 = 0 AND h.hired_on < '2025-06-01' THEN DATEADD(day, 300, h.hired_on) END,
       52000 + n * 1373 % 60000
FROM dbo.numbers
CROSS APPLY (SELECT CAST(n % 9 + 2 AS smallint) AS dept) d
CROSS APPLY (SELECT DATEADD(day, -(n * 37 % 2500), CAST('2026-08-01' AS date)) AS hired_on) h
WHERE n <= 50;
SET IDENTITY_INSERT hr.employees OFF;

INSERT hr.salary_history (employee_id, effective_on, salary, reason)
SELECT employee_id, hired_on,
       CASE WHEN DATEADD(year, 1, hired_on) <= '2026-09-01' THEN CAST(salary * 0.88 AS decimal(10,2)) ELSE salary END,
       'hire'
FROM hr.employees
UNION ALL
SELECT employee_id, DATEADD(year, 1, hired_on), salary,
       CASE WHEN employee_id % 4 = 0 THEN 'promotion' ELSE 'merit' END
FROM hr.employees
WHERE DATEADD(year, 1, hired_on) <= '2026-09-01';

SET IDENTITY_INSERT inventory.categories ON;
INSERT inventory.categories (category_id, parent_id, name) VALUES
    (1, NULL, N'Electronics'), (2, 1, N'Audio'), (3, 2, N'Headphones'), (4, 2, N'Speakers'),
    (5, 1, N'Computers'), (6, 5, N'Laptops'), (7, 5, N'Monitors'), (8, 5, N'Accessories'),
    (9, NULL, N'Home & Kitchen'), (10, 9, N'Cookware'), (11, 9, N'Small Appliances'), (12, 11, N'Coffee'),
    (13, NULL, N'Outdoors'), (14, 13, N'Camping'), (15, 13, N'Cycling'),
    (16, NULL, N'Books'), (17, 16, N'Fiction'), (18, 16, N'Technical');
SET IDENTITY_INSERT inventory.categories OFF;

SET IDENTITY_INSERT inventory.suppliers ON;
INSERT inventory.suppliers (supplier_id, name, country, contact_email, rating, is_active) VALUES
    (1, N'Northwind Audio', 'US', 'orders@northwind-audio.test', 5, 1),
    (2, N'Kōbe Electronics', 'JP', 'sales@kobe-elec.test', 4, 1),
    (3, N'Rhein Computing GmbH', 'DE', 'vertrieb@rhein-computing.test', 4, 1),
    (4, N'Pacific Displays', 'SG', 'b2b@pacific-displays.test', 3, 1),
    (5, N'Cable & Co', 'GB', 'hello@cableandco.test', 4, 1),
    (6, N'Casa Cucina', 'IT', 'ordini@casacucina.test', 5, 1),
    (7, N'Bean There Coffee Gear', 'NL', 'wholesale@beanthere.test', 4, 1),
    (8, N'Trailhead Outfitters', 'CA', 'supply@trailhead.test', 3, 1),
    (9, N'Vélo Parts', 'FR', 'commandes@veloparts.test', 4, 1),
    (10, N'Paperback Press', 'US', 'trade@paperback.test', 5, 1),
    (11, N'Stackwise Books', 'US', 'orders@stackwise.test', 4, 1),
    (12, N'Defunct Imports Ltd', 'GB', 'nobody@defunct.test', 1, 0);
SET IDENTITY_INSERT inventory.suppliers OFF;

SET IDENTITY_INSERT inventory.products ON;
INSERT inventory.products (product_id, sku, name, category_id, supplier_id, list_price, unit_cost,
                           weight_kg, discontinued, attributes, created_at)
SELECT n,
       CONCAT(CHOOSE(k, 'AUD', 'AUD', 'CMP', 'CMP', 'CMP', 'HOM', 'HOM', 'OUT', 'OUT', 'BKS', 'BKS'),
              '-', RIGHT(CONCAT('0000', n), 4)),
       CONCAT(CHOOSE(n % 10 + 1, N'Pro', N'Lite', N'Max', N'Mini', N'Ultra', N'Classic', N'Eco',
                     N'Smart', N'Travel', N'Studio'), N' ',
              CHOOSE(k, N'Headphones', N'Speaker', N'Laptop', N'Monitor', N'USB-C Hub', N'Skillet',
                     N'Espresso Machine', N'Tent', N'Bike Light', N'Novel', N'Programming Guide'),
              N' ', n),
       CHOOSE(k, 3, 4, 6, 7, 8, 10, 12, 14, 15, 17, 18),
       n % 12 + 1,
       p.list_price,
       CAST(p.list_price * (0.5 + n % 5 * 0.05) AS decimal(10,2)),
       CASE WHEN k < 10 THEN CAST(0.2 + n % 40 * 0.15 AS decimal(7,3)) END,
       CASE WHEN n % 23 = 0 THEN 1 ELSE 0 END,
       CASE WHEN n % 3 = 0 THEN CONCAT('{"color":"', CHOOSE(n % 5 + 1, 'black', 'white', 'silver', 'red', 'blue'),
                                       '","warranty_months":', 12 * (n % 4 + 1), '}') END,
       DATEADD(day, -(n * 3), CAST('2026-06-01T10:00:00' AS datetime2(0)))
FROM dbo.numbers
CROSS APPLY (SELECT n % 11 + 1 AS k) c
CROSS APPLY (SELECT CAST(CHOOSE(k, 149, 89, 1299, 329, 49, 59, 449, 219, 35, 18, 54)
                         * (0.8 + n % 7 * 0.1) - 0.01 AS decimal(10,2)) AS list_price) p
WHERE n <= 300;
SET IDENTITY_INSERT inventory.products OFF;

INSERT inventory.warehouses (warehouse_id, code, city, country, opened_on, capacity_m3) VALUES
    (1, 'DAL', N'Dallas', 'US', '2016-03-01', 42000),
    (2, 'RTM', N'Rotterdam', 'NL', '2019-09-15', 30000),
    (3, 'SIN', N'Singapore', 'SG', '2021-05-10', 18000),
    (4, 'CHI', N'Chicago', 'US', '2023-02-20', 25000);

INSERT inventory.stock (warehouse_id, product_id, on_hand, reorder_level)
SELECT w.warehouse_id, p.product_id,
       (p.product_id * 37 + w.warehouse_id * 11) % 120,
       10 + p.product_id % 4 * 10
FROM inventory.warehouses w CROSS JOIN inventory.products p;

INSERT sales.promotions (promo_code, description, pct_off, starts_on, ends_on) VALUES
    ('WELCOME10', N'10% off for new customers', 10, '2023-01-01', '2027-12-31'),
    ('LOYAL5', N'5% loyalty discount', 5, '2024-01-01', '2026-12-31'),
    ('SPRING24', N'Spring sale 2024', 15, '2024-03-01', '2024-05-31'),
    ('BF2024', N'Black Friday 2024', 25, '2024-11-25', '2024-12-02'),
    ('SUMMER25', N'Summer sale 2025', 12, '2025-06-01', '2025-08-31'),
    ('BF2025', N'Black Friday 2025', 30, '2025-11-24', '2025-12-01');

SET IDENTITY_INSERT sales.customers ON;
INSERT sales.customers (customer_id, first_name, last_name, email, phone, marketing_opt_in, created_at, notes)
SELECT n,
       CHOOSE(n % 20 + 1, N'Olivia', N'Liam', N'Emma', N'Noah', N'Sofía', N'Mateo', N'Chloé',
              N'Lukas', N'Aiko', N'Wei', N'Priya', N'Arjun', N'Fatima', N'Omar', N'Ingrid',
              N'Björn', N'Zoë', N'Dmitri', N'Grace', N'Kwame'),
       CHOOSE(n / 20 % 20 + 1, N'Smith', N'García', N'Müller', N'Rossi', N'Tanaka', N'Chen', N'Patel',
              N'Okafor', N'Johansson', N'Kowalski', N'Nguyen', N'O''Brien', N'Dubois', N'Silva',
              N'Kim', N'Haddad', N'Novák', N'Andersen', N'Ivanova', N'Moreau'),
       CONCAT('customer', n, '@', CHOOSE(n % 4 + 1, 'example.com', 'mail.test', 'shop.test', 'inbox.test')),
       CASE WHEN n % 6 <> 0 THEN CONCAT('+1-555-', RIGHT(CONCAT('000', n * 17 % 10000), 4)) END,
       CASE WHEN n % 3 = 0 THEN 1 ELSE 0 END,
       DATEADD(hour, n * 4, CAST('2023-01-01T09:00:00' AS datetime2(0))),
       CASE WHEN n % 50 = 0 THEN CONCAT(N'VIP — call before shipping.', NCHAR(10),
                                        N'Prefers email; allergic to spam 🙂') END
FROM dbo.numbers WHERE n <= 2000;
SET IDENTITY_INSERT sales.customers OFF;

INSERT sales.addresses (customer_id, kind, line1, city, region, postal_code, country, is_default)
SELECT c.n, k.kind,
       CONCAT(c.n * 13 % 900 + 100 + k.bump, N' ',
              CHOOSE(c.n % 8 + 1, N'Maple St', N'Oak Ave', N'Harbour Rd', N'Königstraße',
                     N'Rue de Rivoli', N'Canal St', N'Sakura-dori', N'Orchard Rd')),
       CHOOSE(c.n % 10 + 1, N'Austin', N'Toronto', N'London', N'Berlin', N'Lyon', N'Utrecht',
              N'Osaka', N'Singapore', N'Melbourne', N'São Paulo'),
       CHOOSE(c.n % 10 + 1, N'TX', N'ON', NULL, N'BE', NULL, N'UT', NULL, NULL, N'VIC', N'SP'),
       RIGHT(CONCAT('0000', c.n * 7717 % 99999), 5),
       CHOOSE(c.n % 10 + 1, 'US', 'CA', 'GB', 'DE', 'FR', 'NL', 'JP', 'SG', 'AU', 'BR'),
       k.is_default
FROM dbo.numbers c
CROSS JOIN (VALUES ('billing', 0, 1), ('shipping', 0, 1), ('shipping', 7, 0)) k (kind, bump, is_default)
WHERE c.n <= 2000 AND (k.is_default = 1 OR c.n % 5 = 0);

-- 20,000 orders, one every 71 minutes from 2024-01-01; a third of them go
-- to the first 200 customers, so tiers come out uneven.
WITH reps AS (
    SELECT employee_id, ROW_NUMBER() OVER (ORDER BY employee_id) - 1 AS rn, COUNT(*) OVER () AS cnt
    FROM hr.employees WHERE department_id IN (2, 3, 4) AND terminated_on IS NULL
), x AS (
    SELECT n,
           CASE WHEN n % 3 = 0 THEN n * 7919 % 200 + 1 ELSE n * 7919 % 2000 + 1 END AS customer_id,
           CAST(n % 4 + 1 AS smallint) AS warehouse_id,
           DATEADD(minute, n * 71, CAST('2024-01-01T08:00:00' AS datetime2(0))) AS ordered_at,
           CASE WHEN n > 19950 THEN 'pending' WHEN n > 19900 THEN 'paid' WHEN n > 19850 THEN 'picking'
                WHEN n > 19700 THEN 'shipped' WHEN n % 97 = 0 THEN 'refunded'
                WHEN n % 29 = 0 THEN 'cancelled' ELSE 'delivered' END AS status
    FROM dbo.numbers WHERE n <= 20000
)
INSERT sales.orders (order_id, customer_id, sales_rep_id, ship_address_id, warehouse_id, promo_code,
                     status, ordered_at, shipped_at)
SELECT 100000 + x.n, x.customer_id, r.employee_id, a.address_id, x.warehouse_id,
       CASE WHEN x.ordered_at BETWEEN '2024-11-25' AND '2024-12-03' AND x.n % 2 = 0 THEN 'BF2024'
            WHEN x.ordered_at BETWEEN '2025-11-24' AND '2025-12-02' AND x.n % 2 = 0 THEN 'BF2025'
            WHEN x.ordered_at BETWEEN '2024-03-01' AND '2024-06-01' AND x.n % 5 = 0 THEN 'SPRING24'
            WHEN x.ordered_at BETWEEN '2025-06-01' AND '2025-09-01' AND x.n % 5 = 0 THEN 'SUMMER25'
            WHEN x.n % 13 = 0 THEN 'WELCOME10'
            WHEN x.n % 17 = 0 THEN 'LOYAL5' END,
       x.status, x.ordered_at,
       CASE WHEN x.status IN ('shipped', 'delivered', 'refunded') THEN DATEADD(hour, 20 + x.n % 70, x.ordered_at) END
FROM x
JOIN sales.addresses a ON a.customer_id = x.customer_id AND a.kind = 'shipping' AND a.is_default = 1
LEFT JOIN reps r ON r.rn = x.n % r.cnt AND x.n % 5 <> 0;

-- One to five lines an order; the product step of 17 never lands on the
-- same product twice within an order.
INSERT sales.order_lines (order_id, line_no, product_id, qty, unit_price, discount_pct)
SELECT o.order_id, l.n, p.product_id, (o.order_id + l.n) % 4 + 1, p.list_price,
       CASE WHEN (o.order_id + l.n) % 11 = 0 THEN 10 ELSE 0 END
FROM sales.orders o
JOIN dbo.numbers l ON l.n <= o.order_id % 5 + 1
JOIN inventory.products p ON p.product_id = (o.order_id * 31 + l.n * 17) % 300 + 1;

EXEC sales.usp_recalculate_order;

-- Open orders hold stock; top up on_hand so every reservation is covered.
UPDATE s SET reserved = r.qty, on_hand = s.on_hand + r.qty
FROM inventory.stock s
JOIN (SELECT o.warehouse_id, ol.product_id, SUM(ol.qty) AS qty
      FROM sales.orders o JOIN sales.order_lines ol ON ol.order_id = o.order_id
      WHERE o.status IN ('pending', 'paid', 'picking')
      GROUP BY o.warehouse_id, ol.product_id) r
  ON r.warehouse_id = s.warehouse_id AND r.product_id = s.product_id;

INSERT finance.invoices (invoice_id, order_id, issued_on, due_on, amount, paid_amount)
SELECT 900000 + ROW_NUMBER() OVER (ORDER BY o.order_id), o.order_id, d.issued_on,
       DATEADD(day, 30, d.issued_on), o.total,
       CASE WHEN o.status IN ('shipped', 'delivered') AND o.order_id % 17 = 0 AND o.ordered_at >= '2026-04-01' THEN 0
            WHEN o.status = 'delivered' AND o.order_id % 19 = 0 AND o.ordered_at >= '2026-02-01'
                 THEN CAST(o.total / 2 AS decimal(12,2))
            ELSE o.total END
FROM sales.orders o
CROSS APPLY (SELECT CAST(o.ordered_at AS date) AS issued_on) d
WHERE o.status NOT IN ('pending', 'cancelled');

INSERT finance.payments (invoice_id, method, amount, paid_at, card_last4)
SELECT i.invoice_id, m.method, i.paid_amount,
       DATEADD(minute, 3 + i.invoice_id % 600, CAST(i.issued_on AS datetime2(3))),
       CASE WHEN m.method = 'card' THEN RIGHT(CONCAT('000', i.invoice_id * 37 % 10000), 4) END
FROM finance.invoices i
CROSS APPLY (SELECT CHOOSE(i.invoice_id % 10 + 1, 'card', 'card', 'card', 'card', 'card', 'card',
                           'paypal', 'paypal', 'wire', 'giftcard') AS method) m
WHERE i.paid_amount > 0;

-- Weekday rates only (2025-01-01 was a Wednesday), so a weekend lookup has
-- to fall back to Friday.
INSERT finance.exchange_rates (currency, rate_date, usd_per_unit)
SELECT c.currency, DATEADD(day, d.n - 1, CAST('2025-01-01' AS date)),
       CAST(c.base * (1 + c.swing * SIN(d.n / 23.0) + 0.0001 * (d.n % 7 - 3)) AS decimal(18,8))
FROM dbo.numbers d
CROSS JOIN (VALUES ('EUR', 1.08, 0.03), ('GBP', 1.27, 0.025), ('JPY', 0.0068, 0.05), ('SGD', 0.745, 0.015))
    c (currency, base, swing)
WHERE d.n <= DATEDIFF(day, '2025-01-01', '2026-09-30') + 1 AND (d.n + 1) % 7 NOT IN (5, 6);

-- History for the movement log; the trigger that applies movements to
-- stock is created after this, so these rows do not move on_hand again.
INSERT inventory.stock_movements (warehouse_id, product_id, qty, reason, order_id, moved_at)
SELECT warehouse_id, product_id, on_hand + 60, 'receipt', NULL, '2026-06-01T07:00:00'
FROM inventory.stock
UNION ALL
SELECT o.warehouse_id, ol.product_id, -ol.qty, 'sale', o.order_id, o.shipped_at
FROM sales.orders o JOIN sales.order_lines ol ON ol.order_id = o.order_id
WHERE o.order_id > 118000 AND o.status IN ('shipped', 'delivered')
UNION ALL
SELECT o.warehouse_id, ol.product_id, ol.qty, 'return', o.order_id, DATEADD(day, 9, o.shipped_at)
FROM sales.orders o JOIN sales.order_lines ol ON ol.order_id = o.order_id
WHERE o.order_id > 110000 AND o.status = 'refunded';

INSERT audit.change_log (table_name, key_value, column_name, old_value, new_value, changed_by, changed_at)
SELECT 'sales.orders', CAST(order_id AS nvarchar(100)), 'status', 'shipped', 'delivered',
       'svc_fulfilment', DATEADD(day, 2, shipped_at)
FROM sales.orders WHERE order_id > 117000 AND status = 'delivered';

INSERT dbo.app_settings (setting_key, setting_value) VALUES
    ('checkout.max_lines', N'50'),
    ('checkout.tax_mode', N'by_warehouse'),
    ('feature.new_search', N'false'),
    ('inventory.restock_multiplier', N'3'),
    ('support.banner', N'Orders placed after 3 pm ship the next business day.');
UPDATE dbo.app_settings SET setting_value = N'true' WHERE setting_key = 'feature.new_search';

INSERT dbo.[Legacy Import 2019] ([Row #], [Customer Name], [Amount (USD)], [Imported?], [Notes/Comments])
SELECT n,
       CHOOSE(n % 5 + 1, N'ACME Corp', N'  padded name  ', NULL, N'Müller & Söhne', N'O''Hara Ltd'),
       CASE WHEN n % 4 = 0 THEN NULL ELSE n * 123.45 END,
       CASE WHEN n % 3 = 0 THEN 0 ELSE 1 END,
       CASE WHEN n % 6 = 0 THEN N'row failed: bad date "31/02/2019"' END
FROM dbo.numbers WHERE n <= 25;
GO

------------------------------------------------------------------ views
CREATE VIEW sales.v_order_summary AS
SELECT o.order_id, o.order_no, o.ordered_at, o.status,
       c.customer_id, c.first_name + N' ' + c.last_name AS customer_name, c.tier,
       e.full_name AS sales_rep, w.code AS warehouse,
       l.line_count, l.units, o.subtotal, o.discount, o.tax, o.total
FROM sales.orders o
JOIN sales.customers c ON c.customer_id = o.customer_id
JOIN inventory.warehouses w ON w.warehouse_id = o.warehouse_id
LEFT JOIN hr.employees e ON e.employee_id = o.sales_rep_id
OUTER APPLY (SELECT COUNT(*) AS line_count, SUM(ol.qty) AS units
             FROM sales.order_lines ol WHERE ol.order_id = o.order_id) l;
GO

CREATE VIEW sales.v_customer_360 AS
WITH stats AS (
    SELECT customer_id, COUNT(*) AS orders, SUM(total) AS lifetime_value,
           MIN(ordered_at) AS first_order_at, MAX(ordered_at) AS last_order_at
    FROM sales.orders
    WHERE status NOT IN ('cancelled', 'refunded')
    GROUP BY customer_id
)
SELECT c.customer_id, c.first_name + N' ' + c.last_name AS name, c.email, c.tier,
       ISNULL(s.orders, 0) AS orders, ISNULL(s.lifetime_value, 0) AS lifetime_value,
       s.first_order_at, s.last_order_at,
       DATEDIFF(day, s.last_order_at, SYSUTCDATETIME()) AS days_since_last_order,
       fav.category AS favorite_category, a.city, a.country
FROM sales.customers c
LEFT JOIN stats s ON s.customer_id = c.customer_id
LEFT JOIN sales.addresses a ON a.customer_id = c.customer_id AND a.kind = 'shipping' AND a.is_default = 1
OUTER APPLY (
    SELECT TOP (1) cat.name AS category
    FROM sales.orders o
    JOIN sales.order_lines ol ON ol.order_id = o.order_id
    JOIN inventory.products p ON p.product_id = ol.product_id
    JOIN inventory.categories cat ON cat.category_id = p.category_id
    WHERE o.customer_id = c.customer_id
    GROUP BY cat.name
    ORDER BY SUM(ol.line_total) DESC, cat.name
) fav;
GO

CREATE VIEW inventory.v_category_tree AS
WITH tree AS (
    SELECT category_id, parent_id, name, 0 AS depth, CAST(name AS nvarchar(400)) AS path
    FROM inventory.categories WHERE parent_id IS NULL
    UNION ALL
    SELECT c.category_id, c.parent_id, c.name, t.depth + 1, CAST(t.path + N' > ' + c.name AS nvarchar(400))
    FROM inventory.categories c JOIN tree t ON t.category_id = c.parent_id
)
SELECT category_id, parent_id, name, depth, path FROM tree;
GO

CREATE VIEW inventory.v_low_stock AS
SELECT w.code AS warehouse, p.product_id, p.sku, p.name AS product,
       s.on_hand, s.reserved, s.available, s.reorder_level,
       sup.name AS supplier, sup.contact_email
FROM inventory.stock s
JOIN inventory.products p ON p.product_id = s.product_id
JOIN inventory.warehouses w ON w.warehouse_id = s.warehouse_id
JOIN inventory.suppliers sup ON sup.supplier_id = p.supplier_id
WHERE s.available <= s.reorder_level AND p.discontinued = 0;
GO

CREATE VIEW hr.v_org_chart AS
WITH tree AS (
    SELECT employee_id, manager_id, full_name, title, department_id, 0 AS depth,
           CAST(full_name AS nvarchar(1000)) AS chain
    FROM hr.employees WHERE manager_id IS NULL
    UNION ALL
    SELECT e.employee_id, e.manager_id, e.full_name, e.title, e.department_id, t.depth + 1,
           CAST(t.chain + N' > ' + e.full_name AS nvarchar(1000))
    FROM hr.employees e JOIN tree t ON e.manager_id = t.employee_id
    WHERE e.terminated_on IS NULL
)
SELECT t.employee_id, REPLICATE(N'    ', t.depth) + t.full_name AS employee, t.title,
       d.name AS department, t.depth, t.chain
FROM tree t JOIN hr.departments d ON d.department_id = t.department_id;
GO

CREATE VIEW reporting.v_monthly_revenue AS
WITH m AS (
    SELECT DATEFROMPARTS(YEAR(ordered_at), MONTH(ordered_at), 1) AS month,
           COUNT(*) AS orders, SUM(total) AS revenue
    FROM sales.orders
    WHERE status NOT IN ('cancelled', 'refunded')
    GROUP BY DATEFROMPARTS(YEAR(ordered_at), MONTH(ordered_at), 1)
)
SELECT month, orders, revenue,
       CAST(revenue / orders AS decimal(12,2)) AS avg_order_value,
       SUM(revenue) OVER (PARTITION BY YEAR(month) ORDER BY month ROWS UNBOUNDED PRECEDING) AS ytd_revenue,
       CAST(100.0 * (revenue - LAG(revenue) OVER (ORDER BY month))
            / NULLIF(LAG(revenue) OVER (ORDER BY month), 0) AS decimal(7,2)) AS mom_growth_pct
FROM m;
GO

CREATE VIEW reporting.v_product_rankings AS
SELECT cat.name AS category, p.sku, p.name AS product,
       SUM(ol.qty) AS units, SUM(ol.line_total) AS revenue,
       RANK() OVER (PARTITION BY cat.name ORDER BY SUM(ol.line_total) DESC) AS rank_in_category,
       CAST(100.0 * SUM(ol.line_total) / SUM(SUM(ol.line_total)) OVER (PARTITION BY cat.name)
            AS decimal(5,2)) AS pct_of_category
FROM sales.order_lines ol
JOIN sales.orders o ON o.order_id = ol.order_id
JOIN inventory.products p ON p.product_id = ol.product_id
JOIN inventory.categories cat ON cat.category_id = p.category_id
WHERE o.status NOT IN ('cancelled', 'refunded')
GROUP BY cat.name, p.sku, p.name;
GO

CREATE VIEW finance.v_ar_aging AS
SELECT i.invoice_id, o.order_no, c.first_name + N' ' + c.last_name AS customer,
       i.issued_on, i.due_on, i.amount, i.paid_amount, i.amount - i.paid_amount AS outstanding,
       a.days_overdue,
       CASE WHEN a.days_overdue <= 0 THEN 'current'
            WHEN a.days_overdue <= 30 THEN '1-30'
            WHEN a.days_overdue <= 60 THEN '31-60'
            WHEN a.days_overdue <= 90 THEN '61-90'
            ELSE '90+' END AS bucket
FROM finance.invoices i
JOIN sales.orders o ON o.order_id = i.order_id
JOIN sales.customers c ON c.customer_id = o.customer_id
CROSS APPLY (SELECT DATEDIFF(day, i.due_on, CAST(SYSUTCDATETIME() AS date)) AS days_overdue) a
WHERE i.paid_amount < i.amount;
GO

-- An indexed view: SQL Server keeps these sums up to date on every write.
CREATE VIEW reporting.v_daily_sales WITH SCHEMABINDING AS
SELECT CAST(o.ordered_at AS date) AS sales_date, o.warehouse_id,
       SUM(ol.line_total) AS revenue, SUM(ol.qty) AS units, COUNT_BIG(*) AS line_count
FROM sales.orders o
JOIN sales.order_lines ol ON ol.order_id = o.order_id
GROUP BY CAST(o.ordered_at AS date), o.warehouse_id;
GO
CREATE UNIQUE CLUSTERED INDEX cix_v_daily_sales ON reporting.v_daily_sales (sales_date, warehouse_id);
GO

-------------------------------------------------------------- functions
CREATE FUNCTION finance.fn_to_usd (@amount decimal(12,2), @currency char(3), @on date)
RETURNS decimal(14,2)
AS
BEGIN
    IF @currency = 'USD' RETURN @amount;
    RETURN (SELECT TOP (1) CAST(@amount * usd_per_unit AS decimal(14,2))
            FROM finance.exchange_rates
            WHERE currency = @currency AND rate_date <= @on
            ORDER BY rate_date DESC);
END;
GO

CREATE FUNCTION inventory.tvf_product_availability (@product_id int)
RETURNS TABLE
AS
RETURN
    SELECT w.warehouse_id, w.code, w.city, s.on_hand, s.reserved, s.available, s.reorder_level
    FROM inventory.stock s
    JOIN inventory.warehouses w ON w.warehouse_id = s.warehouse_id
    WHERE s.product_id = @product_id;
GO

CREATE FUNCTION hr.tvf_reports_to (@manager_id int)
RETURNS @team TABLE (
    employee_id int NOT NULL PRIMARY KEY,
    full_name   nvarchar(101) NOT NULL,
    title       nvarchar(80) NOT NULL,
    manager_id  int NULL,
    depth       int NOT NULL
)
AS
BEGIN
    WITH tree AS (
        SELECT employee_id, full_name, title, manager_id, 0 AS depth
        FROM hr.employees WHERE employee_id = @manager_id
        UNION ALL
        SELECT e.employee_id, e.full_name, e.title, e.manager_id, t.depth + 1
        FROM hr.employees e JOIN tree t ON e.manager_id = t.employee_id
        WHERE e.terminated_on IS NULL
    )
    INSERT @team SELECT employee_id, full_name, title, manager_id, depth FROM tree;
    RETURN;
END;
GO

CREATE FUNCTION reporting.tvf_sales_between (@from date, @to date)
RETURNS TABLE
AS
RETURN
    SELECT sales_date, SUM(revenue) AS revenue, SUM(units) AS units, SUM(line_count) AS lines
    FROM reporting.v_daily_sales WITH (NOEXPAND)
    WHERE sales_date >= @from AND sales_date < @to
    GROUP BY sales_date;
GO

------------------------------------------------------------- procedures
CREATE PROCEDURE sales.usp_place_order
    @customer_id  int,
    @lines        sales.OrderLineList READONLY,
    @warehouse_id smallint = 1,
    @promo_code   varchar(20) = NULL,
    @sales_rep_id int = NULL,
    @order_id     int = NULL OUTPUT
AS
BEGIN
    SET NOCOUNT, XACT_ABORT ON;
    DECLARE @msg nvarchar(2048);

    IF NOT EXISTS (SELECT 1 FROM sales.customers WHERE customer_id = @customer_id)
        THROW 50001, 'No such customer.', 1;
    IF NOT EXISTS (SELECT 1 FROM @lines)
        THROW 50002, 'An order needs at least one line.', 1;
    IF EXISTS (SELECT 1 FROM @lines l LEFT JOIN inventory.products p ON p.product_id = l.product_id
               WHERE p.product_id IS NULL OR p.discontinued = 1)
        THROW 50003, 'An order line names a missing or discontinued product.', 1;
    IF @promo_code IS NOT NULL AND sales.fn_promo_pct(@promo_code, CAST(SYSUTCDATETIME() AS date)) = 0
    BEGIN
        SET @msg = CONCAT(N'Promotion ', @promo_code, N' is not valid today.');
        THROW 50005, @msg, 1;
    END;

    BEGIN TRY
        BEGIN TRANSACTION;

        -- UPDLOCK so two orders cannot both take the last unit.
        SELECT @msg = STRING_AGG(CONCAT(p.sku, ' (', ISNULL(s.available, 0), ' left, ', l.qty, ' wanted)'), ', ')
        FROM @lines l
        JOIN inventory.products p ON p.product_id = l.product_id
        LEFT JOIN inventory.stock s WITH (UPDLOCK, HOLDLOCK)
               ON s.product_id = l.product_id AND s.warehouse_id = @warehouse_id
        WHERE ISNULL(s.available, 0) < l.qty;
        IF @msg IS NOT NULL
        BEGIN
            SET @msg = CONCAT(N'Not enough stock: ', @msg);
            THROW 50004, @msg, 1;
        END;

        SET @order_id = NEXT VALUE FOR sales.order_id_seq;
        INSERT sales.orders (order_id, customer_id, sales_rep_id, ship_address_id, warehouse_id, promo_code)
        VALUES (@order_id, @customer_id, @sales_rep_id,
                (SELECT TOP (1) address_id FROM sales.addresses
                 WHERE customer_id = @customer_id AND kind = 'shipping'
                 ORDER BY is_default DESC, address_id),
                @warehouse_id, @promo_code);

        INSERT sales.order_lines (order_id, line_no, product_id, qty, unit_price)
        SELECT @order_id, ROW_NUMBER() OVER (ORDER BY l.product_id), l.product_id, l.qty, p.list_price
        FROM @lines l JOIN inventory.products p ON p.product_id = l.product_id;

        UPDATE s SET reserved = s.reserved + l.qty, updated_at = SYSUTCDATETIME()
        FROM inventory.stock s JOIN @lines l ON l.product_id = s.product_id
        WHERE s.warehouse_id = @warehouse_id;

        EXEC sales.usp_recalculate_order @order_id;
        COMMIT;
    END TRY
    BEGIN CATCH
        IF @@TRANCOUNT > 0 ROLLBACK;
        THROW;
    END CATCH;

    SELECT * FROM sales.v_order_summary WHERE order_id = @order_id;
END;
GO

CREATE PROCEDURE sales.usp_quick_order
    @customer_id  int,
    @product_id   int,
    @qty          int = 1,
    @warehouse_id smallint = 1
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @lines sales.OrderLineList;
    INSERT @lines (product_id, qty) VALUES (@product_id, @qty);
    EXEC sales.usp_place_order @customer_id = @customer_id, @lines = @lines, @warehouse_id = @warehouse_id;
END;
GO

-- pending -> paid -> picking -> shipped -> delivered, one step a call.
CREATE PROCEDURE sales.usp_advance_order @order_id int
AS
BEGIN
    SET NOCOUNT, XACT_ABORT ON;
    DECLARE @status varchar(10), @warehouse_id smallint, @total decimal(12,2), @msg nvarchar(200);

    BEGIN TRANSACTION;
    SELECT @status = status, @warehouse_id = warehouse_id, @total = total
    FROM sales.orders WITH (UPDLOCK, ROWLOCK) WHERE order_id = @order_id;

    IF @status IS NULL
    BEGIN
        SET @msg = CONCAT(N'No such order: ', @order_id);
        THROW 50010, @msg, 1;
    END;

    IF @status = 'pending'
    BEGIN
        -- Payment is taken up front: invoice the order and settle it by card.
        DECLARE @invoice_id int = NEXT VALUE FOR finance.invoice_seq;
        INSERT finance.invoices (invoice_id, order_id, issued_on, due_on, amount, paid_amount)
        VALUES (@invoice_id, @order_id, CAST(SYSUTCDATETIME() AS date),
                DATEADD(day, 30, CAST(SYSUTCDATETIME() AS date)), @total, @total);
        INSERT finance.payments (invoice_id, method, amount, paid_at, card_last4)
        VALUES (@invoice_id, 'card', @total, SYSUTCDATETIME(), '4242');
        UPDATE sales.orders SET status = 'paid' WHERE order_id = @order_id;
    END
    ELSE IF @status = 'paid'
        UPDATE sales.orders SET status = 'picking' WHERE order_id = @order_id;
    ELSE IF @status = 'picking'
    BEGIN
        -- The movement trigger takes the units off on_hand; the reservation goes with them.
        INSERT inventory.stock_movements (warehouse_id, product_id, qty, reason, order_id)
        SELECT @warehouse_id, product_id, -SUM(qty), 'sale', @order_id
        FROM sales.order_lines WHERE order_id = @order_id GROUP BY product_id;

        UPDATE s SET reserved = s.reserved - l.qty
        FROM inventory.stock s
        JOIN (SELECT product_id, SUM(qty) AS qty FROM sales.order_lines
              WHERE order_id = @order_id GROUP BY product_id) l ON l.product_id = s.product_id
        WHERE s.warehouse_id = @warehouse_id;

        UPDATE sales.orders SET status = 'shipped', shipped_at = SYSUTCDATETIME() WHERE order_id = @order_id;
    END
    ELSE IF @status = 'shipped'
        UPDATE sales.orders SET status = 'delivered' WHERE order_id = @order_id;
    ELSE
    BEGIN
        SET @msg = CONCAT(N'Order ', @order_id, N' is ', @status, N'; nothing comes after that.');
        THROW 50011, @msg, 1;
    END;
    COMMIT;

    SELECT order_id, order_no, status, ordered_at, shipped_at, total
    FROM sales.orders WHERE order_id = @order_id;
END;
GO

CREATE PROCEDURE sales.usp_cancel_order @order_id int, @reason nvarchar(200)
AS
BEGIN
    SET NOCOUNT, XACT_ABORT ON;
    DECLARE @status varchar(10), @warehouse_id smallint, @msg nvarchar(200);

    BEGIN TRANSACTION;
    SELECT @status = status, @warehouse_id = warehouse_id
    FROM sales.orders WITH (UPDLOCK, ROWLOCK) WHERE order_id = @order_id;

    IF @status IS NULL OR @status NOT IN ('pending', 'paid', 'picking')
    BEGIN
        SET @msg = CONCAT(N'Order ', @order_id, N' is ', ISNULL(@status, N'missing'), N' and cannot be cancelled.');
        THROW 50012, @msg, 1;
    END;

    UPDATE s SET reserved = s.reserved - l.qty
    FROM inventory.stock s
    JOIN (SELECT product_id, SUM(qty) AS qty FROM sales.order_lines
          WHERE order_id = @order_id GROUP BY product_id) l ON l.product_id = s.product_id
    WHERE s.warehouse_id = @warehouse_id;

    -- Money already taken goes back, which makes it a refund, not a cancellation.
    UPDATE sales.orders SET status = IIF(@status = 'pending', 'cancelled', 'refunded') WHERE order_id = @order_id;
    INSERT audit.change_log (table_name, key_value, column_name, new_value)
    VALUES ('sales.orders', CAST(@order_id AS nvarchar(100)), 'cancel_reason', @reason);
    COMMIT;
END;
GO

-- Three result sets: the customer, a page of their orders, totals by status.
CREATE PROCEDURE sales.usp_customer_orders
    @customer_id int,
    @status      varchar(10) = NULL,
    @page        int = 1,
    @page_size   int = 25
AS
BEGIN
    SET NOCOUNT ON;
    IF @page < 1 OR @page_size NOT BETWEEN 1 AND 500
        THROW 50006, 'page starts at 1 and page_size is 1 to 500.', 1;

    SELECT * FROM sales.v_customer_360 WHERE customer_id = @customer_id;

    SELECT order_id, order_no, ordered_at, status, line_count, units, total, sales_rep, warehouse
    FROM sales.v_order_summary
    WHERE customer_id = @customer_id AND (@status IS NULL OR status = @status)
    ORDER BY ordered_at DESC
    OFFSET (@page - 1) * @page_size ROWS FETCH NEXT @page_size ROWS ONLY;

    SELECT status, COUNT(*) AS orders, SUM(total) AS total
    FROM sales.orders WHERE customer_id = @customer_id
    GROUP BY status ORDER BY orders DESC;
END;
GO

CREATE PROCEDURE finance.usp_apply_payment
    @invoice_id int,
    @amount     decimal(12,2),
    @method     varchar(8) = 'card',
    @card_last4 char(4) = NULL
AS
BEGIN
    SET NOCOUNT, XACT_ABORT ON;
    DECLARE @outstanding decimal(12,2), @msg nvarchar(200);

    BEGIN TRANSACTION;
    SELECT @outstanding = amount - paid_amount
    FROM finance.invoices WITH (UPDLOCK, ROWLOCK) WHERE invoice_id = @invoice_id;

    IF @outstanding IS NULL
        THROW 50040, 'No such invoice.', 1;
    IF @amount > @outstanding
    BEGIN
        SET @msg = CONCAT(N'Only ', @outstanding, N' is outstanding on invoice ', @invoice_id, N'.');
        THROW 50041, @msg, 1;
    END;

    INSERT finance.payments (invoice_id, method, amount, paid_at, card_last4)
    VALUES (@invoice_id, @method, @amount, SYSUTCDATETIME(), @card_last4);
    UPDATE finance.invoices SET paid_amount = paid_amount + @amount WHERE invoice_id = @invoice_id;
    COMMIT;

    SELECT invoice_id, amount, paid_amount, amount - paid_amount AS outstanding
    FROM finance.invoices WHERE invoice_id = @invoice_id;
END;
GO

-- A cursor on purpose: it reports progress as it goes, which the set-based
-- version could not.
CREATE PROCEDURE inventory.usp_restock
    @warehouse_id smallint = NULL,
    @dry_run      bit = 1
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @multiplier int = ISNULL(TRY_CAST((SELECT setting_value FROM dbo.app_settings
                                               WHERE setting_key = 'inventory.restock_multiplier') AS int), 2);
    DECLARE @plan TABLE (warehouse_id smallint, product_id int, sku varchar(20),
                         available int, reorder_level int, order_qty int);

    INSERT @plan
    SELECT s.warehouse_id, s.product_id, p.sku, s.available, s.reorder_level,
           s.reorder_level * @multiplier - s.available
    FROM inventory.stock s JOIN inventory.products p ON p.product_id = s.product_id
    WHERE s.available <= s.reorder_level AND p.discontinued = 0
      AND (@warehouse_id IS NULL OR s.warehouse_id = @warehouse_id);

    DECLARE @w smallint, @p int, @qty int, @n int = 0;
    DECLARE restock CURSOR LOCAL FAST_FORWARD FOR
        SELECT warehouse_id, product_id, order_qty FROM @plan ORDER BY warehouse_id, sku;
    OPEN restock;
    FETCH NEXT FROM restock INTO @w, @p, @qty;
    WHILE @@FETCH_STATUS = 0
    BEGIN
        IF @dry_run = 0
            INSERT inventory.stock_movements (warehouse_id, product_id, qty, reason)
            VALUES (@w, @p, @qty, 'receipt');
        SET @n += 1;
        IF @n % 50 = 0 RAISERROR('restock: %d lines so far', 0, 1, @n) WITH NOWAIT;
        FETCH NEXT FROM restock INTO @w, @p, @qty;
    END;
    CLOSE restock;
    DEALLOCATE restock;

    PRINT CONCAT(IIF(@dry_run = 1, 'dry run: would restock ', 'restocked '), @n, ' lines');
    SELECT w.code AS warehouse, pl.sku, pl.available, pl.reorder_level, pl.order_qty
    FROM @plan pl JOIN inventory.warehouses w ON w.warehouse_id = pl.warehouse_id
    ORDER BY w.code, pl.sku;
END;
GO

CREATE PROCEDURE hr.usp_give_raise
    @employee_id int,
    @pct         decimal(5,2),
    @reason      varchar(20) = 'merit'
AS
BEGIN
    SET NOCOUNT ON;
    IF @pct <= 0 OR @pct > 25
        THROW 50030, 'A raise is more than 0 and at most 25 percent.', 1;
    IF @reason NOT IN ('merit', 'promotion', 'market', 'correction')
        THROW 50031, 'reason is merit, promotion, market or correction.', 1;

    -- The salary trigger reads the reason from here.
    EXEC sys.sp_set_session_context @key = N'raise_reason', @value = @reason;
    UPDATE hr.employees SET salary = CAST(salary * (1 + @pct / 100) AS decimal(10,2))
    WHERE employee_id = @employee_id AND terminated_on IS NULL;
    IF @@ROWCOUNT = 0
        THROW 50032, 'No such active employee.', 1;
    EXEC sys.sp_set_session_context @key = N'raise_reason', @value = NULL;

    SELECT TOP (5) h.effective_on, h.salary, h.reason, e.full_name
    FROM hr.salary_history h JOIN hr.employees e ON e.employee_id = h.employee_id
    WHERE h.employee_id = @employee_id
    ORDER BY h.effective_on DESC, h.history_id DESC;
END;
GO

CREATE PROCEDURE reporting.usp_sales_report
    @from     date = NULL,
    @to       date = NULL,
    @group_by varchar(10) = 'month'
AS
BEGIN
    SET NOCOUNT ON;
    SET @from = ISNULL(@from, DATEADD(year, -1, CAST(SYSUTCDATETIME() AS date)));
    SET @to = ISNULL(@to, CAST(SYSUTCDATETIME() AS date));

    DECLARE @bucket nvarchar(200) = CASE @group_by
        WHEN 'day' THEN N'CONVERT(char(10), o.ordered_at, 23)'
        WHEN 'week' THEN N'CONVERT(char(10), DATEADD(week, DATEDIFF(week, 0, o.ordered_at), 0), 23)'
        WHEN 'month' THEN N'CONVERT(char(7), o.ordered_at, 23)'
        WHEN 'category' THEN N'cat.name'
        WHEN 'warehouse' THEN N'w.code'
        WHEN 'rep' THEN N'ISNULL(e.full_name, N''(house)'')'
    END;
    IF @bucket IS NULL
        THROW 50020, 'group_by is day, week, month, category, warehouse or rep.', 1;

    DECLARE @sql nvarchar(max) = N'
SELECT ' + @bucket + N' AS bucket,
       COUNT(DISTINCT o.order_id) AS orders,
       SUM(ol.qty) AS units,
       SUM(ol.line_total) AS revenue,
       SUM(ol.qty * p.unit_cost) AS cost,
       SUM(ol.line_total) - SUM(ol.qty * p.unit_cost) AS gross_margin
FROM sales.orders o
JOIN sales.order_lines ol ON ol.order_id = o.order_id
JOIN inventory.products p ON p.product_id = ol.product_id
JOIN inventory.categories cat ON cat.category_id = p.category_id
JOIN inventory.warehouses w ON w.warehouse_id = o.warehouse_id
LEFT JOIN hr.employees e ON e.employee_id = o.sales_rep_id
WHERE o.ordered_at >= @from AND o.ordered_at < DATEADD(day, 1, @to)
  AND o.status NOT IN (''cancelled'', ''refunded'')
GROUP BY ' + @bucket + N'
ORDER BY ' + IIF(@group_by IN ('day', 'week', 'month'), N'bucket', N'revenue DESC') + N';';

    EXEC sys.sp_executesql @sql, N'@from date, @to date', @from = @from, @to = @to;
END;
GO

CREATE PROCEDURE reporting.usp_refresh_customer_tiers
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @changes TABLE (customer_id int, old_tier varchar(10), new_tier varchar(10));

    UPDATE c SET tier = t.tier
    OUTPUT inserted.customer_id, deleted.tier, inserted.tier INTO @changes
    FROM sales.customers c
    LEFT JOIN (SELECT customer_id, SUM(total) AS lifetime_value
               FROM sales.orders WHERE status NOT IN ('cancelled', 'refunded')
               GROUP BY customer_id) v ON v.customer_id = c.customer_id
    CROSS APPLY (SELECT sales.fn_customer_tier(ISNULL(v.lifetime_value, 0)) AS tier) t
    WHERE c.tier <> t.tier;

    SELECT old_tier, new_tier, COUNT(*) AS customers
    FROM @changes GROUP BY old_tier, new_tier ORDER BY old_tier, new_tier;
END;
GO

CREATE PROCEDURE audit.usp_purge_change_log
    @older_than_days int = 90,
    @batch_size      int = 500
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @cutoff datetime2(3) = DATEADD(day, -@older_than_days, SYSUTCDATETIME());
    DECLARE @n int = 1, @total int = 0;
    WHILE @n > 0
    BEGIN
        DELETE TOP (@batch_size) FROM audit.change_log WHERE changed_at < @cutoff;
        SET @n = @@ROWCOUNT;
        SET @total += @n;
    END;
    SELECT @total AS purged_rows, @cutoff AS cutoff;
END;
GO

-- For trying cancel: EXEC dbo.usp_wait 30, then cancel it.
CREATE PROCEDURE dbo.usp_wait @seconds int = 10
AS
BEGIN
    DECLARE @delay char(8) = CONVERT(char(8), DATEADD(second, @seconds, 0), 108);
    RAISERROR('waiting %d seconds; try cancelling me', 0, 1, @seconds) WITH NOWAIT;
    WAITFOR DELAY @delay;
    SELECT @seconds AS waited_seconds, SYSUTCDATETIME() AS finished_at;
END;
GO

-- Broken on purpose: the table it reads was dropped years ago. It compiles
-- (deferred name resolution) and fails when run.
CREATE PROCEDURE dbo.usp_legacy_export
AS
    SELECT * FROM dbo.legacy_orders_2019;
GO

--------------------------------------------------------------- triggers
CREATE TRIGGER sales.trg_orders_status_audit ON sales.orders
AFTER UPDATE
AS
BEGIN
    SET NOCOUNT ON;
    IF NOT UPDATE(status) RETURN;
    INSERT audit.change_log (table_name, key_value, column_name, old_value, new_value)
    SELECT 'sales.orders', CAST(i.order_id AS nvarchar(100)), 'status', d.status, i.status
    FROM inserted i JOIN deleted d ON d.order_id = i.order_id
    WHERE i.status <> d.status;
END;
GO

CREATE TRIGGER inventory.trg_stock_movements_apply ON inventory.stock_movements
AFTER INSERT
AS
BEGIN
    SET NOCOUNT ON;
    UPDATE s SET on_hand = s.on_hand + m.qty, updated_at = SYSUTCDATETIME()
    FROM inventory.stock s
    JOIN (SELECT warehouse_id, product_id, SUM(qty) AS qty FROM inserted
          GROUP BY warehouse_id, product_id) m
      ON m.warehouse_id = s.warehouse_id AND m.product_id = s.product_id;
END;
GO

CREATE TRIGGER hr.trg_employees_salary ON hr.employees
AFTER UPDATE
AS
BEGIN
    SET NOCOUNT ON;
    IF NOT UPDATE(salary) RETURN;
    INSERT hr.salary_history (employee_id, effective_on, salary, reason)
    SELECT i.employee_id, CAST(SYSUTCDATETIME() AS date), i.salary,
           ISNULL(CAST(SESSION_CONTEXT(N'raise_reason') AS varchar(20)), 'correction')
    FROM inserted i JOIN deleted d ON d.employee_id = i.employee_id
    WHERE i.salary <> d.salary;
END;
GO

------------------------------------------------- indexes and synonyms
CREATE INDEX ix_orders_customer ON sales.orders (customer_id, ordered_at DESC) INCLUDE (status, total);
CREATE INDEX ix_orders_open ON sales.orders (ordered_at) WHERE status IN ('pending', 'paid', 'picking');
CREATE INDEX ix_order_lines_product ON sales.order_lines (product_id) INCLUDE (qty, line_total);
CREATE INDEX ix_products_category ON inventory.products (category_id) INCLUDE (list_price);
CREATE INDEX ix_employees_manager ON hr.employees (manager_id);
CREATE INDEX ix_customers_last_name ON sales.customers (last_name, first_name);
CREATE NONCLUSTERED COLUMNSTORE INDEX ncci_stock_movements
    ON inventory.stock_movements (warehouse_id, product_id, qty, reason, moved_at);

-- sa's default schema is dbo, so `select * from orders` just works.
CREATE SYNONYM dbo.orders FOR sales.orders;
CREATE SYNONYM dbo.customers FOR sales.customers;
CREATE SYNONYM dbo.products FOR inventory.products;
GO

------------------------------- some activity, through the real procedures
EXEC reporting.usp_refresh_customer_tiers;

DECLARE @lines sales.OrderLineList, @id int;
INSERT @lines (product_id, qty) VALUES (12, 2), (40, 1), (101, 3);
EXEC sales.usp_place_order @customer_id = 7, @lines = @lines, @warehouse_id = 1,
     @promo_code = 'WELCOME10', @sales_rep_id = 11, @order_id = @id OUTPUT;
EXEC sales.usp_advance_order @id;
EXEC sales.usp_advance_order @id;
EXEC sales.usp_advance_order @id;

EXEC sales.usp_quick_order @customer_id = 42, @product_id = 57, @qty = 2, @warehouse_id = 2;
EXEC sales.usp_advance_order 119990;
EXEC sales.usp_cancel_order 119995, N'Customer found it cheaper elsewhere';
EXEC hr.usp_give_raise @employee_id = 12, @pct = 4.5, @reason = 'merit';
EXEC inventory.usp_restock @warehouse_id = 3, @dry_run = 0;
GO

-- Last, so the seed itself is not in the DDL log.
CREATE TRIGGER trg_ddl_audit ON DATABASE
FOR DDL_DATABASE_LEVEL_EVENTS
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @e xml = EVENTDATA();
    INSERT audit.ddl_events (event_type, object_name, login_name, tsql)
    VALUES (@e.value('(/EVENT_INSTANCE/EventType)[1]', 'nvarchar(64)'),
            @e.value('concat((/EVENT_INSTANCE/SchemaName)[1], ".", (/EVENT_INSTANCE/ObjectName)[1])', 'nvarchar(256)'),
            @e.value('(/EVENT_INSTANCE/LoginName)[1]', 'sysname'),
            @e.value('(/EVENT_INSTANCE/TSQLCommand/CommandText)[1]', 'nvarchar(max)'));
END;
GO
