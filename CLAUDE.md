# CLAUDE.md

Alamo DB: desktop database tool (PostgreSQL, MySQL, MariaDB), Tauri 2 + Rust + React.
Dominant criterion: **performance on large result sets**.

Alamo DB is a **professional tool for developers**, used by a team on Windows, macOS
and Linux against real servers, not a side project that runs on one Mac. Every
change is judged as in DataGrip or DBeaver: it works on every OS and on servers with
limits (connections, privileges, thousands of tables), failures say what happened and
what to do in the user's words, with the technical detail one click away and
copyable, and nothing important hides behind a tooltip or a hover.

`ARCHITECTURE.md` is the source of truth for every decision; `§N` in code comments
points there. Read the relevant section before changing a mechanism.
`CONTRIBUTING.md` holds the code rules.

## Layout

```
crates/alamo-core/             columnar model, Dialect trait, errors, splitter. No deps, no Tauri
crates/alamo-driver-postgres/  PG driver (tokio-postgres)
crates/alamo-driver-mysql/     MySQL + MariaDB driver (mysql_async)
crates/alamo-session/          registry, streaming, budget, cancel, export
crates/alamo-tunnel/           SSH tunnel (russh)
crates/alamo-bench/            M0 benchmarks
app/src-tauri/                 Tauri commands: serialization and wiring only
app/src/                       React UI: Glide grid, CodeMirror, Zustand, TanStack Query
xtask/                         dev tooling (separate workspace)
docs/                          measurements and optimization backlog
```

Rust owns the data (connections, result sets, schema cache). The UI asks only for
the visible window and is never the source of truth (§8, §9).

## Commands

```bash
cargo xtask run                    # build frontend + app, sign, launch
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace             # integration tests skip without ALAMO_*_TEST_URL
cargo xtask test-matrix [mysql]    # full engine matrix via Docker (§15.1)
cargo xtask e2e                    # app with remote control for end-to-end tests
(cd app && pnpm build)             # tsc --noEmit + vite build
```

Run fmt, clippy and test before saying a change is done. Run `test-matrix` on the
affected engines when touching drivers, session or cancel.

## Rules that are easy to break

- `alamo-core` gets no dependencies and never depends on Tauri (CI fails).
- A column is identified by **position**, never by name (§4.3).
- Memory budget in **bytes**, computed from the layout, never a hand-kept counter (§4.8, §5).
- No automatic `COUNT(*)`, no rewriting the user's SQL (§5.2).
- Dialect differences go through `Capabilities`, never `if dialect == …` in the UI (§2.1).
- Row cache and CodeMirror document live **outside React** (§9.2).
- No GPL/AGPL dependencies, not even transitive (`deny.toml`).
- Passwords never touch the disk (§12).

## How we write code

- **No duplication.** Repeated logic goes into a focused module of functions named after
  what it does (like `bitmap.rs`, `varbuf.rs`), in the crate that owns the concept; if
  several crates need it, in the lowest one they share. Frontend: a module in `app/src/`.
- **Separate crate** for code that is not about Alamo's domain (as `alamo-tunnel`).
  Propose it before creating it.
- **No dead code.** Delete it: no `#[allow(dead_code)]`, no commented-out code, no
  "kept for later". Unused exports, files and dependencies go too.
- **Errors and logs short and meaningful**: what failed and the identifier that
  matters (connection, statement position, column). No payload dumps, no secrets.
  There is no logging crate: errors are typed (§6) and reach the UI. Do not add
  one, or `println!` in library crates, without asking.
- **Repo essential only.** No new top-level files, docs or scripts unless needed;
  measurement output stays in gitignored folders.
- A new non-obvious decision goes into `ARCHITECTURE.md` with its rationale.

## Commits and release

- Conventional Commits in English: `fix(grid): …`, `feat(session): …`.
- Commits signed off: `git commit -s` (DCO, see `CONTRIBUTING.md`).
- User-visible changes go under "Unreleased" in `CHANGELOG.md`.
- Release: `scripts/release-alpha.sh` from the Mac (signing keys stay local) or the
  `Release (alpha)` workflow. The official build is an extension build that calls the
  same script with `--app` (ARCHITECTURE §13). All of them publish to a public repo:
  never run without explicit confirmation.
