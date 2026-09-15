# sql-bench performance

Measured numbers and the machine they came off. The budgets they are
measured against are in [DESIGN.md](DESIGN.md); *(T7.1)* fills in the rest
of this file once there is an app to time.

## Environment

Measured 2026-09-14 on the development machine:

| | |
|---|---|
| CPU | AMD Ryzen 7 8745HS, 16 threads (`nproc`) |
| memory | 27 GiB total (`free -h`) |
| OS | Arch Linux, kernel 7.2.3 |
| docker | 29.8.0, compose 5.5.1 |
| rust | 1.98.0 |

`scripts/db-reset.sh` — both containers destroyed with their volumes,
recreated, waited for and seeded — took **46 s** wall clock, images already
pulled. Most of it is the two servers starting: the seeding itself is the
tail.

`db-up.sh` prints its own seed time at the end of each line. Into empty
databases that was 1 s for SQL Server and 9 s for Oracle — the same
1,000,000-row `events` table either way, and one set-based insert either
way; Oracle is simply slower at it. Run again against databases that are
already seeded, both drop to 0-1 s, since both seed files skip what already
exists.

## Queries

One block per run of `scripts/perf.sh`, against the containers
`scripts/db-up.sh` seeds. Milliseconds, nearest-rank percentiles; `connect`
is the one connect each run of `bench` makes, `rows/s` is over every run.

### 2026-09-14 (a7d0bf8) — 20 runs, 5 for the 100k scans

**local-mssql**

| query | rows | connect | first_row p50 | first_row p95 | total p50 | total p95 | rows/s |
|---|---|---|---|---|---|---|---|
| select 1 | 1 | 5 | 0 | 0 | 0 | 0 | 20000 |
| customers, top 100 | 50 | 4 | 0 | 0 | 0 | 0 | 1000000 |
| events, 10k cap | 10000 | 4 | 3 | 5 | 13 | 14 | 787401 |
| events, 100k cap | 100000 | 5 | 4 | 5 | 95 | 106 | 1026694 |

**local-oracle**

| query | rows | connect | first_row p50 | first_row p95 | total p50 | total p95 | rows/s |
|---|---|---|---|---|---|---|---|
| select 1 | 1 | 39 | 0 | 0 | 0 | 0 | 20000 |
| customers, top 100 | 50 | 38 | 0 | 0 | 0 | 0 | 333333 |
| events, 10k cap | 10000 | 38 | 0 | 0 | 8 | 10 | 1242236 |
| events, 100k cap | 100000 | 38 | 0 | 1 | 77 | 102 | 1216545 |
