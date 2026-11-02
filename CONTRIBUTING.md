# Contributing to Alamo

## Developer Certificate of Origin (DCO)

Every commit must be signed off with `Signed-off-by`:

```bash
git commit -s -m "…"
```

The sign-off certifies the [Developer Certificate of Origin 1.1](https://developercertificate.org/):
you have the right to contribute the code you are submitting, under the
project's license (Apache-2.0).

### License of contributions

Architecture §13. Every contribution enters under **Apache-2.0**, the same
license as the project (section 5 of the license): no CLA, the copyright stays
with whoever writes the code.

Alamo DB is **open core**: this repository is the Community edition, complete
and open source. There is also a paid Pro edition, with additional features in
separate, non-public code. Apache-2.0 allows anyone, the maintainer included,
to reuse the Community code also in non-open-source products: the license says
so, and we say it here so that it is clear before you contribute. The code you
contribute here remains available under Apache-2.0 to everyone, forever.

The DCO is not bureaucracy: it certifies the **origin** of the code (that it is
yours, or that you may contribute it). Adding it from the start costs five
minutes; reconstructing the origin of a contribution years later can be
impossible.

> The DCO does **not** grant relicensing rights. If one day the project wanted
> to change the license of the existing code in a way Apache-2.0 does not
> allow, a **CLA** would be needed, to be introduced **before the first
> external contribution**.

The "Alamo DB" name and the logo are not covered by the license: see
[`TRADEMARK.md`](TRADEMARK.md).

## Before opening a PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The integration tests inside `cargo test` skip themselves if the env vars
(`ALAMO_TEST_URL`, `ALAMO_MYSQL_TEST_URL`, ...) are not set. To run them on
the full matrix of §15.1 (PG 15/18, MySQL 8.0/8.4, MariaDB 10.11/11.8) you
need Docker:

```bash
cargo xtask test-matrix            # the whole matrix
cargo xtask test-matrix mariadb    # one engine, or `mysql:8.4` for one version
```

## Code rules enforced by CI

- **`alamo-core` does not depend on Tauri** and has no external dependencies
  (§2). This is what makes it possible to run spikes and integration tests
  from the CLI, without a webview.
- **No GPL/AGPL dependencies**, not even transitive ones (`deny.toml`, §13).
- No `unsafe` in `alamo-core` and `alamo-driver-postgres`
  (`#![forbid(unsafe_code)]`). `alamo-bench` is the exception: it reads the
  RSS from the operating system.

## Code rules CI **cannot** enforce

These are the most important ones, and they all come from mistakes documented
in `ARCHITECTURE.md`:

1. **A column's identity is its position, never its name** (§4.3).
   `SELECT a.id, b.id` produces two `id` columns. Any `name → column` map is
   already a bug.
2. **No per-cell enum** (§4.2). The enum lives at column level. `ValueRef` is
   a temporary view, never storage.
3. **Memory is computed from the layout, never with a counter** (§4.8).
   Hand-maintained counters always drift.
4. **The memory budget is in bytes, never in rows** (§5). 50 TEXT columns and
   3 INT columns have the same number of rows and two orders of magnitude of
   difference in RAM.
5. **No rewriting of the user's SQL** to inject `LIMIT` (§5.2). It breaks edge
   cases and shifts error positions. Limits are applied at protocol level.
6. **A non-representable value degrades the column to `Utf8`, it is not
   rounded** (§4.6). MySQL zero-dates, MySQL `TIME` (which is a duration), PG
   `infinity`: ignoring them produces a crash or, worse, a wrong value that
   looks like a right one.
7. **NULL, the empty string and the string `"NULL"` remain three distinct
   things** along the whole pipeline, IPC included (§8.2).
