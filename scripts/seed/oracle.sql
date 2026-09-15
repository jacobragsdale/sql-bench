/*
  sql-bench seed data -- Oracle dialect, loaded as the user BENCH (the user
  is the schema). Re-runnable: DDL swallows ORA-00955 "name already used"
  and the inserts are skipped once CUSTOMERS has rows.

  Licensing: this loads into Oracle Database Free, which Oracle licenses
  free of charge for development, testing and production alike, limited to
  1 CPU, 2 GB of memory and 12 GB of user data per database.
*/
WHENEVER SQLERROR EXIT SQL.SQLCODE
WHENEVER OSERROR EXIT FAILURE
SET DEFINE OFF
SET FEEDBACK OFF
SET ECHO OFF

------------------------------------------------------------------ tables
DECLARE
    PROCEDURE ddl(p_sql VARCHAR2) IS
    BEGIN
        EXECUTE IMMEDIATE p_sql;
    EXCEPTION
        WHEN OTHERS THEN
            IF SQLCODE != -955 THEN RAISE; END IF;   -- -955: already exists
    END;
BEGIN
    ddl('CREATE TABLE customers (
             id           NUMBER(10)    NOT NULL PRIMARY KEY,
             name         NVARCHAR2(100) NOT NULL,
             email        VARCHAR2(200) NOT NULL UNIQUE,
             country      CHAR(2)       NOT NULL,
             created_at   TIMESTAMP     NOT NULL,
             credit_limit NUMBER(12,2))');
    ddl('CREATE TABLE orders (
             id          NUMBER(10)    NOT NULL PRIMARY KEY,
             customer_id NUMBER(10)    NOT NULL REFERENCES customers(id),
             ordered_at  TIMESTAMP     NOT NULL,
             status      VARCHAR2(20)  NOT NULL,
             total       NUMBER(12,2)  NOT NULL)');
    ddl('CREATE TABLE order_items (
             id         NUMBER(10)   NOT NULL PRIMARY KEY,
             order_id   NUMBER(10)   NOT NULL REFERENCES orders(id),
             sku        VARCHAR2(40) NOT NULL,
             qty        NUMBER(10)   NOT NULL,
             unit_price NUMBER(10,2) NOT NULL)');
    ddl('CREATE TABLE events (
             id          NUMBER(19)   NOT NULL PRIMARY KEY,
             customer_id NUMBER(10)   NOT NULL,
             kind        VARCHAR2(30) NOT NULL,
             payload     VARCHAR2(200) NOT NULL,
             at          TIMESTAMP    NOT NULL)');
    ddl('CREATE TABLE big_text (id NUMBER(10) NOT NULL PRIMARY KEY, body CLOB)');
    ddl('CREATE TABLE binary_blobs (id NUMBER(10) NOT NULL PRIMARY KEY, data BLOB)');
    ddl('CREATE TABLE all_types (
             id               NUMBER(10) NOT NULL PRIMARY KEY,
             c_number         NUMBER,
             c_number_10_2    NUMBER(10,2),
             c_binary_float   BINARY_FLOAT,
             c_binary_double  BINARY_DOUBLE,
             c_char           CHAR(5),
             c_varchar2       VARCHAR2(20),
             c_nvarchar2      NVARCHAR2(20),
             c_date           DATE,
             c_timestamp      TIMESTAMP,
             c_timestamp_tz   TIMESTAMP WITH TIME ZONE,
             c_interval_ds    INTERVAL DAY TO SECOND,
             c_raw            RAW(8),
             c_clob           CLOB,
             c_blob           BLOB)');
END;
/

------------------------------------------------------------------- data
DECLARE
    n     NUMBER;
    chunk VARCHAR2(32767) := RPAD('Lorem ipsu', 25600, 'Lorem ipsu');
    body  CLOB;
BEGIN
    SELECT COUNT(*) INTO n FROM customers;
    IF n = 0 THEN
        INSERT INTO customers (id, name, email, country, created_at, credit_limit)
        SELECT LEVEL,
               CASE MOD(LEVEL, 10)
                   WHEN 0 THEN N'山田太郎'
                   WHEN 1 THEN N'Zoë Bauer'
                   WHEN 2 THEN N'Ægir Nilsen'
                   WHEN 3 THEN N'李雷'
                   WHEN 4 THEN N'José Álvarez'
                   WHEN 5 THEN N'Björk Þórsdóttir'
                   WHEN 6 THEN N'Анна Иванова'
                   WHEN 7 THEN N'محمد الفارسي'
                   WHEN 8 THEN N'Mary O''Neill'
                   ELSE N'Šimon Novák'
               END,
               'customer' || LEVEL || '@example.com',
               CASE MOD(LEVEL, 10)
                   WHEN 0 THEN 'JP' WHEN 1 THEN 'DE' WHEN 2 THEN 'NO' WHEN 3 THEN 'CN'
                   WHEN 4 THEN 'ES' WHEN 5 THEN 'IS' WHEN 6 THEN 'RU' WHEN 7 THEN 'SA'
                   WHEN 8 THEN 'IE' ELSE 'CZ'
               END,
               TIMESTAMP '2024-01-01 08:30:00' + NUMTODSINTERVAL(LEVEL, 'DAY'),
               CASE WHEN MOD(LEVEL, 7) = 0 THEN NULL ELSE 100.50 * LEVEL END
        FROM dual CONNECT BY LEVEL <= 50;
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM orders;
    IF n = 0 THEN
        INSERT INTO orders (id, customer_id, ordered_at, status, total)
        SELECT LEVEL,
               MOD(LEVEL - 1, 50) + 1,
               TIMESTAMP '2024-03-01 09:00:00' + NUMTODSINTERVAL(LEVEL, 'HOUR'),
               CASE MOD(LEVEL, 4)
                   WHEN 0 THEN 'NEW' WHEN 1 THEN 'PAID'
                   WHEN 2 THEN 'SHIPPED' ELSE 'CANCELLED'
               END,
               3.25 * LEVEL
        FROM dual CONNECT BY LEVEL <= 500;
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM order_items;
    IF n = 0 THEN
        INSERT INTO order_items (id, order_id, sku, qty, unit_price)
        SELECT LEVEL,
               MOD(LEVEL - 1, 500) + 1,
               'SKU-' || TO_CHAR(LEVEL, 'FM00000'),
               MOD(LEVEL, 5) + 1,
               9.99 + MOD(LEVEL, 20)
        FROM dual CONNECT BY LEVEL <= 2000;
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM events;
    IF n = 0 THEN
        INSERT /*+ APPEND */ INTO events (id, customer_id, kind, payload, at)
        SELECT LEVEL,
               MOD(LEVEL, 50) + 1,
               CASE MOD(LEVEL, 5)
                   WHEN 0 THEN 'login' WHEN 1 THEN 'view' WHEN 2 THEN 'click'
                   WHEN 3 THEN 'purchase' ELSE 'logout'
               END,
               'payload for event ' || LEVEL,
               CAST(DATE '2024-01-01' + LEVEL / 86400 AS TIMESTAMP)
        FROM dual CONNECT BY LEVEL <= 1000000;
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM big_text;
    IF n = 0 THEN
        DBMS_LOB.createtemporary(body, TRUE);
        FOR i IN 1 .. 4 LOOP                       -- 102400 characters
            DBMS_LOB.writeappend(body, LENGTH(chunk), chunk);
        END LOOP;
        INSERT INTO big_text (id, body) VALUES (1, TO_CLOB('short body'));
        INSERT INTO big_text (id, body) VALUES (2, NULL);
        INSERT INTO big_text (id, body) VALUES (3, body);
        DBMS_LOB.freetemporary(body);
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM binary_blobs;
    IF n = 0 THEN
        INSERT INTO binary_blobs (id, data) VALUES (1, TO_BLOB(HEXTORAW('0102030405')));
        INSERT INTO binary_blobs (id, data)
        VALUES (2, TO_BLOB(UTL_RAW.cast_to_raw(RPAD('AB', 2000, 'AB'))));
        COMMIT;
    END IF;

    SELECT COUNT(*) INTO n FROM all_types;
    IF n = 0 THEN
        INSERT INTO all_types VALUES (
            1, 12345.6789, 123.45, 1.25, 1.2345678901234,
            'abcde', 'varchar2 value', N'nvarchar2 value',
            DATE '2024-05-17', TIMESTAMP '2024-05-17 13:45:30.123456',
            TIMESTAMP '2024-05-17 13:45:30.123456 +02:00',
            INTERVAL '2 03:04:05.6' DAY TO SECOND,
            HEXTORAW('0102030405060708'), TO_CLOB('clob value'),
            TO_BLOB(HEXTORAW('AABBCC')));
        INSERT INTO all_types VALUES (
            2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, NULL, NULL, NULL);
        COMMIT;
    END IF;
END;
/

--------------------------------------- views, procedures, function, package
CREATE OR REPLACE VIEW v_customer_totals AS
SELECT c.id AS customer_id,
       c.name,
       COUNT(o.id) AS order_count,
       NVL(SUM(o.total), 0) AS total_amount
FROM customers c
LEFT JOIN orders o ON o.customer_id = c.id
GROUP BY c.id, c.name;

CREATE OR REPLACE VIEW v_recent_orders AS
SELECT o.id, o.ordered_at, o.status, o.total, c.name AS customer_name
FROM orders o
JOIN customers c ON c.id = o.customer_id
ORDER BY o.ordered_at DESC, o.id DESC
FETCH FIRST 100 ROWS ONLY;

CREATE OR REPLACE PROCEDURE customer_orders (
    p_customer_id IN  NUMBER,
    p_cur         OUT SYS_REFCURSOR
) IS
BEGIN
    OPEN p_cur FOR
        SELECT id, ordered_at, status, total
        FROM orders
        WHERE customer_id = p_customer_id
        ORDER BY ordered_at;
END;
/

CREATE OR REPLACE PROCEDURE mark_shipped (p_order_id IN NUMBER) IS
BEGIN
    UPDATE orders SET status = 'SHIPPED' WHERE id = p_order_id;
    COMMIT;
END;
/

CREATE OR REPLACE FUNCTION order_total (p_order_id IN NUMBER) RETURN NUMBER IS
    v_total NUMBER;
BEGIN
    SELECT NVL(SUM(qty * unit_price), 0) INTO v_total
    FROM order_items WHERE order_id = p_order_id;
    RETURN v_total;
END;
/

CREATE OR REPLACE PACKAGE order_pkg AS
    FUNCTION order_count (p_customer_id IN NUMBER) RETURN NUMBER;
    PROCEDURE cancel_order (p_order_id IN NUMBER);
END order_pkg;
/

CREATE OR REPLACE PACKAGE BODY order_pkg AS
    FUNCTION order_count (p_customer_id IN NUMBER) RETURN NUMBER IS
        v_count NUMBER;
    BEGIN
        SELECT COUNT(*) INTO v_count FROM orders WHERE customer_id = p_customer_id;
        RETURN v_count;
    END order_count;

    PROCEDURE cancel_order (p_order_id IN NUMBER) IS
    BEGIN
        UPDATE orders SET status = 'CANCELLED' WHERE id = p_order_id;
        COMMIT;
    END cancel_order;
END order_pkg;
/

EXIT
