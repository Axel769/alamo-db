# Benchmarks — large result sets

**Date:** 2026-11-11 · **Machine:** Apple M3 Pro, 18 GB, macOS 26.6.2 ·
**Servers:** PostgreSQL 18.6 and MySQL 8.4.11 in Docker 29.5 on the same Mac
(8 CPUs and 4 GB for the Docker VM), default configuration · **Build:**
release (`cargo build --release -p alamo-bench`), Rust 1.99.1.

## What is measured

`alamo-bench launch` (`crates/alamo-bench/src/launch.rs`) goes through the
path the app takes, not a driver loop: a `Session` (§7.2) with the engine's
driver, the user's `SELECT * FROM <table>` streamed into the columnar result
set (§4), then the windows the grid asks for while scrolling (§8). No
`LIMIT`, no rewriting of the SQL (§5.2); the byte budget and the row pause
(§5) are off, so the whole stream is measured.

Dataset: the canonical M0 tables (`dataset.rs`), 1,000,000 rows each, the
same values on both engines:

- **T1 narrow**: 6 columns (bigint, integer, double, boolean, timestamp), ~60 B/row;
- **T2 wide**: 14 columns with UUID, text with 15% NULLs, numeric, two
  timestamps, JSON and an array, ~500 B/row;
- **T3 60 int**: 60 integer columns, the per-column overhead.

Columns of the tables below, median of 5 runs:

- **First rows**: from `execute` to the first closed chunk, what the user
  waits before the grid shows something. The first chunk is 1,024 rows on
  purpose (§5), the following ones up to 64k rows.
- **All rows**: to the end of the stream; rows/s from it.
- **Result set**: its bytes as the columnar layout counts them (§4.8), the
  figure the memory budget uses.
- **Peak RSS**: of the whole bench process, one table per process. On macOS
  RSS does not count compressed pages, so it can be below the result set:
  the result set column is the one to compare.
- **Scroll window**: 60 rows × all columns at random positions (2,000
  samples), the request of one repaint of the grid.

## Results

### PostgreSQL 18.6

| Table | Rows | First rows | All rows | Rows/s | Result set | Peak RSS | Scroll window p50 / p99 |
|---|---|---|---|---|---|---|---|
| T1 (narrow) | 1,000,000 | 1 ms | 177 ms | 5.66 M | 35.2 MB (36 B/row) | 46.2 MB | 23 µs / 31 µs |
| T2 (wide) | 1,000,000 | 3 ms | 1,252 ms | 799 k | 642.7 MB (673 B/row) | 443.9 MB | 48 µs / 59 µs |
| T3 (60 int) | 1,000,000 | 3 ms | 1,459 ms | 686 k | 236.1 MB (247 B/row) | 252.8 MB | 52 µs / 64 µs |

### MySQL 8.4.11

| Table | Rows | First rows | All rows | Rows/s | Result set | Peak RSS | Scroll window p50 / p99 |
|---|---|---|---|---|---|---|---|
| T1 (narrow) | 1,000,000 | 1 ms | 548 ms | 1.82 M | 38.9 MB (40 B/row) | 50.5 MB | 20 µs / 24 µs |
| T2 (wide) | 1,000,000 | 3 ms | 1,519 ms | 658 k | 647.1 MB (678 B/row) | 537.0 MB | 43 µs / 54 µs |
| T3 (60 int) | 1,000,000 | 3 ms | 1,807 ms | 553 k | 236.1 MB (247 B/row) | 252.5 MB | 53 µs / 62 µs |

## Reading them

- The first rows arrive in 1-3 ms whatever the size: the grid is never
  waiting for the end of the stream.
- A million rows of the wide table, JSON included, are in memory in 1.3 s
  (PG) and 1.5 s (MySQL), at about 1.3× their nominal size: the layout has
  no per-row objects (§4).
- A repaint of the grid costs tens of microseconds at any position in the
  million rows, against 16.7 ms of a 60 Hz frame: scrolling does not depend
  on the size of the result.
- On PG the narrow table runs at its ceiling: reading the same rows and
  discarding them, with nothing built, gives 5.59 M rows/s (`alamo-bench
  run --table t1 --no-budget`, same server). MySQL is slower there: its
  text protocol sends every number as a string to parse.

Not measured here: the webview side (frame time while scrolling, see
`docs/m0-phase2-results.md`), remote servers (the network dominates), and
other tools, which have no command line to drive the same way.

## Reproduction

```sh
docker run -d --rm --name alamo-bench-pg -e POSTGRES_PASSWORD=alamo -e POSTGRES_DB=alamo_bench -p 55432:5432 postgres:18
docker run -d --rm --name alamo-bench-my -e MYSQL_ROOT_PASSWORD=alamo -e MYSQL_DATABASE=alamo_bench -p 53306:3306 mysql:8.4
cargo build --release -p alamo-bench
B=./target/release/alamo-bench
PG=postgres://postgres:alamo@127.0.0.1:55432/alamo_bench
MY=mysql://root:alamo@127.0.0.1:53306/alamo_bench
$B prepare --url $PG --rows 1000000 --force
$B prepare --url $MY --rows 1000000 --force
for u in $PG $MY; do for t in t1 t2 t3; do $B launch --url $u --table $t --reps 5; done; done
```
