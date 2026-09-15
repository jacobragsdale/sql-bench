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
