# Alamo DB

Desktop database editor/tool. PostgreSQL, MySQL, MariaDB.
The dominant design criterion: **performance on large result sets**.

The architecture document is [`ARCHITECTURE.md`](ARCHITECTURE.md): it is the
source of truth for every decision. The `§N` references scattered through the
code comments point there, and they are not decoration — the counterintuitive
layout choices (inverted validity bitmap, `i32` offsets, irreversible
degradation to `Utf8`) have a rationale there that cannot be reconstructed
from the code.

**Status: alpha** (`0.1.0-alpha.12`), macOS and Windows. What is there:

- PostgreSQL, MySQL and MariaDB, over TLS and SSH tunnels; passwords in the
  system keychain, never on disk (§12)
- SQL editor with multiple tabs, schema-aware autocomplete and autosave
- streaming result grid built for large result sets, with cancel (§4, §7)
- schema tree, table tabs with Data/Structure/DDL, foreign-key navigation
- in-grid editing with transactions, manual commit and production
  safeguards (confirmation of writes, `UPDATE`/`DELETE` without `WHERE`)
- export to CSV, TSV, JSON and `INSERT`
- English and Italian interface

Builds and the in-app updater use the
[releases of this repository](https://github.com/Axel769/alamo-db/releases).
User-visible changes are listed in [`CHANGELOG.md`](CHANGELOG.md).

## Structure

```
crates/alamo-core/             columnar data model, schema, errors, Dialect trait
crates/alamo-driver-postgres/  PG driver: connection, dialect, binary decoding
crates/alamo-driver-mysql/     MySQL and MariaDB driver
crates/alamo-session/          registry, streaming, cancel, budget, escalation, export
crates/alamo-tunnel/           SSH tunnel
crates/alamo-bench/            M0 spike: measures socket -> columnar layout
app/src-tauri/ + app/src/      Tauri 2 app: thin commands, React + CodeMirror + Glide
xtask/                         dev tooling: run, bundle, test matrix, e2e
```

`core` **does not depend on Tauri** and has no external dependencies (a
non-negotiable constraint from §2). A CI job fails if anyone violates it.

## Running the app

Prerequisites: Rust stable, Node + [pnpm](https://pnpm.io), a reachable
PostgreSQL or MySQL/MariaDB database.

```bash
(cd app && pnpm install)        # first time only, or when dependencies change

cargo xtask run                 # from the repo root: build + sign + launch
```

`cargo xtask run` is the normal way to start the app in development. In order:

1. rebuilds the frontend (`pnpm build` in `app/`): Tauri embeds `app/dist` at
   compile time, without this step the old UI would run;
2. compiles `alamo-app` in debug;
3. on macOS signs the binary with the identity written in
   `~/.tauri/alamo-sign-identity` (override: `ALAMO_SIGN_IDENTITY`; with
   neither, "Apple Development", which is ambiguous if the Keychain holds
   several development certificates), so the Keychain does not ask for
   permission at every build; without an identity it launches anyway,
   unsigned, with a warning;
4. launches the app.

Alternatives, when needed:

```bash
# frontend with hot reload: two terminals
(cd app && pnpm dev)            # terminal 1 — vite on :1420
cargo run -p alamo-app          # terminal 2 — debug build, loads devUrl

# optimized build
(cd app && pnpm build)
cargo build --release -p alamo-app
./target/release/alamo-app      # loads app/dist

# distributable bundle (macOS: .app + .dmg in target/release/bundle/)
cargo xtask bundle
```

`cargo xtask bundle` runs `pnpm tauri build`, signing with the same identity
as `cargo xtask run` (or with `APPLE_SIGNING_IDENTITY`). The bundle works on
this Mac and on those that accept the Gatekeeper warning; distributing it over
the internet requires a Developer ID certificate and notarization, not yet
configured. The icons in `app/src-tauri/icons/` are generated from `icon.svg`
with `pnpm tauri icon` (rerun it when the SVG is replaced).

At startup the connections dialog opens: profiles are persisted in SQLite
(`~/Library/Application Support/io.github.axel769.alamo/alamo.db`), the
password is typed at every connect or saved in the system keychain.

Environment variables (read by the `alamo-app` process):

| Variable | Effect |
|---|---|
| `ALAMO_M1_SMOKE=1` | e2e smoke: auto-connect to `ALAMO_BENCH_URL`, results on stdout |
| `ALAMO_M0_AUTO=1` | M0 phase 2 frame-time harness (with `ALAMO_M0_PROFILE`, `ALAMO_M0_OUT`) |
| `ALAMO_BENCH_URL` | PG URL for bench and smoke (never used by the normal user path) |

The session integration tests (`crates/alamo-session/tests/`) run only with
`ALAMO_TEST_URL` (PostgreSQL) and `ALAMO_MYSQL_TEST_URL` (MySQL/MariaDB) set;
`cargo xtask test-matrix` runs them on the whole version matrix with Docker
(see `CONTRIBUTING.md`).

## M0 bench — how to run it

It needs **a single PostgreSQL instance** (the latest). The version matrix of
§15.1 is an M1 and M3 problem and does not block M0.

```bash
# toolchain
rustup toolchain install stable

# local server
docker run --rm -d -p 5432:5432 \
  -e POSTGRES_PASSWORD=alamo -e POSTGRES_DB=alamo_bench \
  --name alamo-pg postgres:18

export ALAMO_BENCH_URL='postgres://postgres:alamo@localhost:5432/alamo_bench'

# canonical dataset (§15): T1 narrow, T2 wide, T3 60 integer columns
cargo run -p alamo-bench --release -- prepare --rows 1000000 --force

# measure
cargo run -p alamo-bench --release -- all --reps 5
```

### Why the method matters as much as the numbers

1. **"1M rows" is not a benchmark.** Six integer columns and fourteen columns
   with JSON and long text differ by an order of magnitude. The schema is
   fixed in `crates/alamo-bench/src/dataset.rs`.
2. **Measure the ceiling first.** `measure_ceiling` does a *read and discard*
   that builds nothing: it says how much the socket and the server can deliver
   on that machine. The real metric is the **distance from the ceiling**, not
   the absolute value.
3. **Fixed environment.** Release build with LTO, same machine, local server,
   no other load, 5 repetitions, median.

The thresholds are in `crates/alamo-bench/src/report.rs`, in three bands, with
the action to take already written next to each. **They were fixed before
running the spike**: it is the only way not to rationalize the results after
seeing them.

### What this phase does NOT measure

- **frame time on continuous scroll of 20k rows** — it is the headline metric
  of §15, and it requires Tauri + Glide (phase 2). "Zero frames above 50 ms"
  weighs more than the average: a single 200 ms hitch is far more noticeable
  than a constant 20 ms;
- cold start;
- Windows and macOS as real gates (phase 3);
- Linux/WebKitGTK smoke test — informative, not blocking (§1.2), but **it is
  the only Linux item that cannot be recovered later**.

## Constraints active from day one

| What | Where | Why now |
|---|---|---|
| Apache-2.0 (open core) | `LICENSE` | §13 |
| DCO | `CONTRIBUTING.md` | §13 — certifies the origin of every contribution; reconstructing it years later can be impossible |
| Trademark | `TRADEMARK.md` | §13 — the name is not covered by the code license: it is the protection against forks that present themselves as Alamo |
| `cargo-deny` with GPL/AGPL deny-list | `deny.toml` + CI | §13 — blocks the transitive dependency that sneaks in six months from now |
| `cargo-about` | `about.toml` | §13 — attribution of the Rust dependencies |
| Bundle id `io.github.axel769.alamo` | `tauri.conf.json` | §13.1 — fixed (decision 5b closed): defined only there, keychain and data path are derived from it |

## License

Apache-2.0 (includes the patent clause). See [`LICENSE`](LICENSE) and
[`NOTICE`](NOTICE).

This repository is the Community edition of Alamo DB, open source and
complete. A paid Pro edition adds features in separate code (open core,
architecture §13). The "Alamo DB" name and the logo are not covered by the
code license: see [`TRADEMARK.md`](TRADEMARK.md).
