# Alamo — Architecture Document

**Status:** decisions consolidated before implementation
**Current phase:** M0 (validation spike) not yet started
**Last updated:** 26 July 2026

Living document. Sections marked `[TBD]` are open decisions; those marked
`[VERIFY M0]` are assumptions to validate with measurements before considering them final.

---

## 0. Goal and scope

Desktop database editor/tool, an alternative to DBeaver with a modern interface.

**Supported dialects (v1):** PostgreSQL, MySQL, MariaDB.
MySQL and MariaDB share the protocol: they are two dialects on top of a single driver.
PostgreSQL is the second real implementation.

**Dominant project criterion:** performance on large result sets.

**Target platforms (v1):** **Windows and macOS**.
**Linux: not a v1 target**, but the decision can be reopened — see §1.2 for the single check
that must not be postponed.

**Non-goals for v1:** SQLite, SQL Server, Oracle, MongoDB. The `Dialect` abstraction
must nevertheless make them addable without touching the UI.

---

## 1. Technology stack

| Layer | Choice | Short rationale |
|---|---|---|
| Desktop shell | **Tauri 2** | 10–20 MB binaries, modest RAM, Rust backend |
| Backend | **Rust** | No GC, zero-copy columnar layout, real streaming |
| Frontend | **React + TypeScript** | Constraint imposed by Glide Data Grid |
| Grid | **Glide Data Grid** | Canvas rendering, handles millions of cells |
| SQL editor | **CodeMirror 6** | Lightweight, autocomplete API suited to context-aware completion |
| PG driver | **tokio-postgres** | Access to OIDs, binary formats, cancel token |
| MySQL driver | **mysql_async** | Pure Rust protocol (no GPL constraint from libmysqlclient) |
| State mgmt | **Zustand** + **TanStack Query** | The former for UI/tabs, the latter for the schema |
| Local persistence | **rusqlite** (bundled) | History, snippets, profiles, autosave |
| TLS | **rustls** | Explicit handling of sslmode modes |
| SSH tunnel | **russh** | — |
| Credentials | **keyring** (OS keychain) | Never in plain text on file |

### 1.1 Rejected choices and why

- **sqlx** — the abstraction hides exactly what a DB tool needs (cancellation,
  `COPY`, `LISTEN/NOTIFY`, prepared statement metadata, server notices), and
  compile-time checking is useless with fully dynamic SQL.
- **Electron** — protocol parsing in JS, one object per row: 3–5× slower and
  much hungrier for RAM on large datasets.
- **.NET 9 + Avalonia** — excellent runner-up (70–80% of the performance with half
  the complexity); rejected because the dominant criterion is performance.
- **Flutter desktop** — custom rendering, weak text/selection handling and accessibility,
  no mature code editor.
- **Qt** — technically excellent (`QTableView` with a virtual model) but expensive licence
  and low productivity.
- **arrow-rs at MVP** — postponed: painful mapping of exotic PG types. The layout stays
  convertible, though (see §4.2).

### 1.2 Target platforms and the WebKitGTK question

**v1 target: Windows and macOS. Linux out of the v1 scope.**

Tauri does not ship a rendering engine: it uses the system webview.

| OS | Webview | Canvas risk | Status |
|---|---|---|---|
| Windows | WebView2 (Chromium) | low | v1 target |
| macOS | WKWebView | low | v1 target |
| Linux | WebKitGTK | ⚠️ high | **out of v1** |

**Positive consequence:** without Linux, the architecture's number one risk almost
entirely disappears. WebView2 and WKWebView handle canvas well → **Tauri becomes a
safer bet**, and M0 has a very high probability of passing.

#### What can be postponed and what cannot

The Linux work splits into two categories with opposite logic:

| Category | Items | When |
|---|---|---|
| **Expensive but postponable without risk** | .deb/.rpm/AppImage/Flatpak packaging, updater strategy, keychain fallback with master password (§12.1), distro matrix, bundled font | **M5 / never** — does not touch the architecture, doing it tomorrow costs the same as doing it today |
| **Cheap but irreversible** | *"does the Glide canvas hold up on WebKitGTK?"* | **NOW** (§15, M0) |

⚠️ The second item is the only one that cannot be recovered later: if the answer were *no* and
it were discovered at M3, a bet on the stack would have been made with months of code on top.
**Verifying it costs half a day** — an Ubuntu VM, the M0 prototype that gets built
anyway, a frame time measurement. No packaging, no keychain, no distro matrix:
those are needed to *distribute* on Linux, not to *know* whether Linux is possible.

#### If Linux is ever reopened

Plan B, in order of increasing cost:

1. **Controlled degradation** — fewer visible rows, scroll throttling, reduced density.
2. **Alternative renderer** — virtualized DOM grid (TanStack Virtual) instead of the canvas.
   → **One more reason to isolate Glide behind an interface of our own** (`DataSource` +
   replaceable grid component). Worth it regardless of Linux: Glide's
   selection/editing model might not fit.
3. **Stack change** — .NET/Avalonia or Qt, which do not depend on a system webview.

Technical notes to recover if reopened: Tauri 2 requires `webkit2gtk-4.1`
(excludes old distros); the `WEBKIT_DISABLE_COMPOSITING_MODE` /
`WEBKIT_DISABLE_DMABUF_RENDERER` workarounds, often necessary on NVIDIA drivers, **make
canvas performance worse, precisely**; X11 and Wayland have different rendering paths.


---

## 2. Workspace structure

Four crates. **Non-negotiable constraint: `core` does not depend on Tauri.**
This allows running spikes and integration tests from the CLI, without a webview.

```
core/              Value, Column, ColumnSet, schema model, errors,
                   Dialect trait, Capabilities
driver-postgres/   PG implementation
driver-mysql/      MySQL + MariaDB implementation (internal flag)
session/           sticky pool per tab, result set registry, execution,
                   cancellation, memory budget
app/ (src-tauri)   thin commands: serialization and wiring only
```

### 2.1 `Dialect` trait and `Capabilities`

The trait covers: introspection, identifier quoting, type mapping,
pagination, cancellation, execution plans.

**Features specific to a single dialect do NOT go into the trait** with empty
methods in the other implementation. They are exposed via `Capabilities`, which the UI queries:

```
supports_schemas, supports_explain_analyze, supports_listen_notify,
supports_copy, editable_without_pk, can_cancel, can_change_database_in_session, …
```

Goal: zero `if dialect == Postgres` scattered through the frontend.

### 2.2 Driver registry (open core extension point, 2026-10-29)

The session no longer knows the engines by name. `alamo-session::driver`
defines two traits and a registry:

- `Connection`: what a lane can do (execute in streaming, cancel, ping,
  read-only, introspection, …). The session holds `Arc<dyn Connection>`.
- `Driver`: opens connections of one engine and describes it with
  `info()` (`EngineInfo`): the id is the `dialect` saved in the profile
  (`postgres`, `mysql`), then name, default port and user, whether the
  database is required, the `sslmode` values it honours. The connection
  dialog (`engines` command, `app/src/engines.ts`) and the import check are
  built from these, so a registered engine appears in the UI without the
  frontend knowing its name. `conn_string()` writes the string `connect`
  receives from a `ConnectTarget` (host, port, user, password, database,
  TLS mode, tunnel port): libpq key/value on PG, a percent-encoded
  `mysql://` URL on MySQL. The app turns a profile into a target and
  never writes an engine's syntax itself.
  `EngineInfo` also carries the dialect's `Capabilities` and a
  `sql_syntax` id: the frontend reads them from the profile's engine
  (`canSwitchDb`, `transactionalDdl` in `engines.ts`), even with the
  connection closed, and picks the editors' CodeMirror dialect and
  identifier quoting from `sqlSyntax.ts` (standard SQL for a syntax it
  does not know). This is how §2.1's "zero `if dialect ==` in the
  frontend" holds.
- `Drivers`: the drivers of a build. `Drivers::builtin()` has PostgreSQL and
  MySQL/MariaDB; the official Pro build calls `register` with its own
  crates (§13). The app picks the driver by the profile's `dialect` and
  hands it to `Session::connect_opts`, which keeps it to reopen lanes.

**Why trait objects instead of the closed enum.** The enum made the
compiler list every spot when an engine arrived, but only inside this
repository: a Pro engine living in a private crate could not add a variant.
`dyn` costs one indirect call per method, and the hot path calls per chunk,
not per row (`execute_streaming` receives `&mut dyn FnMut(StreamEvent)`).

**No default methods.** Engine-specific behaviour (catalog `search_path`,
read-only transaction, `SHOW WARNINGS`, killing abandoned connections) is
implemented by every driver, as a no-op where it does not apply. A default
would let a new driver skip read-only or the catalog pin without anyone
noticing — the guarantee the enum gave is kept by the trait.

**Differences become capabilities.** The session's last `if driver ==`
became `Capabilities` (§2.1): `read_only_txn` (PG wraps read-only executions
in a `READ ONLY` transaction, MySQL refuses dynamic SQL instead) and
`can_change_database_in_session` (MySQL syncs the `USE` before an
`EXPLAIN`). `as_any` exists only for the M0 bench, which needs the
concrete PG type.

**The traits live in `alamo-session`, not in `alamo-core`:** their
signatures carry tokio types (the control channel) and `alamo-core` has no
dependencies (§2). A Pro driver depends on `alamo-session`.

---

## 3. Connection model

It is not "a pool". It is three distinct things:

1. **Sticky connection per tab/editor** — transactions and session state (`SET`,
   `search_path`, temp tables) live there and cannot migrate between connections.
2. **Dedicated introspection connection** — otherwise browsing the schema during
   an open transaction blocks or reads inconsistent data. **Statistics**
   (sizes and counts per database, only on the user's request) have a
   separate, lazy lane: on servers with many databases the query takes tens of
   seconds and must not make the list and the tree wait. The whole-server
   autocomplete catalog (§10) runs there too: it starts at every connect and
   on a server with 20,000 tables it held the introspection lane for seconds
   (the table list of a database: 6 s instead of 0.1 s).
3. **Out-of-band cancellation channel** — by definition outside the connection
   that is executing (see §7).

Also: **heavy fetches run on a dedicated connection**, not on the sticky one,
when there is no open transaction (see §5.1, MySQL trap).

### 3.1 *Lazy* sticky — correction imposed by table tabs

The "one sticky per tab" rule does not hold with table tabs (§9.5): 30 open tabs
would become 30 connections to the server, unacceptable on production.

| Tab type | Connection |
|---|---|
| SQL tab | **sticky from the start** — the user can write `SET` at any moment |
| Read-only table tab | **shared connection** from the pool |
| Table tab in editing or with an open transaction | **acquires a dedicated sticky**, releases it on commit/rollback |

The switch from shared to dedicated is a state transition of the tab, to be modelled
explicitly (and to be shown in the UI: the user must know when a tab "holds"
a connection).

`reset_connection` primitive planned from the start.
Note: in PG `DISCARD ALL` destroys temp tables and prepared statements — do **not** use it
as an automatic reset, only on explicit request.

### 3.2 User connection limit — the tree borrows the sticky

Real servers cap the connections per user (`max_user_connections`, often 3),
and the user's other programs (HeidiSQL, DBeaver) count against the same cap.
When the server refuses the introspection lane and no other auxiliary lane is
open to reuse, the tree **borrows the sticky** instead of failing, as DBeaver
does with "separate connection for metadata" off:

- only between executions: with one running or a paused fetch the tree fails
  at once with an explanation and Retry, it never queues behind the user's query;
- after a refusal the tree stays on the sticky for 60 s before trying to open its
  own lane again (each attempt is a login and, over SSH, a channel);
- statistics never borrow it: their query can take tens of seconds and the
  editor would wait;
- accepted cost: a catalog read resets the diagnostics of the user's last
  statement (`SHOW WARNINGS`, `ROW_COUNT()`). It changes nothing else: no
  `USE`, no transaction effect (plain `SELECT` on `information_schema`).

MySQL/MariaDB only, because only there the limit is recognised (§6.2). On
PostgreSQL the sticky has no `search_path` pinned to `pg_catalog` and must not
serve catalog reads.

**Connections abandoned after a sleep** (2026-10-29). When the PC sleeps the
network or the SSH tunnel dies and closing a connection never reaches the
server: it keeps it in `Sleep` until `wait_timeout` (8 hours by default),
counted against the cap. Alamo closes them itself with `KILL <id>` from a
new connection of the same user (no privilege needed):

- inside a session, a lane reopened because it was dead closes the old one;
- across sessions, a closed session's server connections (`ServerConnId`:
  server, server start, id) are saved per profile in SQLite and the next
  connect of the profile closes them. Without this, Disconnect + Connect
  after a sleep left 1 seat out of 3, and the next sleep none.

An id is meaningful only within one life of one server, and a proxy that
shares server connections between clients (ProxySQL, RDS Proxy, MaxScale)
could hand it to someone else. So a `KILL` needs all of: same server
(`@@server_uuid`, `@@hostname` on MariaDB, which has no UUID), same server
start (from `Uptime`, ±5 s), the id shown by `PROCESSLIST` as **idle
(`Sleep`) and of the same user**; saved ids are dropped after 24 hours.
A connection running something is never killed. Limit: if the zombies
already fill the cap, no connection opens to close them; only the server's
timeout or an administrator frees them.

### 3.3 PostgreSQL — one connection per database

A PG connection is bound to one database (`can_change_database_in_session`
false), while the user sees the server's databases as DBeaver does
(connection → databases → schemas → tables). Each other database is a
**sibling connection**: a full session of its own (sticky, lanes, registry,
transaction), opened lazily with `open_database` the first time it is used.

- Opened from what the profile's connection kept in memory at connect — the
  profile and the password actually used (§12: never on disk) — with the same
  TLS regime, prod/read-only flags and the same SSH tunnel port.
- The tunnel stays owned by the profile's connection: a sibling that finds it
  dropped rebuilds that one. Closing the profile's connection closes its
  siblings; closing a sibling touches nothing else.
- One sibling per (connection, database): opening it again, or twice at once,
  gives back the same one; the profile's database gives back the profile's
  connection.
- Tabs keep their database (`db`, saved in the workspace). After a restart or
  a reconnection they come back detached; the profile's connection takes
  back those on its database, a sibling those on its own when it opens, and
  the active tab opens its database's sibling. Never all at once: restoring
  a workspace must not open one connection per database.
- The profile's database filter (`db_include`/`db_exclude`) applies to the
  server's databases, as the schemas on MySQL; PG schemas are not filtered.
- Rejected: switching the database on the existing sessions. Session state
  (transaction, `SET`, temp tables) would be lost at every switch, and every
  catalog read would have to name its database.

---

## 4. Data model

### 4.1 Entities

An execution does **not** necessarily produce a result set, and it can produce **more than one**
(`CALL` of a MySQL stored procedure). Model `Vec<ExecutionOutcome>` from the start.

```
ExecutionOutcome
 ├─ RowSet(ResultSet)
 ├─ Affected { rows, last_insert_id }   // DML
 └─ Void                                 // DDL, SET, …

ResultSet
 ├─ schema: Arc<[ColumnMeta]>     // once only, NEVER per chunk
 ├─ chunks: Vec<Chunk>
 ├─ row_starts: Vec<u64>          // prefix sum for addressing
 ├─ state: Streaming | Paused | Complete | Truncated | PartialCancelled
 └─ bytes: u64

Chunk
 ├─ rows: u32
 ├─ columns: Vec<Column>          // same order as the schema
 └─ lobs: LobArena

Column
 ├─ validity: Bitmap
 ├─ null_count: u32
 └─ data: ColumnData
```

### 4.2 The three layout rules (Arrow convertibility at ~zero cost)

1. **Validity bitmap separate from the data.** Bits packed LSB-first,
   **1 = valid, 0 = NULL** (the opposite of intuition). Never `Option<T>` per element.
   Explicit `null_count`: allows skipping the check on columns without NULLs.
2. **Varlen as offsets + contiguous buffer.** `offsets: Vec<i32>` of length `rows+1`,
   first element 0, monotonic; `data: Vec<u8>`. Value *i* = `data[offsets[i]..offsets[i+1]]`.
   NULL = repeated offset + validity bit at 0. Never `Vec<String>`.
3. **No enum per cell.** The enum sits at the *column* level (`ColumnData`).
   `Value` exists only as a temporary view built on the fly for the UI, never as storage.

**Alignment:** Arrow prefers 64-byte aligned buffers, `Vec<u8>` does not guarantee it.
A one-off `memcpy` at conversion time is accepted. Not a problem at MVP.

### 4.3 `ColumnMeta` — capture at execution time

It arrives in the `RowDescription` (PG) / column definition (MySQL) **at execution
time**. Afterwards it cannot be reconstructed.

- `name` (output name)
- `native_type` — OID in PG; `ColumnType` + flags in MySQL (`UNSIGNED`, charset to
  distinguish `BINARY` from text, `ZEROFILL`)
- `type_display` — name shown to the user
- `nullable` — MySQL says it, PG does not for computed columns
- `source: Option<…>` — `{table_oid, column_attnum}` in PG,
  `{db, org_table, org_name}` in MySQL
- **`len_p95: Option<u32>`** and **`is_ascii: bool`** — per-column statistics computed in
  Rust **from the offsets already present** (§4.2) while building the chunk. Zero cost.
  They enable automatic column sizing without `measureText` (§9.8).

> **`source` unlocks in-grid editing.** It costs nothing to capture it now; without it,
> editing is not implementable at all. PG populates it only for columns that come
> directly from a table (on `SELECT a+b` it is zero).

> ⚠️ **A column's identity is its POSITION, never its name.**
> `SELECT a.id, b.id FROM …` produces two columns named `id`. Any
> `name → column` map in the UI or in the IPC is already a bug.

### 4.4 `ColumnData` variants

Keep them few: every variant is decoding, comparison, formatting and export code.

| Variant | Covers |
|---|---|
| `Bool` | bit-packed |
| `I16` / `I32` / `I64` | smallint, int, bigint, MySQL YEAR |
| `U64` | MySQL `BIGINT UNSIGNED` |
| `F32` / `F64` | real, double |
| `Utf8` | text, JSON/JSONB, UUID, enum/set, **and the whole fallback** |
| `Binary` | bytea, BLOB (same shape as Utf8) |
| `Date32` | days since the epoch |
| `Time64Micros` | time of day |
| `TimestampMicros { tz: bool }` | timestamp / timestamptz |

Everything else (interval, range, array, geometries, domains, bit/varbit, custom types)
→ `Utf8` with the text **exactly as the server sends it**.

Possible future upgrades: `IntervalMonthDayNano` (matches almost perfectly the
PG months/days/microseconds triple), `Decimal128` for MySQL `DECIMAL`.

### 4.5 Text rule: lossless round-trip

Not "always keep the text" (it would double memory for nothing), but:

- **Exact** typed variant and text reproducible by formatting
  (`i64`, `Date32`, `TimestampMicros`) → **the type only**.
- **Lossy** conversion → **the text only**.

Concrete cases:
- **PG `NUMERIC`** → stays `Utf8` permanently (precision/scale may be
  unspecified, values exceed any `Decimal128`). Flag in `ColumnMeta`:
  "right-align, sort numerically".
- **MySQL `DECIMAL`** → precision and scale known in the metadata, promotable to `Decimal128`
  in the future. One more reason to keep the native metadata even if unused.
- **`float8`** → no problem: since PG 12 the default `extra_float_digits` produces
  text with an exact round-trip.

### 4.6 Cases not representable in Arrow

**Strategy: at the first non-representable value the column degrades to `Utf8` and stays there.**
It costs one rewrite of the current chunk, and protects forever.

- **MySQL zero-date `'0000-00-00'`** — legal with certain `sql_mode`s, extremely common in
  old schemas. Does not fit in `Date32`.
- **MySQL `TIME`** — from `-838:59:59` to `838:59:59`: **it is a duration, not a time of day**.
  Does not fit in `Time64`.
- **PG `infinity` / `-infinity`** on dates and timestamps — no integer represents them.

Ignoring them produces a crash or, worse, a wrong value with the face of a right one.

### 4.7 Large values (LOB)

Inline cap per cell: **64 KB**. Beyond the threshold the value does not go into the column
buffer but into the chunk's `LobArena`; the column holds a truncated prefix + handle.

The arena lives **inside the chunk** because a stream does not rewind: "fetch the full
value" cannot mean re-running the query. It is however a separate unit, so it is the
first thing that can be spilled to disk.

IPC sends only the prefix; `get_cell_full` reads from the arena.

### 4.8 Addressing, sorting, accounting

- **Global row → (chunk, offset)**: binary search on `row_starts`. Once per
  window, not per cell.
- **Local sort and filter do not move data**: they produce a permutation (`Vec<u32>` of
  global indices) through which the window is read. Zero buffer copies, and the
  future move to DataFusion requires no redesign.
- **Rows inserted from the grid stay where they were added** (table tab, like
  DBeaver): unsaved, the grid shows each new row right below the row it was
  added under (`rowLayout.ts`); on save the backend re-reads it, appends it to
  the chunks like any page and moves it in the view to the row the grid showed
  it on (`AppendTo.at`). The unsorted view then becomes an explicit order
  (`Vec<u32>`, the same shape as the sort permutation), built only the first
  time a row is placed: without it the view stays "physical minus hidden",
  no copy. Rejected: re-querying after save (loses scroll and position, the
  server may sort the row anywhere) and appending at the bottom (the row
  vanishes from where the user was working, and a following page pushes it
  further down).
  The next page (`OFFSET`) can then bring a row already on screen: the inserted
  row where the server sorts it, or, when the insert sorts before the rows
  already read, the last of the previous page pushed down by one. Rows of the
  page whose key (PK) is already in the view are hidden but **counted** in the
  `OFFSET` (`dups`): the server sent them and still has them. Only after an
  insert from the grid; inserts by other sessions remain the general limit of
  `OFFSET` paging, as in DBeaver.
- **Memory**: every `Column` can report its own bytes (capacity of data + offsets + bitmap),
  the `Chunk` sums columns + arena, the `ResultSet` sums the chunks.
  **A property computed from the layout, never a hand-maintained counter** — hand-maintained
  counters always drift.

---

## 5. Memory policy

Premise that dissolves the problem: separate **interactive browsing** (the user will never
read 50M rows: they need the first rows immediately and smooth scrolling) from **operations
on the full set** (export, count: they are **streamed to file**, never materialized).

### Chunking

`Vec<Chunk>` with chunks of ~64k rows. Pays off on five fronts: no giant reallocations,
evict/spill per single chunk, mid-fetch cancellation that keeps the complete chunks,
`fetch_window` that becomes "find the chunk + offset", 1:1 mapping onto
`RecordBatch` for Arrow.

**Closing a chunk is triggered by three conditions:**
1. row count (64k)
2. byte threshold
3. **risk of `i32` offset overflow** — a chunk cannot exceed 2 GB of text,
   and with large TEXT columns you get there sooner than it seems

Chunking is therefore the single mechanism governing both memory and layout validity.

### Thresholds — the budget is in BYTES, never in rows

50 TEXT columns and 3 INT columns have the same number of rows and two orders of magnitude
of difference in RAM.

- **Soft budget per result set** (default 256–512 MB): when reached, the fetch is **paused**,
  not interrupted. Banner: *"1.2M rows loaded, 480 MB —
  Continue / Stop / Export to file"*. The user decides.
- **Global budget** across all open result sets: LRU eviction of the non-visible ones
  (reloadable if the stream is alive) or refusal of new executions.
- **Spill to disk**: not in the MVP, but almost free afterwards — a chunk is already a set of
  contiguous buffers: write it and `mmap` it without serialization.

### 5.1 Dialect traps

**MySQL** — streaming **occupies the connection** until it is fully consumed.
Stopping the fetch halfway leaves two options: drain the rest (waste) or
`COM_RESET_CONNECTION` (clean and cheap, but **loses the open transaction, temp tables
and session variables**).
→ **Decision: large fetches run on a dedicated connection**, not on the sticky one,
when there is no open transaction.
→ **Stopping a stream mid-way fires `KILL QUERY` first, then drains** (keeps
transaction and session state, unlike the reset). Without the KILL the drain
reads the whole rest of the result: minutes for a large table behind SSH,
with the connection — and the next Run — blocked meanwhile. The KILL is
awaited before the drain ends, so it never reaches the next statement.

**PostgreSQL** — `query_raw` uses a portal with batches, which keeps an implicit transaction
open for the whole duration: an active snapshot that **blocks vacuum**. A result set
forgotten in a tab for 20 minutes on a production DB is a behaviour a
DBA forgives only once.
→ **Inactivity timeout on paused fetches.**

### 5.2 Two prohibitions

- **No automatic `SELECT COUNT(*)`** (full scan). The grid must live with an unknown
  total: Glide is given the number of rows loaded and "1,000,000+" is shown
  while the stream is open. The exact count is an explicit action.
- **No rewriting of the user's SQL** to inject `LIMIT`. It breaks edge cases
  and — above all — **shifts error positions** (§6). Use protocol-level limits
  (the portal's `max_rows` in PG, stopping the stream in MySQL). If an
  auto-LIMIT is offered, it must be an explicit, visible option.

---

## 6. Error model

An error is not a string, it is a **structured record**, complete from the MVP: it costs
little now and a great deal later.

### Fields

- **severity** — errors are not the only thing. PG sends `NOTICE`/`WARNING` **asynchronously**
  during execution: `RAISE NOTICE` in PL/pgSQL is *the* way debugging
  is done. Without it, the tool is unusable for anyone writing stored procedures.
  MySQL instead accumulates warnings and requires an explicit `SHOW WARNINGS` when
  `warning_count > 0` → **run it automatically**, with an option to disable it.
- **code** — SQLSTATE for PG; for MySQL **both errno and SQLSTATE** (MySQL users
  think in numbers: 1062, 1452…).
- **message / detail / hint** — PG has three distinct fields and the hints are often decisive.
  Show them hierarchically, not concatenated on one line.
- **schema / table / column / constraint / datatype** — PG populates them on constraint
  violations. Keep them even if initially unused: they enable "click the constraint
  to open the table".
- **where / context** — PL/pgSQL stack trace.
- **position**, plus `internal_position` + `internal_query` for errors inside functions or
  prepared statements.

### 6.1 Position mapping — the critical detail

PG gives the position **in characters, 1-based, relative to the statement it received**.
The editor buffer is split into statements, so what is needed is:

1. statement start offset — **the splitter must return it both in bytes and in characters**
2. conversion from characters to a position in the full buffer
3. handing it to CodeMirror

With non-ASCII text (comments in Italian, strings with accents) confusing bytes and
characters makes the highlight slide: it is **the** classic bug of this feature.

**MySQL does not give the position.** The message sometimes contains "near '...' at line N":
best-effort parsing, otherwise the whole statement is highlighted.
→ The model has `position: Option<…>` and the UI degrades gracefully.

### 6.2 Taxonomy (the UI reaction differs)

| Category | Reaction |
|---|---|
| Transport / connection | offer reconnection |
| Authentication | reopen the credentials dialog |
| SQL error | highlight, do **not** reconnect |
| **Cancellation** | PG `57014`, MySQL errno `1317` → show "cancelled by the user", **never** as a red error |
| Timeout | — |
| Internal bug | — |

### 6.3 Dedicated case: PG `25P02`

*"current transaction is aborted, commands ignored until end of transaction block"*.
If an error happens inside a transaction, everything that follows fails and the user
gets stuck without understanding. **Banner with a "Rollback" button**: it immediately distinguishes a
serious tool from an amateur one.

### 6.4 Multi-statement

Default: **stop at the first error**, with a "continue on error" option.
Every statement has its own result or its own error — never a single aggregated outcome.
The export of a script follows the same rule: one section (single file) or
one file (new folder, never an existing one) per statement, introduced by
its SQL, never a merged table — the results have different columns.

### 6.5 Statement splitter

It must handle **Postgres dollar-quoting `$$`** and **MySQL `DELIMITER`**:
a classic breaking point.

Note: `sqlparser-rs` is a *strict* parser and fails on incomplete text → **it is not
suitable for autocomplete while typing**. Two different things are needed: a tolerant
tokenizer on the frontend side for completion, and the robust splitter on the Rust side.

---

## 7. Cancellation

### Three distinct levels

Confusing them is the cause of the classic *"I pressed Cancel but the server is at 100% CPU"*.

1. **Local abort** — stop reading / drop the future. The server carries on undisturbed.
   On its own it is almost useless and leaves the connection dirty.
2. **Server-side cancel** — the out-of-band request. **The only one that really matters.**
3. **Timeout** — client-side (= level 1) or server-side (`statement_timeout` in PG,
   `MAX_EXECUTION_TIME` hint in MySQL, SELECT only). Server-side ones are far more
   reliable but **change the semantics of the user's queries**: optional per connection
   profile, never a hidden default.

**1 and 2 must both fire, in this order: first the server-side cancel, then stop consuming.**
`tokio-util`'s `CancellationToken` propagated into the task covers level 1.

### 7.1 Mechanics per dialect

**PostgreSQL** — at connect time you receive backend PID + secret key. Cancellation is a
`CancelRequest` packet on **a new connection** (`tokio-postgres` exposes it as
`cancel_token`). It is best-effort and asynchronous: it may arrive after the query has finished.
⚠️ **Behind a pooler (PgBouncer) it may not reach the right backend and simply
not work.**

**MySQL / MariaDB** — no protocol-level cancellation. You need the connection id
(handshake or `CONNECTION_ID()`) and a **second connection** that runs `KILL QUERY <id>`
(kills the statement, not the session). Requires the `PROCESS` privilege or being the same
user. ⚠️ **On managed services (RDS, Aurora, PlanetScale) it may fail silently.**

→ **At connect time, capture and keep the cancellation handle, and verify the capability.**
If it cannot be obtained, a disabled Cancel button with an explanatory tooltip is better than
a button that pretends.

### 7.2 State machine (in the registry)

```
Pending → Running → (Completed | Failed | Cancelling → Cancelled)
```

- Cancel after completion is a **no-op**, not an error: the race is perfectly normal.
- Two clicks = a single action (idempotence).
- **Escalation**: first click → cancel/`KILL QUERY`; if after ~10s nothing happens,
  offer terminating the connection (`pg_terminate_backend`, `KILL CONNECTION`),
  which is destructive and must be chosen deliberately.

### 7.3 Partial data

**It is kept**, and the result set is **marked as incomplete** (behaviour
expected by anyone coming from DataGrip).
⚠️ The flag must propagate to **local sort, filter and export**: otherwise false
results are produced with the face of real ones.

### 7.4 Connection state after cancel

- PG: if in an explicit transaction → aborted state (§6.3).
- MySQL: the connection survives `KILL QUERY`, but if a stream was dropped a reset
  is needed.

---

## 8. IPC surface

**Principle: the full result set stays in Rust, indexed by an id. The frontend
asks only for the visible window.**

| Area | Commands |
|---|---|
| Connections | `test`, `connect`, `disconnect`, `list_sessions` |
| Introspection | `list_databases`, `list_objects` (lazy per level), `describe_object`, `refresh` |
| Execution | `execute` → immediately returns `exec_id`; streaming events on a channel (progress, first rows ready, server notices/warnings, done, error) |
| Results | `fetch_window(result_id, row_range, col_range)`, `get_cell_full` |
| Lifecycle | `release_result`, `cancel(exec_id)` |
| Export | executed **entirely in Rust to file**, without going through the UI |

### 8.1 Two details discovered late

1. **The window is columnar on the wire too** — one array per column, not an array of
   objects. Less JSON, and Glide works by coordinates anyway.
2. **Large values must be truncated in the window**, with on-demand fetch. Otherwise a
   1 MB TEXT column over 200 visible rows = 200 MB of JSON for a single scroll.

JSON is perfectly fine for ~200-row windows: **do not over-engineer**.

### 8.2 NULL and formatting

- NULLs must remain distinguishable from the string `"NULL"` and from the empty string
  (separate array or compact bitmap), otherwise the grid shows three different things
  the same way.
- **Formatting (locale, display timezone, decimal separator) is
  presentation**: it is done centrally in Rust, but the **canonical form always stays
  available** for clipboard copy and export. Whoever copies a cell expects
  the value, not its localized rendering.

---

## 9. Frontend

**Guiding principle: the frontend is never the source of truth about the data.**
Rust owns connections, sessions, result sets, schema cache. The frontend owns
the UI state (layout, tabs, selection, editor buffer, scroll) and a *mirror* synchronized
via events.

### 9.1 Entities

- **Connection profile** — persisted in SQLite: host, port, user, sslmode, tunnel
  config, reference to the credential in the keychain, **colour/label** (distinguishing
  production from development: a trivial and much-loved feature).
- **Session** — runtime: connected profile + introspection connection + N sticky.
- **Tab** — SQL editor (buffer, sticky connection, current database/schema, execution
  history) | data tab | object editor.
- **Execution** — exec_id, statement, state, timings, result_id or error.
- **Workspace** — set of tabs + layout, persisted.

### 9.2 Where NOT to put things (the decision that matters)

| What | Where | Why |
|---|---|---|
| Sessions, tabs, executions, settings | **Zustand** (separate slices, narrow selectors) | otherwise every backend event redraws the app |
| Schema introspection | **TanStack Query** | cache, invalidation, background refetch, lazy per node |
| **Row window cache** | **outside React**, mutable object + imperative redraw (`updateCells`) | in a store every scroll → cascade of re-renders → dropped frames |
| **CodeMirror document** | **outside React**, `EditorState` in a registry/ref, synced to the store on debounce or blur | in the store on every keystroke = typing lag on large SQL files |

> The last two rows are counterintuitive: they go against the React mental model.
> They are **the single mistake that ruins the performance of a canvas grid**.

### 9.3 Events from the backend

A single subscriber that dispatches into the store.
Non-obvious requirement: events are asynchronous and may arrive **after the tab has
been closed** → the reducer must ignore unknown ids without breaking.

### 9.4 Persistence

**Unsaved SQL in the tabs must survive a crash.** Debounced autosave to
SQLite (not localStorage). It is an implicit expectation for this category of tool.

### 9.5 Interaction model — tabs

**Decision: single bar**, DataGrip style. Heterogeneous tabs (SQL, table, object
editor) in the same strip, differentiated by icon.

**Table tab** — contains sub-tabs: **Data / Structure / DDL / Indexes / Constraints / Triggers**.

**Consequences of the single bar** (tabs pile up much faster):

- **Preview tab** mandatory: a single click from the tree opens a provisional tab,
  replaced by the next click; a double click makes it permanent. Without it, after ten
  minutes of exploration there are 40 open tabs.
- **Overflow dropdown**, pin, "close the others".
- Keyboard navigation: `Ctrl+Tab` in MRU order, `Ctrl+PgUp/PgDn` positional.
- **Every tab shows the connection colour** (border or dot). With heterogeneous tabs
  mixed together, without this the user loses track of which connection each one points to —
  and it is the point where the "prod" colour (§9.7) really starts to work.
- Disambiguated title: `table_name @ profile` when there are homonyms across schemas or
  connections.
- Tabs belong to a *tab group*: in v1 there is a single group (no split panes),
  but the model must not preclude it.
- **Grouping inside the single row**: tabs grouped by connection and, inside the group, in a
  block per database: its SQL tabs on the left, its table tabs on the right, with a visual
  separator. Gives the conceptual separation at zero cost in vertical space, and each database
  shows up once under its marker. A new tab joins its database's block; open tabs never move
  on their own (an SQL tab that switches database stays put, the user drags it). Earlier
  "all SQL left, all tables right" split a database in two as soon as it had both kinds.

##### Rejected alternative: two separate rows

The real problem it would solve: **an SQL tab with unsaved work can end up
in the overflow** while exploring the tree and opening tables.

Rejected for three reasons:

1. **The vertical cost is permanent, the benefit occasional.** ~32px always lost, even
   when one of the two rows is empty — which in practice is often.
2. **It doubles the interaction complexity.** Two overflows, two scopes for `Ctrl+Tab`, and
   "where does a new tab go" becomes ambiguous as soon as the third type (object editor) arrives.
   No tool known to users does this.
3. **The problem is better solved elsewhere**: the preview tab eliminates the churn at the
   source, pinning protects the important tabs, grouping gives the visual separation.

→ If the real need were *seeing query and table together*, the answer is **side-by-side
panels** (each with its own strip), not two fixed bars: two rows are a
worse, uncontrollable version of that. To be evaluated only if the problem persists
in real use.

**Advantage of the table tab:** the SQL rewriting prohibition (§5.2) applies to the
*user's* SQL. In the table tab the query is generated by the app, so the following are legitimate and
recommended: keyset pagination, server-side `ORDER BY` on header click, per-column filters
translated into `WHERE`.

**Hand-written WHERE bar** (as in DBeaver): the user's text is ANDed with the
chip filters, in parentheses and closed by a line break (a trailing `-- comment` does not eat
`LIMIT`). It is not rewritten, but it is user SQL inside an app SELECT that does not go
through `execute`: before running it the session verifies that it remains **a single statement**
(otherwise `1=1; DELETE ...` would run as a second statement) and, on read-only
profiles, applies `check_read_only` and on PG a READ ONLY transaction also for the following
pages (a function in the WHERE could write).

### 9.6 Colouring by data type

**Decision: text tint**, not background.

Rust exposes a **`semantic_class`** field in `ColumnMeta`; the frontend maps class →
colour. The colour is resolved **once per column**, never per cell (the type is already in
`ColumnData`: zero cost).

**Five classes**, deliberately few:
`Numeric` · `Temporal` · `Text` (default, no tint) · `Boolean` · `Structured` (JSON/array/binary)

Rules:

1. **The type colour must not collide with the state colour.**
   Separate channels: **type = text tint, state = background/border**.
   State (modified uncommitted cell, PK, error, selection) **always wins** over type.
2. **NULL is a state, not a type.** Identical rendering in every column — italic, grey,
   `NULL` text — regardless of the class. Otherwise you get NULLs in five
   different colours.
3. **Low saturation.** The strongest signal for numbers remains
   **right alignment + tabular digits** (`font-feature-settings: "tnum"` or monospace);
   colour is reinforcement, not the main vehicle.
4. **Type icon in the header**, so the information does not travel by colour alone (colour blindness).

**Glide implementation:** the tint is applied via per-cell `themeOverride`, but the
overrides must be **precomputed once** (five frozen objects, indexed by
column) and **never allocated inside `getCellContent`**, which runs for every cell on every frame.

**Single tokens:** Glide is canvas and lives outside CSS. Colours must be defined as shared
tokens and passed both to the design system and to the Glide theme, otherwise dark mode and
grid diverge. Every class must be contrast-checked on: normal background, alternate
row, selection, modified cell, error cell — in both themes.

#### 9.6.1 Colour tokens — ✅ `[APPROVED — iter. 4]`

Verified with `check_tokens_v4.py`. **Re-run it on every future change.**

##### Outcome

| Check | Outcome |
|---|---|
| sRGB gamut, both themes | ✅ **no token out of gamut** |
| Text × 7 backgrounds (AA 4.5:1) | ✅ PASS — worst **4.77** |
| UI components (3:1) | ✅ PASS — worst **3.04** |
| Double focus ring, `max(inner,outer)` ≥ 3:1 | ✅ PASS — worst **10.84** |
| Range selection (`selection vs alt`) | ✅ 1.193 light / 1.327 dark |

##### Setup

- **4 coloured classes at identical lightness** (L 44% light / 76% dark): only the hue varies,
  no false hierarchy between types. `text` is achromatic = `cell-fg`.
- **Separate channels**: type = text tint · state = background · state always wins.
- **Selection**: jade tint on neutral backgrounds (`normal`, `alt`), **border only** on
  chromatic backgrounds (`modified`, `error`). Mixing jade and red produced a mud that failed
  contrast and lost the error identity.
- **Focus ring = achromatic double ring**, identical in both themes. In light the dark
  outer ring does the work (13.9–19.7), in dark the light inner one (10.8–18.0):
  **it adapts by itself to any background, with no conditional logic**, and competes with
  no hue. Requirement: `max(inner, outer)` ≥ 3:1.
- **Primary key = icon** in `cell-muted-fg`, not a colour.
- `cell-border` decorative (1.02–1.35:1) and **not** subject to 1.4.11: the structure is carried
  by alignment, spacing and zebra striping. Raising it would give a lattice that dominates the data.
  → Plan a **"high-contrast grid"** option in the settings.
- `cell-divider` (≥3:1) is the token for separators **that carry meaning**: header
  underline, divider of pinned columns, panel borders.

##### Hue map

```
 25°  state-error              -> 35°
 60°  type-structured          -> 25°   (accepted: different channels)
 85°  state-modified           -> 70°
155°  cell-selection-border    -> 60°
215°  type-boolean             -> 35°
250°  type-numeric             -> 50°
300°  type-temporal            -> 40°
340°  cell-prod                -> 45°
```

> **Durable rule: the hue budget is allocated per layer** — types / states / chrome —
> not by assigning every new token to the free slot nearest to the conceptually
> related one. A previous round had produced 10 signals in two neighbourhoods with 155° unused.

##### Final values

```css
:root {
  --type-numeric: oklch(44.0% 0.120 250);
  --type-temporal: oklch(44.0% 0.120 300);
  --type-text: oklch(24.0% 0.014 95);
  --type-boolean: oklch(44.0% 0.078 215);
  --type-structured: oklch(44.0% 0.103 60);
  --state-normal: oklch(97.5% 0.005 95);
  --state-alt: oklch(93.0% 0.007 95);
  --state-selection: rgb(188 221 201);
  --state-modified: oklch(90.0% 0.075 85);
  --state-error: oklch(90.0% 0.052 25);
  --cell-fg: oklch(24.0% 0.014 95);
  --cell-muted-fg: oklch(46.0% 0.012 95);
  --cell-null-fg: oklch(46.0% 0.015 95);
  --cell-border: oklch(88.0% 0.006 95);
  --cell-divider: oklch(57.0% 0.010 95);
  --cell-selection-border: oklch(50.0% 0.124 155);
  --cell-panel-bg: oklch(99.0% 0.003 95);
  --cell-header-bg: oklch(95.5% 0.004 95);
  --cell-focus-ring-inner: oklch(97.0% 0.000 0);
  --cell-focus-ring-outer: oklch(12.0% 0.000 0);
  --cell-prod: oklch(50.0% 0.180 340);
}

[data-theme="dark"] {
  --type-numeric: oklch(76.0% 0.100 250);
  --type-temporal: oklch(76.0% 0.130 300);
  --type-text: oklch(92.0% 0.008 250);
  --type-boolean: oklch(76.0% 0.130 215);
  --type-structured: oklch(76.0% 0.130 60);
  --state-normal: oklch(20.0% 0.010 250);
  --state-alt: oklch(25.0% 0.011 250);
  --state-selection: rgb(13 62 44);
  --state-modified: oklch(34.0% 0.070 85);
  --state-error: oklch(32.0% 0.100 25);
  --cell-fg: oklch(92.0% 0.008 250);
  --cell-muted-fg: oklch(72.0% 0.012 250);
  --cell-null-fg: oklch(72.0% 0.020 250);
  --cell-border: oklch(28.0% 0.012 250);
  --cell-divider: oklch(60.5% 0.012 250);
  --cell-selection-border: oklch(62.0% 0.150 155);
  --cell-panel-bg: oklch(17.0% 0.009 250);
  --cell-header-bg: oklch(15.0% 0.009 250);
  --cell-focus-ring-inner: oklch(97.0% 0.000 0);
  --cell-focus-ring-outer: oklch(12.0% 0.000 0);
  --cell-prod: oklch(72.0% 0.160 340);
}
```

##### sRGB hex for the Glide theme

⚠️ Glide is canvas: `fillStyle` is not reliable with `oklch()` on all webviews.
`oklch` for the CSS, hexadecimal for Glide, **conversion as a build step** (not runtime).

| Token | Light | Dark |
|---|---|---|
| `type-numeric` | `#085491` | `#7FB6EE` |
| `type-temporal` | `#5D408A` | `#BE9DF7` |
| `type-text` | `#211F18` | `#E1E5EA` |
| `type-boolean` | `#005D6D` | `#1EC5E4` |
| `type-structured` | `#7B4200` | `#ED9C55` |
| `state-normal` | `#F8F7F3` | `#13161A` |
| `state-alt` | `#E9E8E3` | `#1E2227` |
| `state-selection` | `#BCDDC9` | `#0D3E2C` |
| `state-modified` | `#F5DBA5` | `#483400` |
| `state-error` | `#FFD1CD` | `#5C1717` |
| `cell-fg` | `#211F18` | `#E1E5EA` |
| `cell-muted-fg` | `#5A5851` | `#9FA5AC` |
| `cell-null-fg` | `#5A584F` | `#9BA6B1` |
| `cell-border` | `#D8D7D3` | `#25292F` |
| `cell-divider` | `#797771` | `#7C8289` |
| `cell-selection-border` | `#007742` | `#0FA05C` |
| `cell-panel-bg` | `#FCFCF9` | `#0C1013` |
| `cell-header-bg` | `#F1F0ED` | `#080B0F` |
| `cell-focus-ring-inner` | `#F5F5F5` | `#F5F5F5` |
| `cell-focus-ring-outer` | `#060606` | `#060606` |
| `cell-prod` | `#A02A83` | `#E579C4` |

##### To validate by eye, not with numbers

WCAG ratios measure text against background. They do **not** measure how well two classes
are distinguished from each other, nor the fatigue in aggregate. These remain to be judged in the
**real-density mock** (30 rows × 8 columns, mixed types, 28–30px):

1. **`numeric` / `boolean` at 35°** — the tightest pair among the types, and the two that
   appear side by side most often (ids, counters, flags). In light they also differ by
   0.042 in chroma: risk that `boolean` reads as *"faded numeric"*, i.e. as a
   state instead of a type. Asymmetric between themes (in dark the chroma is equal).
2. **`structured` (60°) on `state-modified` (85°)** — amber on amber. It passes contrast
   (5.94 / 5.34), but it is a matter of legibility.
3. **Saturation in aggregate** — isolated swatches deceive; 240 coloured cells do not.
4. **`cell-prod` magenta on the tab bar** — does not read as "danger" the way red does.
   Choice deemed correct (it is a marker of **persistent identity**, not an alarm: danger
   is communicated by the confirmations of §9.7), but to be confirmed visually.

#### 9.6.2 Typography — ✅ `[APPROVED]`

##### Decisions

| | Choice |
|---|---|
| Grid and SQL editor | **Monospace on all cells** |
| Chrome (tabs, tree, dialogs, menus) | **Bundled Inter** — identical rendering on Windows and macOS |
| Default density | **Compact 26px** (presets: 26 / 30 / 36) |
| Cell text size | **13px, independent of density and adjustable** |
| SQL editor text size | 14px |

**Why the text size is separate from density:** density serves those who want more rows on
screen, text size serves those who do not see well. Two different needs, two different controls.

##### Why monospace everywhere: automatic sizing for free

In §4.2 varlen columns are `offsets: Vec<i32>` + contiguous buffer → the length of
element *i* is `offsets[i+1] - offsets[i]`. **Rust already knows the length of every
cell at zero cost.**

With a monospace font: `column width = len_p95 × advance + padding`. One
multiplication, **no `measureText` on any cell**. With a proportional font you would have to
sample and measure, with an approximate outcome anyway.

→ Statistics in `ColumnMeta` (§4.3): **`len_p95`** (percentile, not maximum: a single
5000-character cell would produce an absurd column) and **`is_ascii`**.

⚠️ **Two limits to handle:**
- length in **bytes ≠ length in characters** with non-ASCII UTF-8;
- **CJK and emoji occupy two cells** even in monospace.

→ When `is_ascii == false`, fall back to sampling with `measureText`.

Database content is in any case mostly identifiers, codes,
timestamps, UUIDs and enums: distinguishing `0` from `O` and `1` from `l` matters more than prose
reading speed. Long descriptive columns are read in the inspection panel
(`get_cell_full`), not in the grid.

**Collateral simplification:** §9.6 asked for "right alignment + tabular digits".
With monospace, tabular digits are automatic → only the alignment remains.

##### ⚠️ Canvas constraint: no OpenType features

`ctx.font` accepts the CSS shorthand but **not** `font-feature-settings`. In the canvas you
cannot enable `zero`, `ss01`, `tnum` nor disable ligatures.

→ **The font must have the right characteristics by default**, not behind a flag:

1. **Dotted or slashed zero as standard** — if it is an optional stylistic set, for us it does not exist.
2. **No ligatures.** A coding font would render `!=` as `≠` and `->` as `→`:
   in code it is pleasant, **in data it is falsification**. A font without ligatures or an
   "NL" variant is needed.
3. **True italic** — rule 2 of §9.6 renders NULLs in italic; many monospace fonts have only
   a synthetic oblique.
4. **OFL licence** for bundling (§14.19).

##### Choice of monospace — `[TO VALIDATE IN THE MOCK]`

Having chosen the compact density, horizontal space is the scarce resource: **the advance
width determines how many characters fit in a column.** Between a 0.6em font and a 0.5em one
about 20% of content swings at equal width.

| Candidate | Notes |
|---|---|
| **JetBrains Mono NL** | Safe choice: high x-height (excellent at 13px), true italic, official no-ligature variant, OFL. **But it is wide.** |
| **Iosevka** | Noticeably narrower advance, OFL, builds without ligatures. More pronounced condensed aesthetic, a build variant must be chosen. |
| **IBM Plex Mono** | No ligatures by default, true italic, OFL. Middle ground. |

→ Generate the mock with **two candidates on the same table** and compare them. A choice
reversible at almost zero cost now, expensive later.
→ **Verify the zero by rendering it**, not by trusting the spec sheets: on this
point they are often imprecise.

##### Implementation traps

1. **Wait for `document.fonts.ready` before the first canvas paint.** Otherwise the
   first frame uses the fallback font, the metrics are wrong and the columns "jump".
2. **Do not use `textBaseline = 'middle'`** to centre vertically: use the alphabetic
   baseline plus the measured cap height. The canvas `middle` uses the centre of the em box,
   optically unbalanced towards the bottom in almost all fonts.
3. **Font licences go in the attribution**: `cargo-about` covers Rust crates, not
   assets.

##### ⚠️ Intersection with §9.6.1: 26px density and weak zebra striping

Zebra striping at **1.143:1** and `cell-border` at **1.03:1** are both almost imperceptible
(consciously accepted). At 30px it held; **at 26px with 8–12 columns the risk of the eye
skipping a row increases**, because the height shrinks without any horizontal
anchor.

→ The real-density mock must be done **at 26px, not 30**, with a specific evaluation
criterion: *can I follow a row from left to right across 10 columns without losing it?*
→ If not, the lever to raise is **the zebra striping, not the border** (raising the border would give the
spreadsheet lattice that §9.6.1 rules out).

---

### 9.9 Component library — ✅ `[APPROVED]`

**Decision: Radix UI + Tailwind, with shadcn/ui components adopted selectively.**

##### What it really covers

The grid is Glide, i.e. canvas: **outside the library by definition**. What is left to the library
is the chrome, and not even all of it.

| Needed | Covered? |
|---|---|
| Dialogs, context menus, dropdowns, selects, tooltips, toasts, forms, sub-tabs, accordions | ✅ |
| **Connection tree** (thousands of nodes, lazy, virtualized) | ❌ → `react-arborist` or TanStack Virtual |
| **Tab bar** with overflow, pin, preview, drag-reorder (§9.5) | ❌ → to be built |
| **Split pane / resizable layout** | ❌ → `react-resizable-panels`; **Dockview** if IDE-style docking is ever needed, but it brings its own tab rendering and would conflict with §9.5 |
| **Command palette** | ❌ → `cmdk` |
| Status bar, toolbar | partial |

##### Why this combination

1. The tokens are already CSS custom properties (§9.6.1) → the Tailwind config maps onto them directly.
2. Radix provides **focus management, keyboard navigation and ARIA**: for a
   keyboard-first tool it is the tedious and treacherous part to write by hand.
3. shadcn **is not a dependency**: it copies the source into the repo. Restyling towards our own
   tokens is a direct edit, not a fight with a theming API. No version lock: if tomorrow
   Radix slows down, the primitive is replaced under a component we already own.
4. It is the format the ecosystem — Claude Design and Claude Code included — knows best.

##### Honesty

shadcn components come with a recognizable aesthetic. Having our own design system
they will need substantial restyling: **the speed advantage is smaller than
is claimed**. The architectural one stands.

##### Rejected

- **Mantine** — more complete out of the box (it even has the tree), excellent TS, but brings its own
  styling system that would conflict with the tokens in CSS custom properties. Rejected for
  this, not for quality.
- **React Aria (Adobe)** — the most rigorous accessibility around, but hook-based:
  more work for the same result. Oversized here.
- **Base UI** (MUI primitives) — ecosystem still small.

---

### 9.10 Visual validation protocol — ✅ `[EXECUTED — 2026-07-30]`

> **Outcome** (mock `mock-visivi.html`, kept outside the repository, iter. 4 tokens, both themes, macOS):
> **A** ✅ numeric/boolean distinguishable when adjacent, structured legible on modified ·
> **B** ✅ row tracking held across 12 full columns at 26px with 1.143:1 zebra striping ·
> **D** ✅ prod magenta recognizable without reading · **C** no longer blocking (font already
> chosen, §9.6.2). Confirmation of canvas text rendering: deferred to the Glide prototype (M0, existing).

The six open checks **do not fit in one screen**. A single mock always produces the
same outcome: it looks nice, it gets approved, the defects surface at M2.
→ **Different constructions for different questions, each adversarial** (worst case, not
representative case).

| Mock | Construction | Question |
|---|---|---|
| **A — types** | `numeric` and `boolean` columns **alternating and adjacent** (`id`, `is_active`, `count`, `is_deleted`, `parent_id`, `is_locked`); a `structured` column next to cells in `modified` state | Are `numeric`/`boolean` at 35° distinguishable? Is amber on amber legible? |
| **B — row tracking** | 10–12 full-width columns, 40 rows, **26px** | *I start from the first column, I reach the last one: am I still on the same row?* The only test of the 1.143:1 zebra striping |
| **C — font** | Exactly the same data twice, one below the other | Monospace comparison. Any other variable invalidates the test |
| **D — chrome** | Tab bar with mixed profiles | `cell-prod` magenta as an identity marker |

##### The data must be realistic

Full UUIDs · timestamps with microseconds · negative and many-digit numbers · NULLs scattered
across columns of **different types** · long truncated text · nested JSON · booleans.

⚠️ **Saturation in aggregate is judged only on ~300 realistic cells**, never on isolated
samples. `123` and *lorem ipsum* test nothing.

##### Order

**A and B first** — they are the only ones that can invalidate the tokens of §9.6.1. Choosing a font
on a palette that then changes is wasted work.

##### ⚠️ Limit of the mock

The mock is HTML, the grid is canvas: **text rendering is not identical** (antialiasing and
hinting differ, on Windows in particular). For colour and layout the mock is reliable;
**for the legibility of a 13px font it is less so**.

→ **The mock selects the candidates, the M0 Tauri + Glide prototype confirms** — it draws
real canvas text at real density, and gets built anyway.

##### Choice of monospace — ✅ CLOSED

**JetBrains Mono NL** (details in §9.6.2). Mock C remains useful only as an after-the-fact
confirmation, it is no longer blocking.

---

##### Chosen font — ✅ **JetBrains Mono NL**

NL variant (no ligatures), OFL. **Three files**: Regular 400 (cells), Medium 500
(headers), **Italic** (NULL, rule 2 of §9.6).

**Advance 0.6em → 7.8px at 13px.** Sizing formula:

```
column width = len_p95 × 7.8 + padding
```

(timestamp with microseconds = 26 characters → 203px + padding)

⚠️ **Do not subset the grid font.** For Inter the Latin subset makes sense: the
UI strings are known. For the grid **no**: the content is arbitrary user data, and
if a cell contains Cyrillic it falls back **with a different advance** → the monospace
alignment breaks exactly where it is needed.

The fallback will exist anyway for CJK, Arabic and emoji (not covered). This is why the
`is_ascii` flag of §4.3 distinguishes *"width computable exactly"* from *"sampling needed"*.

⚠️ **Silent canvas fallback.** If the family in `ctx.font` does not match a loaded font,
the canvas **reports nothing**: it draws with the default, with different metrics → all
the computed widths are wrong with no apparent cause.

→ **Assertion at startup**: measure a known string of N characters and verify that
`measureText().width` is within tolerance of `N × 7.8`. Better an explicit error than
crooked columns.

→ **To verify by rendering**: that the zero is dotted by default (in the canvas it cannot be
enabled as an OpenType feature).

##### Alternative evaluated

**Iosevka** — advance ~0.5em, hence denser. Rejected because it requires choosing or
compiling a build (extra work and maintenance) against a real gain smaller than the
per-character calculation: the ~20% **materializes only on the columns whose width is
dictated by long content** (timestamps, UUIDs, hashes); a column of small integers or
sized by its header is identical with both fonts.

It remains a token: replaceable if mock C were to overturn the judgement.

---

#### 9.6.1-bis History of the iterations

Superseded values, kept only as a trace of the reasoning. **Do not use.**

| Iter. | Problems found | Correction |
|---|---|---|
| 1 | 5 tokens out of sRGB gamut; `null-fg` at 4.23 on dark normal background; combined `*-selected` states below 4:1 | — |
| 2 | Selection rendered by mixing jade into the chromatic backgrounds: red + jade = mud that failed contrast **and lost the error identity** | Removed the 3 `*-selected` tokens; selection via border |
| 3 | `selection vs alt` at 1.015:1 → the interior of a multiple selection was invisible on alternate rows, the range read **as stripes** | Jade tint on neutrals, border on chromatics |
| 3 | `cell-border` below 3:1 | Accepted (decorative) + introduced `cell-divider` ≥3:1 for separators that carry meaning |
| 4 | 10 chromatic signals in 2 neighbourhoods with 155° unused: `focus-ring` **5°** from `boolean`, `prod` **15°** from `error`, `pk-marker` 25° from `selection-border` | Focus ring → achromatic double ring; `prod` → 340°; PK → icon |

**Transferable lesson:** WCAG ratios check one pair at a time and see neither
the composition of states nor the crowding of hues. Both structural defects
emerged only by looking at the palette **as a whole**.

---

### 9.7 Production profiles and operational safety

**`is_production`** flag on the connection profile, with an associated colour that pervades the tab
strip, the editor border and the grid header.

**On a prod profile:**

1. **Auto-commit OFF.** Commit and rollback are explicit buttons.
2. **Confirmation at the first write of every transaction** `[to confirm: transaction
   scope, not session]`. Per session it would be a single confirmation and then hours of
   unsupervised writes; per transaction it coincides with the logical unit of change and
   pairs with the manual commit — two gates per transaction, unobtrusive.
3. **`[RECOMMENDED]` Irreversible operations always stay confirmed**, even if they
   are not the first write: DDL on MySQL, `TRUNCATE`, `DROP`, `UPDATE`/`DELETE`
   without `WHERE`. Reason in §9.7.1: if they slip through an already open gate there is no
   safety net.
4. **Read-only mode** available as a separate, complementary flag: the manual
   commit protects from distraction, read-only protects from the case where nothing
   should have been written at all.

**Manual commit outside prod** (2026-10-28): a toolbar switch per connection,
as in DBeaver, turns on the same regime (implicit BEGIN, grid edits in the
transaction, Commit/Rollback, log, idle rollback) without the gates. It
cannot change with a transaction open — switching would leave it neither
committed nor rolled back — nor on prod (always manual) or read-only.
Isolation level and DBeaver's "smart commit" are left out until asked for.

**On every other profile** (2026-10-28): only `UPDATE`/`DELETE` without `WHERE`
are confirmed, as DataGrip and DBeaver do — DDL stays free on development
servers, where tables are dropped and recreated all day.

**In the confirmation dialog**: profile name and colour, statement, and — if available —
**the estimate of affected rows derived from `EXPLAIN`**, which is free and does not require the
`COUNT(*)` forbidden in §5.2.

#### 9.7.1 Trap 1 — in MySQL the manual commit is partly a fiction

MySQL **has no transactional DDL**. `CREATE TABLE`, `ALTER TABLE`, `DROP TABLE`,
`TRUNCATE` cause an **implicit commit**: they also commit everything that was
pending, and cannot be undone.

⚠️ On MySQL prod the Rollback button **does not save** you from a `DROP TABLE`. If the user believes
otherwise, the tool has given them a false sense of security — worse than no security.
→ DDL must be intercepted with a confirmation that explicitly says *"operation cannot be undone"*.

In PostgreSQL DDL is transactional and rollback really works, with the exceptions that
cannot run inside a transaction (`CREATE INDEX CONCURRENTLY`, `VACUUM`,
`CREATE/DROP DATABASE`, `REINDEX CONCURRENTLY`, `ALTER SYSTEM`) which must be handled separately.

→ New capability: **`transactional_ddl`**.

#### 9.7.2 Trap 2 — auto-commit off is itself a risk on production

With auto-commit disabled **even a simple `SELECT` opens a transaction**. Query
launched, result read, user off to lunch → *idle in transaction* connection on a
production database: in PG it holds a snapshot and **blocks vacuum**, in MySQL it grows
the history list and holds locks. The safety measure becomes the problem.

Mitigations, all necessary:

- **Distinguish a read-only transaction from a transaction with writes.** The Commit
  button "lights up" only after an actual write; otherwise the user sees a
  pending commit after every SELECT and stops giving it weight.
- **Visible timer** on the open transaction, with progressive warning
  ("open for 4 minutes").
- **Automatic rollback when the tab is closed**, prompt when the app is closed.
- **Transaction log** (2026-10-28, like DBeaver's): the tracker keeps the
  successful writes of the open transaction (editor and grid Save, rows
  affected), at most 500 of them and each cut at 2,000 characters — the count
  stays exact. Commit shows the count; on prod it opens the list first, since
  Commit makes permanent statements the user may no longer have on screen.
  Elsewhere Commit is immediate and the badge opens the list.
- **If the connection drops with an open transaction the work is lost**: it must be said
  explicitly, not discovered.
- **Automatic rollback of the idle transaction** (2026-10-28): after 15 minutes
  without statements on the sticky the client sends `ROLLBACK`, on every
  connection with an open transaction (prod or a hand-written `BEGIN`). The
  badge counts down the last 2 minutes; the UI says whether writes were lost.
  Client-side because MySQL has no server variable for it (and RDS MySQL is the
  team's real server), as DBeaver does; idleness counts from the last
  statement, so a running or paused query never triggers it. Rollback, never
  commit: what nobody confirmed is not applied.
- Server-side `idle_in_transaction_session_timeout` (PG) / `idle_transaction_timeout`
  (MariaDB) as an additional net, at 20 minutes: after the client's rollback,
  for when the app cannot do it (frozen, network gone).

#### 9.7.3 Read/write classification — do not look at the first word

⚠️ In PostgreSQL `WITH x AS (DELETE FROM … RETURNING *) SELECT * FROM x` **starts with
`WITH` and writes**. `CALL` and functions are opaque. `SELECT … FOR UPDATE` takes locks.

**Conservative rule: if the statement cannot be classified with certainty, on a prod profile
it counts as a write.**

---

## 10. Introspection

- **PostgreSQL**: use `pg_catalog`, **not** `information_schema` — richer and much faster.
- **MySQL**: `information_schema` becomes extremely slow on instances with thousands of tables →
  consider the `SHOW` commands as a fallback.
- **Lazy** loading of the tree + cache invalidation strategy.

---

## 11. Dialect divergences — summary

| Aspect | PostgreSQL | MySQL / MariaDB |
|---|---|---|
| Cancellation | `CancelRequest` on a new connection; ⚠️ pooler | `KILL QUERY <id>` from a 2nd connection; ⚠️ privileges on managed |
| Error position | yes (characters, 1-based) | no (best-effort from the message) |
| Warnings | asynchronous `NOTICE`/`WARNING` | explicit `SHOW WARNINGS` |
| Error code | SQLSTATE | errno **+** SQLSTATE |
| Half-consumed stream | portal, blocks vacuum | **occupies the connection** |
| Database change within session | **impossible** (needs a new connection) | `USE db` |
| Binary format | per single column (Bind) | only with prepared statements |
| Column source metadata | `table_oid` + `attnum` | `db` + `org_table` + `org_name` |
| Aborted transaction | `25P02`, blocks everything | no equivalent |
| **Transactional DDL** | **yes** (except `CREATE INDEX CONCURRENTLY`, `VACUUM`, …) | **no** — implicit commit, cannot be undone |

⚠️ **Impact on the data model**: "change database in this tab" in PG means opening a
new connection and **losing the session state**. It must be represented in `Capabilities`
and the UI must warn, not silently make an open transaction disappear.

---

## 12. Security

- TLS via `rustls`, with explicit handling of the `sslmode` modes
  (`alamo_core::tls`); what the user sees of it is §12.2.
- SSH tunnel via `russh`.
- **SSH profiles are a shared entity** (`ssh_profiles`, referenced by
  `profiles.ssh_profile_id`): a bastion is saved once and used by several
  connections, and so is its credential in the keychain. They are created,
  edited and deleted in their own manager (`SshProfilesDialog`), which shows
  who goes through each bastion, its confirmed host key and whether a
  credential is saved; the connection's SSH tab only picks one (DataGrip
  model). Rationale: a change to a bastion applies to every connection that
  uses it, so it is made where that is visible, and deletion asks for
  confirmation listing the connections that fall back to a direct
  connection. "Test tunnel" (`alamo_tunnel::probe`) saves the profile first
  and logs into the bastion without forwarding anything; the typed credential
  reaches the keychain only once the bastion has accepted it, and only if
  asked.
- **Credentials in the system keychain** (`keyring` crate) — never in plain text in the
  configuration file.

### 12.1 Linux caveat `[DEFERRED — outside v1 target]`

To be picked up only if Linux comes back into scope:

⚠️ On Linux `keyring` uses the **Secret Service via D-Bus** (gnome-keyring, KWallet).
On minimal desktops, lightweight window managers, headless sessions or some distros
**it is not available** → a fallback with an encrypted file and master password would be needed.

⚠️ **Flatpak is sandboxed**: access to `~/.ssh` and to the Secret Service must be declared
explicitly in the manifest. It would affect SSH tunnels and credentials.

On Windows (Credential Manager) and macOS (Keychain) the problem does not arise: they are always
available.


### 12.2 What the user sees about TLS (decided 2026-11-04)

**The facts are measured on the open session, never inferred from the
profile.** `Connection::tls_info` returns `None` for a plaintext channel
(`prefer` that fell back) or protocol, cipher, the server certificate and
whether the chain was verified; the app serialises it as `Security` for
"Test connection", `connect_profile` and `open_database`, and the UI
shows it after the test and on the connection page. `verified` means the
handshake succeeded under a chain-verifying mode: a `prefer`/`require`
channel is encrypted and *not* verified, and the page says so in words.

Sources, per engine, because neither driver exposes the rustls session:

- **PG**: every mode goes through a `ServerCertVerifier` of ours, so the
  connector wraps it in one that keeps the end-entity certificate it saw
  (`alamo_driver_postgres::tls::Capture`); protocol and cipher come from
  the `pg_stat_ssl` row of the backend, the query that already told
  whether the channel was encrypted. The cancel/terminate connections reuse
  the connector against the same server: their overwrite of the slot is
  harmless.
- **MySQL/MariaDB**: `mysql_async` hides stream and certificate.
  `SHOW SESSION STATUS` gives `Ssl_version`, `Ssl_cipher` and the
  certificate's validity (`Ssl_server_not_before/after`, OpenSSL date
  format); subject and issuer are not available and are reported as such.

**A datum an engine cannot give is said, not hidden**: `Option` fields in
`TlsInfo`/`CertSummary`, "not available" in the UI. Hiding the row would
make MySQL look as inspected as PG.

**The root CA is inspected before connecting** (`alamo_session::x509`,
`inspect_root_cert`): subject, issuer and validity under the field as soon
as a path is there, or why the file is wrong (missing, unreadable, no PEM
certificate). `x509-cert` and `rustls-pki-types` were already in the tree
through tokio-postgres-rustls: no new dependency for parsing. The modes
themselves are explained in the dialog (`app/src/tlsModes.ts`): label, one
line on what each protects, padlock by level, technical id next to it; the
list still comes from `EngineInfo::tls_modes` (§2.1). A new production
profile starts from `verify-full` when the engine has it.

**Automatic coverage**: the only servers with a CA the profile can verify
are the `+tls` targets of `cargo xtask test-matrix` (CA and server
certificate generated by `xtask/src/tls.rs` at every run), also kept alive
by `cargo xtask tls-lab` for the e2e app; the plain matrix cannot test a
verifying mode succeeding.

---

## 13. Licence and legal aspects

### Dependencies — no constraint

The stack is entirely permissive: Tauri, tokio-postgres, mysql_async, rusqlite, rustls,
arrow-rs, russh, Glide Data Grid, CodeMirror 6 (MIT and/or Apache-2.0).
*To be re-verified precisely before release.*

> **Non-obvious note:** the official MySQL C client (`libmysqlclient`) is **GPL**,
> MariaDB Connector/C is **LGPL**. Whoever uses C bindings carries the constraint, often
> without noticing. `mysql_async`, a pure Rust reimplementation of the protocol,
> avoids it completely. A concrete advantage of the stack choice already made.

### Project licence

**Decision: Apache-2.0, open core model.**

**The model:**
- **Community** — this repository, Apache-2.0: a complete tool for the
  individual developer. Everything there is up to M5 (editing, export, transactions,
  TLS, SSH, keychain, multi-connection, prod/read-only protections) stays
  here: paid safety features cost trust.
- **Pro** — separate code in a private repo, hooking into the Community
  through extension points (driver registry, IPC commands, UI panels).
- The public repo **never contains** references to Pro code
  (`#[cfg(feature = "pro")]` towards private crates): it must compile on its own.

**How an extension build hooks in** (decided 2026-10-30):
- *Backend:* `alamo_app_lib::run(context, extensions)` (`app/src-tauri/src/lib.rs`);
  the Community binary passes none, the Pro binary (private repo) passes
  its Tauri plugin (own commands, own namespace) and its drivers (§2.2). A
  plugin reaches the open connections through `AppState::session`, and
  reads what the catalog methods do not cover with `Session::catalog_rows`
  (statistics lane, so the tree does not wait; per-engine queries stay
  in the extension). A
  single generic command or a `pro` feature were discarded: the first
  reinvents Tauri's plugins without types, the second is forbidden above.
  `context` comes from the calling binary because `generate_context!`
  embeds that crate's configuration and assets. The extension's binary
  therefore has its own `tauri.conf.json` (with a `plugins.updater`
  section: `run` initialises the updater, and without it the app does not
  start), `serde_json` as a dependency (the macro's output needs it),
  and declares its plugin's commands in `build.rs` and in a capability:
  plugin commands, unlike the app's, go through Tauri's ACL.
- *Frontend:* registries of extension points composed at build time. The
  Pro repo imports the Community sources from a sibling checkout, registers
  its parts before `main.tsx` and builds one bundle (one React, typed).
  Loading a separate bundle at
  runtime was discarded: shared React versions, CSP, errors only at
  runtime — and the official build comes from the Pro repo anyway.
- First registry: sidebar panels (`app/src/panels.ts`). The panels that
  need nothing from the Studio go through it, the Community History
  included, so the mechanism is used and tested in this repository. A
  panel gets `done()` to close the floating sidebar after a pick.

**Why Apache-2.0 and not GPL-3.0.** Apache is consistent with the stack (all
permissive) and allows using contributions in the Pro as well without a CLA (the
inbound licence is the outbound one, section 5). GPL-3.0 would prevent closed
forks, but would require a CLA to bring contributions into the Pro and discourages
some corporate contributors. The protection against forks lies in the trademark.

**DCO from the first external contribution, no CLA.** ⚠️ The DCO certifies the *origin*
of the code, it does **not** grant rights to relicense. With Apache they are not needed: Apache
code can already end up in non-open-source products. A CLA would be needed
only to relicense the existing code in a way Apache does not allow, and
it would have to be introduced **before** the first external contribution.

**Trademark** (`TRADEMARK.md`): Apache-2.0 does not grant the name (section 6). Whoever
publishes a modified version must change name and icon.

### Operational obligations

- Distribute licence texts and copyright notices of the dependencies →
  **`cargo-about`** (Rust) + npm equivalent, automatic generation.
- **`cargo-deny` in CI from day one**, with a **deny-list on GPL/AGPL**: it serves to block
  a transitive dependency that comes in six months later without anyone noticing.

### Trademarks

"MySQL" belongs to Oracle; "PostgreSQL" and "MariaDB" have their own policies.
- Nominative use ("supports MySQL") normally allowed.
- Do **not** call the product "MySQL Studio" or similar, do **not** use their logos in a way
  that suggests endorsement — beware of the icons in the connection tree.

### 13.1 Application identifier

The product is **Alamo DB**; it contains neither "SQL", "MySQL", "Postgres" nor "Maria".

#### Identifiers to fix NOW

| What | Value | Why now |
|---|---|---|
| **Bundle identifier** | **`io.github.axel769.alamo`** (final) | See below |
| **Data directory** | `%APPDATA%\Alamo` · `~/Library/Application Support/Alamo` | Hosts the SQLite with profiles, history and autosave (§9.4) |
| **Crate names** | `alamo-core`, `alamo-driver-postgres`, `alamo-driver-mysql`, `alamo-session`, `alamo-app` | §2 |

#### Bundle identifier — placeholder and deadline

⚠️ **The real deadline is the first signed and distributed build (M5)**, not the first line of
code: before that moment no user exists with saved credentials to
orphan, so changing it is free.

**Resolved (2026-10-01): `io.github.axel769.alamo`** — GitHub namespace of the repository
(`github.com/Axel769/alamo-db`), demonstrably controlled and compatible with
Flatpak.

**The change must be a single edit:**
- define it **only** in `tauri.conf.json`;
- **derive** the keychain service name and the data path from there, never rewrite them elsewhere;
- a single constant in the core, not scattered through the commands.

Consequence worth knowing: **Tauri derives the data directory from the identifier**, so
changing it moves the folder and the development data seem to have vanished. Irrelevant before
release, but knowing why saves half an hour of confusion. *(Verify the exact behaviour
against the current Tauri 2 documentation.)*

Form constraints: lowercase only, alphanumerics and dots, no underscores.

⚠️ **Keychain**: the service name must be derived from the identifier as a stable constant. On
macOS the mechanism goes through the ACL tied to the signature more than through the literal bundle id, but the
practical consequence is the same: **if the app's identity changes, the saved credentials
become unreachable**.

---

## 14. Open decisions

| # | Decision | Impact | Priority |
|---|---|---|---|
| 1 | **Target operating systems** — Linux first-class or "nice to have"? | Determines how much weight to give the WebKitGTK result in M0 | **Before M0** |
| ~~1~~ | ~~Target operating systems~~ | **RESOLVED**: v1 = Windows + macOS. Linux out of scope, can be reopened (§1.2) | ✅ |
| ~~2~~ | ~~DB version matrix~~ | **RESOLVED**: §15.1 — CI on 6 containers, best effort further back. **Does not block M0**, which requires a single PG instance | ✅ |
| 3 | **PG transport format** — hybrid binary/text for whitelisted OIDs? How much control does `tokio-postgres` expose, is it necessary to go down to `postgres-protocol`? | Decoding performance | `[VERIFY M0]` |
| 4 | Workspace **global or per profile** | Persistence model | Medium |
| ~~5~~ | ~~Project name~~ | **RESOLVED**: **Alamo** (§13.1) | ✅ |
| 5b | **Final bundle identifier** — ✅ resolved: `io.github.axel769.alamo` | §13.1. Fixed on 2026-10-01 (replaces the provisional id of 2026-08-07), before any signed build | **Closed** |
| 6 | **Editing without PK** — block, or generate `UPDATE` matching on all columns (risky)? | Phase M4 | Low |
| 7 | Final constants (64k rows/chunk, 256–512 MB, 64 KB LOB) | To be confirmed with the M0 measurements, now that the thresholds are fixed (§15, M0) | `[VERIFY M0]` |
| 8 | **Scope of the "first write" confirmation** — transaction (recommended) or session? | §9.7 | **High** |
| 9a | ~~Colour tokens~~ | **CLOSED**: §9.6.1 approved (iter. 4, all thresholds pass) | ✅ |
| 9b | ~~Typography~~ | **CLOSED**: §9.6.2 — mono on all cells, Inter for the chrome, 26px, 13px. The choice of the specific monospace remains, to be validated in the mock | ✅ |
| 9c | ~~Component library~~ | **CLOSED**: §9.9 — Radix + Tailwind + selective shadcn | ✅ |
| 9d | ~~Visual validation~~ | **CLOSED** (2026-07-30): §9.10 executed, mocks A/B/D approved — iter. 4 tokens confirmed | ✅ |
| 10 | ~~Sort and filter: local or re-query?~~ | **CLOSED** (2026-07-30): staged. **M2**: local only on a permutation (§4.8), header click; on a truncated result set an explicit badge "only the N downloaded rows are sorted" (§7.3). **M3+**: server re-query as a separate action in the context menu, never the same gesture as the local sort. **Revised 2026-10-15** (real use, as in DBeaver): in the **table tab** the header click sorts **on the server** (Shift+click adds the column: multi-column `ORDER BY`, priority shown in the header), the local sort stays in the header menu ("Sort only the downloaded rows", ⇡/⇣ mark). The local sort on click led to sorting 1,000 rows out of 17,000 without noticing. In **SQL tabs** the click remains the local sort (the user's SQL is not rewritten, §5.2). The two gestures stay distinct per tab | ✅ |
| 11 | **Copy formats** — TSV for Excel, CSV, JSON, `INSERT`; with or without header | Grid semantics | Medium |
| 12 | **Selection model** — cells, rows, columns, multiple ranges | Grid semantics | Medium |
| 13 | **Split panes in v1?** (side-by-side comparison of two tables) | The tab group model allows it, the implementation does not | Low |
| 14 | **UI language** — IT, EN, both? If i18n is planned it must be set up from the start | Touches every string if added later | Medium |
| 15 | **Telemetry yes/no** | For a tool that connects to corporate DBs, "no telemetry" is a marketing feature | Medium |
| ~~16~~ | ~~Linux distro baseline~~ | `[DEFERRED]` Needed only if Linux comes back. For the M0 smoke test an Ubuntu LTS VM is enough | — |
| ~~17~~ | ~~Linux package formats~~ | `[DEFERRED]` M5, only if Linux comes back | — |
| ~~18~~ | ~~Updater strategy on Linux~~ | `[DEFERRED]` M5, only if Linux comes back | — |
| 19 | ~~Bundled font~~ | **RESOLVED**: §9.6.2 — Inter (chrome) + an OFL monospace (grid), both bundled. Which mono remains to be chosen, in the mock | ✅ |
| ~~21~~ | ~~AI features~~ | **RESOLVED** (2026-10-26): the user's own API key. No recurring cost for us, no proxy server, prompts and schema go straight from the app to the provider the user chose | ✅ |

---

## 15. Roadmap

### M0 — Validation spike ⚠️ *before everything else*

#### What is really needed

The spike requires **a single PostgreSQL instance** (the latest). The version matrix of §15.1
is an M1 and M3 problem: **it does not block M0**.

#### Phases

1. **Rust CLI** (no webview): query on the canonical tables → columnar → measurements.
2. **Minimal Tauri + Glide** scrolling over the dataset.
3. **Windows and macOS** — the real gates.
4. **Linux/WebKitGTK smoke test** `[INFORMATIVE, NOT BLOCKING]` — §1.2.

#### Method — without this the numbers mean nothing

**1. "1M rows" is not a benchmark.** Six integer columns and fourteen with JSON and long
text differ by an order of magnitude. Fixed canonical schema, below.

**2. Measure the ceiling first.** Run a *"read and discard"* that builds nothing: it
says how much socket + server can deliver on that machine. If local PG delivers
250 MB/s, the decoder will never go beyond.
→ **The real metric is the distance from the ceiling, not the absolute value.**

**3. Fixed environment.** Release build with LTO, same machine, local server, no
other load, 5 repetitions, **median**.

#### Canonical dataset

**T1 — narrow** (~60 B/row), best case, measures the per-field overhead:

```sql
CREATE TABLE bench_narrow (
  id         bigint,
  parent_id  bigint,
  qty        integer,
  ratio      double precision,
  is_active  boolean,
  created_at timestamptz
);
```

**T2 — wide** (~500 B/row), realistic case, exercises the fallback paths:

```sql
CREATE TABLE bench_wide (
  id          bigint,
  parent_id   bigint,
  uid         uuid,
  code        varchar(16),
  name        text,               -- ~24 char
  description text,               -- ~120 char, 15% NULL
  amount      numeric(14,2),      -- -> Utf8 (lossy, §4.5)
  ratio       double precision,
  qty         integer,
  is_active   boolean,
  created_at  timestamptz,
  updated_at  timestamptz,
  payload     jsonb,              -- ~180 B -> Utf8
  tags        text[]              -- -> Utf8 fallback (§4.4)
);
```

1M rows each. ⚠️ T2 at 1M rows exceeds the soft budget of §5 (~494 MB): **this is intended**,
it exercises the pause mechanism. For the pure throughput measurement, disable the budget.

Also useful **T3 — 60 integer columns**, to isolate the per-column overhead.

#### Thresholds — three bands

A binary pass/fail is not enough: you need to know **what to do**.

| Metric | 🟢 Green | 🟡 Yellow | 🔴 Red |
|---|---|---|---|
| First row on screen (end-to-end, local) | < 250 ms | 250–600 | > 600 |
| Pipeline overhead after the first bytes | < 30 ms | 30–80 | > 80 |
| T1 throughput | ≥ 1M rows/s | 400k–1M | < 400k |
| T2 throughput | ≥ 300k rows/s | 120k–300k | < 120k |
| RAM / logical size | ≤ 1.3× | 1.3–1.8× | > 1.8× |
| Peak RSS / steady-state RSS | ≤ 1.3× | — | ≥ 2× |
| `fetch_window` (100 rows × 20 cols) | p99 < 10 ms | 10–25 | > 25 |
| **Frame time, continuous scroll over 20k rows** | **p95 ≤ 16.6 ms and ZERO frames > 50 ms** | p95 ≤ 25 ms, max 3 frames > 50 ms | worse |
| Cold start | < 1.2 s | 1.2–2.5 | > 2.5 |

**Frame time is the headline metric**, and "zero frames above 50 ms" weighs more than the average:
a single 200 ms stutter is noticed far more than a constant 20 ms.

**Below 400k rows/s on T1 you are in Node territory** and the choice of Rust stops paying off.

#### What to do per band

| Outcome | Action |
|---|---|
| All green | → M1 |
| Isolated yellow | Proceed, but the item enters an **optimization backlog** with the recorded measurement |
| **Red on frame time** | **Architectural decision: plan B of §1.2** (degradation / DOM grid / stack change) |
| Red on throughput or memory | **Profile before concluding**: it is almost always a hidden per-cell allocation, not a limit of the stack |

⚠️ **The thresholds must be fixed before running the spike** — otherwise you look at the
results and rationalize them. This table is that pre-commitment.

---

### Next phases

| Phase | Content |
|---|---|
| **M1** | Single connection + editor + read-only grid + cancel |
| **M2** | Lazy schema tree + autocomplete + multi-tab |
| **M3** | MySQL / MariaDB dialect |
| **M4** | In-grid editing, export, transactions |
| **M5** | SSH / TLS, keychain, updater, code signing |

**Not to forget in M5**: Tauri updater, Apple notarization (~$99/year),
Windows certificate, CI on Windows + macOS.
*(The Linux packaging work is deferred with §1.2 — items 16–18 of §14.)*

**Integration tests**: `testcontainers-rs` against real servers, from M1 onwards.

---

### 15.1 DB version matrix — ✅ `[APPROVED]`

**Principle: support what is *installed*, not what is *supported*.**
MySQL 8.0 has been EOL since April 2026 and is still everywhere; a database tool that
refuses to connect would be unusable.

| Level | Versions |
|---|---|
| **Tested in CI** (`testcontainers-rs`, 6 containers) | PG **15** and **18** · MySQL **8.0** and **8.4 LTS** · MariaDB **10.11** and **11.8 LTS** |
| **Best effort** (must work, not tested on every commit) | PG from **12** · MySQL **5.7** · MariaDB **10.5** |

Context (July 2026): supported PG lines 14–18, 14 EOL on 12/11/2026, latest stable
18.4, 19 in beta. MySQL 8.0 EOL in April 2026 with 8.0.46. MariaDB 10.6 LTS out of
support since 6/7/2026; 11.8 LTS is the latest, until June 2028. ⚠️ MariaDB LTS releases now last
**3 years**, no longer 5 as up to 11.4.

#### The three divergences that cost code

**1. Authentication.** MySQL 8.4 removed `mysql_native_password` from the defaults; since 8.0.4
the default is `caching_sha2_password`, which at the first handshake requires TLS or an RSA exchange —
by far the most common cause of "I can't connect". MariaDB **does not implement**
`caching_sha2_password` **at all**: it uses native or ed25519.
→ This is why MySQL 8.0 **and** 8.4 are both in CI.

**2. ⚠️ In MariaDB `JSON` is not a native type.** It is an alias of `LONGTEXT` with a
`JSON_VALID()` constraint. The type code from the protocol is **TEXT, not JSON** → the semantic class
`Structured` (§9.6) would not fire and those columns would be coloured as text.
→ Detect it from the constraint during introspection, or accept it consciously.
**First case in which a dialect difference falls back onto the design.**

**3. Dialect identification.** MariaDB historically prefixes `5.5.5-` to the version
string. **The MariaDB flag must be taken from the handshake, never from parsing the version.**

---
