//! # alamo-session
//!
//! Sticky pool per tab, result set registry, execution, cancellation,
//! memory budget (§2). **Does not depend on Tauri**: spikes and tests run from
//! the CLI, the `app` crate only does wiring.
//!
//! The session is multi-dialect (§2.1): it receives the driver chosen by the
//! profile from the registry (§2.2) and the differences live behind the
//! `driver::Connection` trait — zero `if dialect` here.
//!
//! The `§N` references point to `ARCHITECTURE.md` at the repo root.

#![forbid(unsafe_code)]

pub mod driver;
pub mod events;
pub mod exec;
pub mod export;
pub mod introspect;
pub mod registry;
pub mod table;
pub mod txn;

use std::sync::Arc;

use alamo_core::dialect::{Capabilities, Dialect};
use alamo_core::error::DbError;
use alamo_core::schema_info::{ColumnInfo, ForeignKey, RelationInfo, SchemaInfo, SchemaStats};
use alamo_core::stream::FetchOptions;
use alamo_core::tls::TlsOptions;
use alamo_driver_postgres::PgSession;

use driver::{DbConn, Driver, ServerConnId};
use events::ExecEvent;
use exec::Emit;
use registry::{LiveResult, Registry};
pub use table::{CellAssign, ColumnFilter, FilterOp, OrderBy, RowUpdate, TableQuery, TableWrite};
pub use txn::TxnStateOut;

/// Page size of SQL tab queries (like DBeaver): after this many rows the fetch
/// stops; scrolling to the bottom brings another page, "Load all" removes
/// the pause. The SQL is not rewritten (§5.2): no `LIMIT` added.
pub const SQL_PAUSE_ROWS: u64 = 200;

/// Page size of the table tab (§9.5). Constant and not negotiated: the first
/// page arrives with `fetch_table`, the following ones are appended with
/// `fetch_table_more` (`OFFSET`, not keyset).
pub const TABLE_PAGE_LIMIT: u64 = 500;

/// Outcome of `fetch_table_more`: how many rows the result set has now, how
/// many the page added and whether there can be another one (full page).
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TableMoreOut {
    pub rows: u64,
    pub added: u64,
    /// Layout bytes of the added page (for the total in the status bar).
    pub layout_bytes: u64,
    pub more: bool,
}

/// Outcome of `refresh_table_rows`: rows re-read and replaced, and those that
/// were not found again (in the result set or in the database) — for the UI
/// it is the signal to reload the whole table.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RefreshRowsOut {
    pub refreshed: u32,
    pub missing: u32,
}

/// Silence beyond which the sticky is checked with a ping before use.
pub const PING_AFTER_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Outcome of `ensure_connected`: whether the sticky was reopened and what was
/// lost with the old one (the transaction open dies with the server side).
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ReconnectOut {
    pub reconnected: bool,
    /// There was an open transaction: the server rolled it back.
    pub lost_txn: bool,
    /// ...with uncommitted writes: work lost, it must be said.
    pub lost_writes: bool,
}

/// Rows per single re-read SELECT: keeps the WHERE of ORs short.
const REFRESH_KEYS_PER_QUERY: usize = 200;

/// After the server refused the introspection lane for the user's connection
/// limit, the tree stays on the borrowed sticky (§3.2) this long before trying
/// to open its own lane again: every attempt costs a login (and an SSH channel).
const CATALOG_LANE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// Default inactivity timeout on a paused fetch (§5.1): long enough to read
/// the banner and decide, short enough not to leave a transaction open
/// through a lunch break.
pub const PAUSE_TIMEOUT_DEFAULT: std::time::Duration = std::time::Duration::from_secs(180);

/// Server-side net on the idle transaction of prod profiles (§9.7.2): after
/// the client's own rollback (`txn::IDLE_ROLLBACK`, 15 min), for when the
/// app cannot do it (frozen, network gone). The server closes the session
/// and the transaction dies in a rollback; the lost work is reported by the
/// UI at the next statement.
pub const IDLE_TXN_TIMEOUT_SECS: u32 = 1200;

/// Outcome of the multi-cell batch (decision 12). Statement errors live
/// HERE, not in the command's `Err`: the dialog shows them together with
/// the count of what had already gone through.
#[derive(Debug, serde::Serialize)]
pub struct BatchUpdateOut {
    /// Rows affected by each successful `UPDATE`, in batch order
    /// (expected: all 1 — discrepancies reported by the dialog).
    pub applied: Vec<u64>,
    /// Index of the failed statement; the rest did not start.
    pub error_index: Option<u32>,
    pub error: Option<events::ErrorOut>,
    /// true = the batch is inside the sticky's transaction (manual commit §9.7):
    /// no auto-commit, the user closes it with Commit/Rollback.
    pub in_transaction: bool,
    /// true = error on the shared lane, everything undone with ROLLBACK
    /// (all or nothing); the database is as it was before the batch.
    pub rolled_back: bool,
    /// Inserted rows re-read and appended to the `AppendTo` result set (like
    /// the following pages): the grid shows them without reloading. Fewer
    /// than the successful INSERTs = some could not be re-read.
    pub appended: u32,
    /// Deleted rows removed from the view of the `AppendTo` result set.
    /// Fewer than the successful DELETEs = some were not found again.
    pub removed: u32,
    /// Rows of the view after the batch (appended and removed included), for
    /// the grid. `None` without `AppendTo`.
    pub rows: Option<u64>,
}

/// The open result set of the table tab to keep aligned with the batch:
/// inserted rows appended, deleted rows removed from the view. The PK
/// columns are used to re-read the inserted ones where there is no
/// `RETURNING` (MySQL/MariaDB) and to find the deleted ones again.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AppendTo {
    pub result_id: u64,
    #[serde(default)]
    pub key_columns: Vec<String>,
    /// View row of each inserted row, in INSERT order: where the grid showed
    /// it while unsaved (the other new rows and the rows to delete counted).
    /// The saved row stays there instead of going to the bottom. Empty = at
    /// the bottom (local sort: the grid re-sorts anyway).
    #[serde(default)]
    pub at: Vec<u64>,
}

/// A batch statement with, for the INSERTs to append, the way to re-read
/// the row: in the same statement (`RETURNING *`) or with a SELECT right
/// after.
struct BatchItem {
    sql: String,
    fetch: Fetch,
}

enum Fetch {
    None,
    Returning,
    FollowUp(String),
}

/// Rows re-read during the batch, to append to the result set only if the
/// batch is not rolled back. A schema different from the open one = nothing
/// is appended (the UI reloads).
struct Collector {
    expected: Arc<[alamo_core::meta::ColumnMeta]>,
    chunks: Vec<alamo_core::chunk::Chunk>,
    mismatch: bool,
}

/// Role of a lane at birth (see `Session::open_lane`): the sticky carries
/// the transactional regime of §9.7 (idle timeout on prod), the auxiliary
/// ones (introspection, shared) stay in auto-commit. The catalog one
/// additionally has its `search_path` pinned to `pg_catalog`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LaneRole {
    Sticky,
    Shared,
    Catalog,
}

/// The server refused the connection because the user has run out of them
/// (`max_user_connections`). MySQL/MariaDB only for now.
fn is_connection_limit(e: &DbError) -> bool {
    matches!(e.code, alamo_core::error::ErrorCode::MySql { errno, .. }
        if alamo_driver_mysql::error::is_user_connection_limit(errno, &e.message))
}

/// A connected session: sticky + registry + (from first use) the auxiliary
/// connections of §3 (introspection, the shared one of the table tabs).
pub struct Session {
    /// The editor's connection. Replaceable: if it drops (network, Mac asleep,
    /// NAT) `ensure_connected` reopens it at the next use, instead of leaving
    /// the session dead until the profile is closed and reopened.
    sticky: std::sync::RwLock<DbConn>,
    /// Last use of the sticky: below `ping_after_idle` it is assumed alive
    /// without a roundtrip.
    last_used: std::sync::Mutex<std::time::Instant>,
    /// Beyond this silence `ensure_connected` checks the sticky with a
    /// ping before using it. Public: the tests set it to zero.
    pub ping_after_idle: std::time::Duration,
    pub registry: Arc<Registry>,
    /// §5.1: a fetch paused beyond this time is stopped automatically
    /// (partials kept). Public: the tests shorten it.
    pub pause_timeout: std::time::Duration,
    /// Used to open the auxiliary connections to the same server.
    driver: Arc<dyn Driver>,
    url: String,
    /// The profile's TLS regime (§12): every lane is born with it — a
    /// plaintext auxiliary on a TLS profile would be a silent downgrade.
    tls: TlsOptions,
    /// Connection dedicated to introspection (§3): **lazy** — opening it at
    /// connect would keep an unused connection on every server — and
    /// reopened if it dropped in the meantime.
    introspect: tokio::sync::Mutex<Option<DbConn>>,
    /// Statistics lane (`schema_stats`, §3): lazy like introspection, but
    /// separate — on servers with many databases the query can take tens of
    /// seconds and, on the same connection, it kept the schema and table
    /// lists stuck (on MySQL the connection mutex stays held for the whole
    /// query). It is born only at the first request for sizes: whoever does
    /// not ask for them does not open an extra connection.
    stats: tokio::sync::Mutex<Option<DbConn>>,
    /// Shared connection of the read-only table tabs (§3.1): 30 open tabs
    /// do NOT become 30 connections. Lazy like introspection; the dedicated
    /// sticky for editing/transactions comes with M4.
    shared: tokio::sync::Mutex<Option<DbConn>>,
    /// The auxiliary lanes open one at a time: whoever opens must see those
    /// just opened by the others to be able to reuse them when the user has
    /// few connections (see `lazy_session`).
    lane_gate: tokio::sync::Mutex<()>,
    /// Last refusal of the introspection lane for the connection limit: until
    /// `CATALOG_LANE_RETRY` has passed the tree borrows the sticky (§3.2).
    catalog_refused_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// §9.7: production profile — always manual commit, and the server-side
    /// idle net on the sticky.
    production: bool,
    /// §9.7: auto-commit off on the sticky (implicit BEGIN before the
    /// user's SQL, grid edits in the same transaction), explicit
    /// commit/rollback. Always on prod; the other profiles switch it from
    /// the toolbar.
    manual_commit: std::sync::atomic::AtomicBool,
    /// §9.7 point 4: read-only. Double net: the classifier rejects certain
    /// writes before sending them, the server (read-only transactions at
    /// session level, on every lane) also rejects those the classifier
    /// cannot read.
    read_only: bool,
    /// Transaction state of the sticky (§9.7): server truth on MySQL,
    /// tracking of control statements on PG.
    txn: Arc<txn::TxnTracker>,
    /// Every connection opened on the server, kept for when the session
    /// ends: after a sleep they may still be alive there (§3.2,
    /// `server_conn_ids`).
    opened: std::sync::Mutex<Vec<ServerConnId>>,
}

/// A cell as text, the whole value even when it is a `Lob`.
fn chunk_text(chunk: &alamo_core::chunk::Chunk, row: usize, col: usize) -> Option<String> {
    let value = chunk.value_at(row, col)?;
    if let alamo_core::value::ValueRef::Lob { handle, .. } = value {
        let bytes = chunk.lobs().get(handle)?;
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    value.to_canonical_string()
}

impl Session {
    /// Bare connection string, without a profile (bench, smoke, tests).
    pub async fn connect(url: &str) -> Result<Session, Box<DbError>> {
        Self::connect_opts(
            driver::builtin_for_url(url),
            url,
            false,
            false,
            TlsOptions::plain(),
        )
        .await
    }

    /// `production`: enables the §9.7 regime on the sticky. `read_only`:
    /// read-only (§9.7 point 4), sets read-only transactions at session
    /// level — if the SET fails the connect fails: promising read-only
    /// without the server net would be a false sense of security. `tls`:
    /// the profile's TLS regime (§12), applied to every lane.
    pub async fn connect_opts(
        driver: Arc<dyn Driver>,
        url: &str,
        production: bool,
        read_only: bool,
        tls: TlsOptions,
    ) -> Result<Session, Box<DbError>> {
        let opened = std::sync::Mutex::new(Vec::new());
        let sticky = Self::open_lane(
            &*driver,
            &opened,
            url,
            &tls,
            production,
            read_only,
            LaneRole::Sticky,
        )
        .await?;
        let transactional_ddl = sticky.dialect().capabilities().transactional_ddl;
        Ok(Session {
            sticky: std::sync::RwLock::new(sticky),
            last_used: std::sync::Mutex::new(std::time::Instant::now()),
            ping_after_idle: PING_AFTER_IDLE,
            registry: Arc::new(Registry::new()),
            pause_timeout: PAUSE_TIMEOUT_DEFAULT,
            driver,
            url: url.to_owned(),
            tls,
            introspect: tokio::sync::Mutex::new(None),
            stats: tokio::sync::Mutex::new(None),
            shared: tokio::sync::Mutex::new(None),
            lane_gate: tokio::sync::Mutex::new(()),
            catalog_refused_at: std::sync::Mutex::new(None),
            production,
            manual_commit: std::sync::atomic::AtomicBool::new(production),
            read_only,
            txn: Arc::new(txn::TxnTracker::new(transactional_ddl)),
            opened,
        })
    }

    /// The connections this session opened on the server. When it ends
    /// without the server noticing (network or tunnel dead after a sleep)
    /// they stay there, counted against the user's limit until
    /// `wait_timeout`: the app keeps them and the next session of the same
    /// profile closes them with `kill_abandoned`.
    pub fn server_conn_ids(&self) -> Vec<ServerConnId> {
        self.opened.lock().unwrap().clone()
    }

    /// Closes on the server the connections `old` left behind by a
    /// previous session of the same profile (see `server_conn_ids`), from
    /// the sticky right after connect. Only those of the same server life,
    /// idle and of the same user: whatever is gone or not ours is skipped.
    pub async fn kill_abandoned(&self, old: &[ServerConnId]) {
        self.sticky().kill_abandoned(old).await;
    }

    pub fn production(&self) -> bool {
        self.production
    }

    pub fn manual_commit(&self) -> bool {
        self.manual_commit
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Toolbar switch (§9.7). Refused on prod (always manual), on
    /// read-only (nothing to commit) and with a transaction open: switching
    /// would leave it neither committed nor rolled back.
    pub fn set_manual_commit(&self, on: bool) -> Result<(), Box<DbError>> {
        if self.production || self.read_only {
            return Err(Box::new(DbError::internal(
                "the commit mode of a production or read-only profile cannot change",
            )));
        }
        if self.txn.snapshot().open {
            return Err(Box::new(DbError::internal(
                "a transaction is open: Commit or Rollback it first",
            )));
        }
        self.manual_commit
            .store(on, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// The current editor connection (cheap clone: Arc inside).
    fn sticky(&self) -> DbConn {
        self.sticky.read().unwrap().clone()
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = std::time::Instant::now();
    }

    /// Before using the sticky (Run, database change): if it has been idle
    /// beyond `ping_after_idle` it is checked; if it is dead it is reopened
    /// with the profile's regime (TLS, prod, read-only). With an execution
    /// running nothing is touched. The old connection's transaction is
    /// lost (the server rolled it back): the outcome says so, the UI shows it.
    pub async fn ensure_connected(&self) -> Result<ReconnectOut, Box<DbError>> {
        let fine = ReconnectOut {
            reconnected: false,
            lost_txn: false,
            lost_writes: false,
        };
        if self.registry.has_active_execution() {
            return Ok(fine);
        }
        let idle = self.last_used.lock().unwrap().elapsed();
        let conn = self.sticky();
        // PG knows by itself whether the connection is closed: the ping is
        // needed only after a silence. MySQL finds out only by using it — a
        // query that just failed because of a dropped connection and is retried
        // right away must find the new connection — so the ping (a roundtrip)
        // is always done.
        let cheap = (idle < self.ping_after_idle)
            .then(|| conn.healthy_cheap())
            .flatten();
        let alive = match cheap {
            Some(alive) => alive,
            None => conn.ping().await,
        };
        if alive {
            self.touch();
            return Ok(fine);
        }
        let lost = self.txn.snapshot();
        let fresh = Self::open_lane(
            &*self.driver,
            &self.opened,
            &self.url,
            &self.tls,
            self.production,
            self.read_only,
            LaneRole::Sticky,
        )
        .await?;
        // On the server the old one may still be alive (see
        // `Connection::kill_abandoned`): it must not keep occupying a slot.
        fresh.kill_abandoned(conn.server_conn_id().as_slice()).await;
        *self.sticky.write().unwrap() = fresh;
        self.txn.reset();
        self.touch();
        Ok(ReconnectOut {
            reconnected: true,
            lost_txn: lost.open,
            lost_writes: lost.has_writes,
        })
    }

    /// The user's "Reconnect": reopens the sticky without asking whether it
    /// is alive — with the network just back the check can wait minutes on
    /// a connection that died silently — and closes on the server the old
    /// one and the auxiliary ones, which reopen at first use (one being
    /// reopened, slot busy, manages by itself). The caller rules out running
    /// executions.
    pub async fn force_reconnect(&self) -> Result<ReconnectOut, Box<DbError>> {
        let fresh = Self::open_lane(
            &*self.driver,
            &self.opened,
            &self.url,
            &self.tls,
            self.production,
            self.read_only,
            LaneRole::Sticky,
        )
        .await?;
        let lost = self.txn.snapshot();
        let old = std::mem::replace(&mut *self.sticky.write().unwrap(), fresh.clone());
        fresh.kill_abandoned(old.server_conn_id().as_slice()).await;
        for slot in [&self.introspect, &self.stats, &self.shared] {
            let Ok(mut lane) = slot.try_lock() else {
                continue;
            };
            if let Some(stale) = lane.take() {
                fresh
                    .kill_abandoned(stale.server_conn_id().as_slice())
                    .await;
            }
        }
        self.txn.reset();
        self.touch();
        Ok(ReconnectOut {
            reconnected: true,
            lost_txn: lost.open,
            lost_writes: lost.has_writes,
        })
    }

    /// The channel of the editor connection is encrypted (see
    /// `Connection::channel_encrypted`).
    pub async fn channel_encrypted(&self) -> Result<bool, Box<DbError>> {
        self.sticky().channel_encrypted().await
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Current transaction state of the sticky (§9.7).
    pub fn txn_state(&self) -> TxnStateOut {
        self.txn.snapshot()
    }

    /// Writes of the open transaction, listed before Commit (§9.7.2).
    pub fn txn_log(&self) -> txn::TxnLogOut {
        self.txn.log()
    }

    /// Explicit COMMIT on the sticky (§9.7 button). Sticky free.
    pub async fn commit(&self) -> Result<TxnStateOut, Box<DbError>> {
        self.txn_exec("COMMIT").await
    }

    /// Explicit ROLLBACK on the sticky (§9.7 button). Sticky free.
    pub async fn rollback(&self) -> Result<TxnStateOut, Box<DbError>> {
        self.txn_exec("ROLLBACK").await
    }

    /// §9.7.2: ROLLBACK of a transaction with no statement for `limit`.
    /// Returns the state it had (`has_writes` = work lost), `None` when
    /// there was nothing to do: no idle transaction, or the sticky busy —
    /// a running or paused statement is not idleness.
    pub async fn rollback_if_idle(
        &self,
        limit: std::time::Duration,
    ) -> Result<Option<TxnStateOut>, Box<DbError>> {
        if !self.txn.idle_expired(limit) {
            return Ok(None);
        }
        let Some(exec_id) = self.registry.try_new_execution(self.sticky().canceller()) else {
            return Ok(None);
        };
        // Checked again with the sticky held: a statement may have ended
        // between the first check and the registration.
        if !self.txn.idle_expired(limit) {
            self.registry
                .set_exec_state(exec_id, registry::ExecState::Completed);
            return Ok(None);
        }
        let before = self.txn.snapshot();
        self.txn_exec_on(exec_id, "ROLLBACK").await?;
        Ok(Some(before))
    }

    async fn txn_exec(&self, sql: &str) -> Result<TxnStateOut, Box<DbError>> {
        // COMMIT/ROLLBACK also occupy the sticky like an execution: without
        // registration, a concurrent `execute` would see the lane free and
        // would start interleaved on the COMMIT (with `begin_first` computed
        // on a snapshot that is by then stale).
        let exec_id = self
            .registry
            .try_new_execution(self.sticky().canceller())
            .ok_or_else(|| {
                Box::new(DbError::internal(
                    "running: commit/rollback need the sticky connection free",
                ))
            })?;
        self.txn_exec_on(exec_id, sql).await
    }

    /// COMMIT/ROLLBACK on the sticky already registered as `exec_id`.
    async fn txn_exec_on(&self, exec_id: u64, sql: &str) -> Result<TxnStateOut, Box<DbError>> {
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let res = self
            .sticky()
            .execute_streaming(sql, &FetchOptions::default(), &mut rx, &mut |_| {})
            .await
            .map_err(|e| *e);
        let out = match res {
            Ok(out) => out,
            Err(e) => {
                // Transport error on COMMIT/ROLLBACK: the connection is
                // compromised and the real state of the transaction can no
                // longer be known by the client. The reset is the honest
                // choice — keeping on showing an "open" transaction on a dead
                // connection would be a false promise. (The application SQL
                // error, below, does NOT reset: there the transaction may
                // still be alive on the server.)
                self.txn.reset();
                self.registry
                    .set_exec_state(exec_id, registry::ExecState::Failed);
                return Err(Box::new(e));
            }
        };
        if let Some(e) = out.error {
            self.registry
                .set_exec_state(exec_id, registry::ExecState::Failed);
            return Err(Box::new(e));
        }
        self.registry
            .set_exec_state(exec_id, registry::ExecState::Completed);
        self.txn
            .apply_statement(sql, true, out.in_transaction, None);
        Ok(self.txn.snapshot())
    }

    /// Export/formatted copy of the result set (decision 11): renders to
    /// `out` the requested range (`None` = everything). Returns the rows
    /// written. Reads from the registry: exports what has been downloaded,
    /// in the view's order (local sort included).
    pub fn export_result(
        &self,
        result_id: u64,
        opts: &export::ExportOptions,
        rows: Option<std::ops::Range<u64>>,
        cols: Option<std::ops::Range<usize>>,
        out: &mut dyn std::io::Write,
    ) -> Result<u64, Box<DbError>> {
        let live = self.registry.result(result_id).ok_or_else(|| {
            Box::new(DbError::internal(
                "result set does not exist or was already released",
            ))
        })?;
        let n_cols = live.schema().len();
        let rows = rows.unwrap_or(0..live.rows());
        let cols = cols.unwrap_or(0..n_cols);
        export::render(
            &live,
            self.sticky().dialect().as_ref(),
            opts,
            rows,
            cols,
            out,
        )
    }

    /// All the results of a script in one file (`export::render_script`).
    pub fn export_script(
        &self,
        items: &[export::ScriptItem],
        opts: &export::ExportOptions,
        out: &mut dyn std::io::Write,
    ) -> Result<u64, Box<DbError>> {
        let results = self.script_results(items)?;
        export::render_script(&results, self.sticky().dialect().as_ref(), opts, out)
    }

    /// All the results of a script, one file each in a new folder `name`
    /// inside `parent`. Returns the folder and the rows written.
    pub fn export_script_to_dir(
        &self,
        items: &[export::ScriptItem],
        opts: &export::ExportOptions,
        parent: &std::path::Path,
        name: &str,
    ) -> Result<(std::path::PathBuf, u64), Box<DbError>> {
        // Every result checked before creating anything on disk.
        let results = self.script_results(items)?;
        let dir = export::create_export_dir(parent, name)?;
        let rows =
            export::write_script_dir(&results, self.sticky().dialect().as_ref(), opts, &dir)?;
        Ok((dir, rows))
    }

    fn script_results<'a>(
        &self,
        items: &'a [export::ScriptItem],
    ) -> Result<Vec<(&'a export::ScriptItem, LiveResult)>, Box<DbError>> {
        items
            .iter()
            .map(|it| {
                let live = self.registry.result(it.result_id).ok_or_else(|| {
                    Box::new(DbError::internal(format!(
                        "result set of statement {} was already released",
                        it.position
                    )))
                })?;
                Ok((it, live))
            })
            .collect()
    }

    /// The session's dialect (§2.1): the UI queries the `Capabilities`,
    /// never the product name.
    pub fn dialect(&self) -> Arc<dyn Dialect> {
        self.sticky().dialect()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.sticky().dialect().capabilities()
    }

    /// Introspection connection (§3): opened at first use, reused afterwards;
    /// if it dropped (server restarted, network) it is reopened transparently.
    /// §3.2: when the user's connection limit leaves no room for a lane of
    /// its own, nor one to reuse, the tree borrows the sticky while it runs
    /// nothing — like DBeaver without "separate connection for metadata".
    /// Statistics never borrow it: their query can keep the editor waiting
    /// for tens of seconds.
    async fn introspect_session(&self) -> Result<DbConn, Box<DbError>> {
        let refused = *self.catalog_refused_at.lock().unwrap();
        if refused.is_some_and(|t| t.elapsed() < CATALOG_LANE_RETRY) {
            return self.borrow_sticky();
        }
        match self.lazy_session(&self.introspect, LaneRole::Catalog).await {
            Err(e) if is_connection_limit(&e) => {
                *self.catalog_refused_at.lock().unwrap() = Some(std::time::Instant::now());
                self.borrow_sticky()
            }
            other => other,
        }
    }

    /// The sticky for a catalog read, only between executions: during one
    /// (or a paused fetch) the read would wait on the connection for as long
    /// as the user's query lasts. Catalog reads change no session state the
    /// user relies on, except the diagnostics of the last statement
    /// (`SHOW WARNINGS`, `ROW_COUNT()`): accepted, see §3.2.
    fn borrow_sticky(&self) -> Result<DbConn, Box<DbError>> {
        if self.registry.has_active_execution() {
            let mut e = DbError::internal(
                "no free connection for the tree: the editor's one is running a query",
            );
            e.hint = Some(
                "This database user has no free connection for the tree, so it shares the editor's one, which is running a query. Retry when the query ends."
                    .into(),
            );
            return Err(Box::new(e));
        }
        Ok(self.sticky())
    }

    /// Statistics lane (§3), same lazy policy and same catalog role
    /// (`search_path` pinned to `pg_catalog`).
    async fn stats_session(&self) -> Result<DbConn, Box<DbError>> {
        self.lazy_session(&self.stats, LaneRole::Catalog).await
    }

    /// Shared connection of the table tabs (§3.1), same lazy policy.
    async fn shared_session(&self) -> Result<DbConn, Box<DbError>> {
        self.lazy_session(&self.shared, LaneRole::Shared).await
    }

    /// User with few connections (`max_user_connections`): a new lane that
    /// would leave the user without free slots is not kept, another already
    /// open auxiliary is reused instead — tree, table tabs and statistics
    /// end up on the same connection and wait for each other. The free
    /// slot is needed by the `KILL QUERY` of "Cancel" and by the user's
    /// other programs (HeidiSQL, DBeaver). Same path if the server refuses
    /// the opening because the slots have already run out. The first
    /// auxiliary is kept anyway: without it, the tree does not open. The
    /// sticky is never shared (the user's transaction, paused results).
    async fn lazy_session(
        &self,
        slot: &tokio::sync::Mutex<Option<DbConn>>,
        role: LaneRole,
    ) -> Result<DbConn, Box<DbError>> {
        let mut guard = slot.lock().await;
        if let Some(s) = guard.as_ref() {
            if s.healthy().await {
                return Ok(s.clone());
            }
        }
        // Dead for us: it must be closed on the server, as soon as the replacement exists.
        let stale = guard.take();
        let gate = self.lane_gate.lock().await;
        let s = match Self::open_lane(
            &*self.driver,
            &self.opened,
            &self.url,
            &self.tls,
            self.production,
            self.read_only,
            role,
        )
        .await
        {
            Ok(fresh) if Self::leaves_no_room(&fresh).await => {
                match self.open_aux_lane(slot).await {
                    Some(other) => {
                        fresh.close().await;
                        other
                    }
                    None => fresh,
                }
            }
            Ok(fresh) => fresh,
            Err(e) if is_connection_limit(&e) => match self.open_aux_lane(slot).await {
                Some(other) => other,
                None => {
                    *guard = stale;
                    return Err(e);
                }
            },
            // The old one goes back to its place: whoever retries (or "Reconnect")
            // must be able to close it on the server. Lost here, it stayed a zombie
            // in `Sleep` occupying one of the user's slots.
            Err(e) => {
                *guard = stale;
                return Err(e);
            }
        };
        if let Some(old) = &stale {
            s.kill_abandoned(old.server_conn_id().as_slice()).await;
        }
        *guard = Some(s.clone());
        // First the slot, then the gate: whoever waits on the gate must
        // find the slot already free and the lane inside.
        drop(guard);
        drop(gate);
        Ok(s)
    }

    /// With `fresh` open the user has no free slots left. When in doubt
    /// (unreadable budget) no: stick to the usual behaviour.
    async fn leaves_no_room(fresh: &DbConn) -> bool {
        matches!(fresh.connection_budget().await, Ok(Some(b)) if b.in_use >= b.limit)
    }

    /// Another open and alive auxiliary, to reuse. `try_lock`: a busy slot
    /// belongs to whoever is opening their lane or checking it — waiting for
    /// it, with the gate in hand, could block everything.
    async fn open_aux_lane(&self, except: &tokio::sync::Mutex<Option<DbConn>>) -> Option<DbConn> {
        for slot in [&self.introspect, &self.shared, &self.stats] {
            if std::ptr::eq(slot, except) {
                continue;
            }
            let Ok(g) = slot.try_lock() else { continue };
            if let Some(s) = g.as_ref() {
                if s.healthy().await {
                    return Some(s.clone());
                }
            }
        }
        None
    }

    /// Single factory of the lanes: EVERY connection to the server is born
    /// here, with the profile's regime applied atomically. Bare
    /// `Driver::connect` is regime-unaware on purpose — a new lane born
    /// elsewhere would silently skip read-only (§9.7 point 4), which must
    /// cover all lanes: the grid editing UPDATE goes through the shared
    /// one, not the sticky.
    async fn open_lane(
        driver: &dyn Driver,
        opened: &std::sync::Mutex<Vec<ServerConnId>>,
        url: &str,
        tls: &TlsOptions,
        production: bool,
        read_only: bool,
        role: LaneRole,
    ) -> Result<DbConn, Box<DbError>> {
        let conn = driver.connect(url, tls).await?;
        opened.lock().unwrap().extend(conn.server_conn_id());
        // Read-only: if the SET fails, the opening fails — promising the
        // server net without having it would be a false sense of security.
        if read_only {
            conn.set_session_read_only().await?;
        }
        // Catalog queries start by themselves (tree, autocomplete): an
        // object placed by others in a schema of the path (`public.count(oid)`)
        // must never run with the user's permissions. Same policy as
        // read-only: without the SET the lane does not open.
        if role == LaneRole::Catalog {
            conn.pin_catalog_search_path().await?;
        }
        // Additional net of §9.7.2, best-effort: on servers without the
        // variable (MySQL, ancient PG) we carry on with the UI timer only.
        // Only on the sticky: the auxiliary ones stay in auto-commit and
        // never hold an idle transaction. Also in read-only: a forgotten
        // manual BEGIN holds the snapshot and blocks vacuum even
        // without writes.
        if production && role == LaneRole::Sticky {
            let _ = conn.set_idle_txn_timeout(IDLE_TXN_TIMEOUT_SECS).await;
        }
        Ok(conn)
    }

    pub async fn schemas(&self) -> Result<Vec<SchemaInfo>, Box<DbError>> {
        self.introspect_session().await?.schemas().await
    }

    /// Databases of the server (PG: each one with its own connection, see
    /// `can_change_database_in_session`).
    pub async fn databases(&self) -> Result<Vec<SchemaInfo>, Box<DbError>> {
        self.introspect_session().await?.databases().await
    }

    /// Numbers per schema/database: on the statistics lane, never on the
    /// introspection one — a slow query must not keep the tree
    /// waiting.
    pub async fn schema_stats(&self) -> Result<Vec<SchemaStats>, Box<DbError>> {
        self.stats_session().await?.schema_stats().await
    }

    pub async fn relations(&self, schema: &str) -> Result<Vec<RelationInfo>, Box<DbError>> {
        self.introspect_session().await?.relations(schema).await
    }

    /// Names of the relations of every schema/database in one round trip
    /// (tree filter): no row estimates, no comments.
    pub async fn relations_all(&self) -> Result<Vec<RelationInfo>, Box<DbError>> {
        self.introspect_session().await?.relations_all().await
    }

    pub async fn columns(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnInfo>, Box<DbError>> {
        self.introspect_session()
            .await?
            .columns(schema, table)
            .await
    }

    /// Foreign keys touching the table, in both directions.
    pub async fn foreign_keys(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>, Box<DbError>> {
        self.introspect_session()
            .await?
            .foreign_keys(schema, table)
            .await
    }

    /// Flat catalog for autocomplete (§10): (schema, table, column).
    /// `schema` = only that schema/database (fast and never truncated by the
    /// cap); `None` = the whole server, up to the cap. The whole server goes
    /// on the statistics lane (§3): it runs at every connect and takes
    /// seconds on servers with thousands of tables, the tree and the
    /// connection page must not wait behind it.
    pub async fn autocomplete_catalog(
        &self,
        schema: Option<&str>,
    ) -> Result<Vec<(String, String, String)>, Box<DbError>> {
        let lane = match schema {
            Some(_) => self.introspect_session().await?,
            None => self.stats_session().await?,
        };
        lane.autocomplete_catalog(schema).await
    }

    /// DDL of the relation (DDL sub-tab, §9.5). Runs on introspection:
    /// it is catalog, not data.
    pub async fn table_ddl(&self, schema: &str, table: &str) -> Result<String, Box<DbError>> {
        self.introspect_session()
            .await?
            .table_ddl(schema, table)
            .await
    }

    /// Rows of one catalog query as text, for what an extension reads beyond
    /// the methods above. On the statistics lane: the tree must not wait
    /// behind it. No byte budget, a catalog is bounded by the schema and not
    /// by the data; long values (view bodies) come back whole, not as a
    /// `Lob` prefix (§4.7). A user with no connection left goes where the
    /// tree goes.
    pub async fn catalog_rows(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, Box<DbError>> {
        let conn = match self.stats_session().await {
            Err(e) if is_connection_limit(&e) => self.introspect_session().await?,
            other => other?,
        };
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let opts = FetchOptions {
            soft_budget_bytes: None,
            ..FetchOptions::default()
        };
        let mut rows = Vec::new();
        let out = conn
            .execute_streaming(sql, &opts, &mut rx, &mut |ev| {
                if let alamo_core::stream::StreamEvent::Chunk { chunk, .. } = ev {
                    for r in 0..chunk.rows() as usize {
                        rows.push(
                            (0..chunk.columns().len())
                                .map(|c| chunk_text(&chunk, r, c))
                                .collect(),
                        );
                    }
                }
            })
            .await?;
        match out.error {
            Some(e) => Err(Box::new(e)),
            None => Ok(rows),
        }
    }

    /// Access to the PG driver for the measuring paths (bench M0): the app must
    /// not go through here for normal execution. `None` on other dialects.
    pub fn pg(&self) -> Option<Arc<PgSession>> {
        let conn = self.sticky();
        let pg = conn.as_any().downcast_ref::<driver::PgConn>()?;
        Some(Arc::clone(&pg.0))
    }

    /// §8: returns the `exec_id` immediately; everything else arrives as events.
    ///
    /// A single active execution: the sticky is a single connection and
    /// queueing silently would hide from the user *where* they are waiting.
    pub fn execute(
        &self,
        sql: String,
        opts: FetchOptions,
        emit: impl Fn(ExecEvent) + Send + Sync + 'static,
    ) -> Result<u64, Box<DbError>> {
        self.check_read_only(&sql)?;
        self.touch();
        let exec_id = self
            .registry
            .try_new_execution(self.sticky().canceller())
            .ok_or_else(|| {
                Box::new(DbError::internal(
                    "an execution is already running on this connection",
                ))
            })?;
        let emit: Emit = Arc::new(emit);
        // §9.7: only the user's SQL on the sticky tracks (and opens) transactions.
        // In read-only no implicit BEGIN of the prod regime: with no
        // possible writes the manual commit protects nothing and the
        // idle transaction is only a cost (§9.7.2). On PG however the
        // batch runs in a READ ONLY transaction opened and closed here
        // (`read_only_txn`, see `check_read_only`).
        let txn = exec::TxnCtx {
            tracker: Arc::clone(&self.txn),
            begin_first: self.manual_commit() && !self.read_only && !self.txn.snapshot().open,
        };
        tokio::spawn(exec::run_execution(
            self.sticky(),
            Arc::clone(&self.registry),
            exec_id,
            sql,
            exec::ExecConfig {
                opts,
                pause_timeout: self.pause_timeout,
                txn: Some(txn),
                read_only_txn: self.read_only && self.capabilities().read_only_txn,
            },
            emit,
        ));
        Ok(exec_id)
    }

    /// Data of a table for the table tab (§9.5): SQL generated by the app —
    /// the no-rewrite rule (§5.2) applies to the *user's* SQL — on the
    /// shared connection (§3.1), so the sticky stays free. The quoting
    /// is the dialect's (backticks on MySQL, §2.1). `query` carries server-side
    /// sort and filters (decision 10, second half): in the table tab the
    /// click on the header, distinct from the local sort (which is in the menu).
    pub async fn fetch_table(
        &self,
        schema: &str,
        table: &str,
        query: &TableQuery,
        opts: FetchOptions,
        emit: impl Fn(ExecEvent) + Send + Sync + 'static,
    ) -> Result<u64, Box<DbError>> {
        let dialect = self.sticky().dialect();
        let sql = table::table_sql(dialect.as_ref(), schema, table, query, TABLE_PAGE_LIMIT);
        let read_only_txn = self.check_table_query(&sql, query)?;
        // Advisory check BEFORE the await: on MySQL `healthy()` (inside
        // `shared_session`) takes the session mutex — if the shared one
        // is streaming, the call would hang instead of answering
        // "busy" right away. The authority remains the atomic
        // `try_new_execution_on` below; this only serves to answer right away.
        if self.registry.has_active_on(registry::Lane::Shared) {
            return Err(Box::new(DbError::internal(
                "another table tab is already loading on the shared connection",
            )));
        }
        let shared = self.shared_session().await?;
        let exec_id = self
            .registry
            .try_new_execution_on(registry::Lane::Shared, shared.canceller())
            .ok_or_else(|| {
                Box::new(DbError::internal(
                    "another table tab is already loading on the shared connection",
                ))
            })?;
        let emit: Emit = Arc::new(emit);
        tokio::spawn(exec::run_execution(
            shared,
            Arc::clone(&self.registry),
            exec_id,
            sql,
            exec::ExecConfig {
                opts,
                pause_timeout: self.pause_timeout,
                txn: None,
                read_only_txn,
            },
            emit,
        ));
        Ok(exec_id)
    }

    /// Next page of the table tab: the same SELECT as `fetch_table` with
    /// `OFFSET` = rows already in the result set, appended to THAT result
    /// set (same id: grid, selection and edits stay).
    /// Shared lane, like the first page. Here we wait for the end: the
    /// page is small and the UI asks for it by itself when reaching the bottom.
    ///
    /// Without `ORDER BY` the order between one page and the next is decided
    /// by the server: on a table that changes in the meantime a row can
    /// repeat or be skipped (same limit as any `OFFSET` paging).
    pub async fn fetch_table_more(
        &self,
        schema: &str,
        table: &str,
        query: &TableQuery,
        result_id: u64,
    ) -> Result<TableMoreOut, Box<DbError>> {
        let busy = || {
            Box::new(DbError::internal(
                "another table tab is already loading on the shared connection",
            ))
        };
        // The check of the hand-written WHERE is on the first page: OFFSET
        // changes neither the number of statements nor what the SELECT reads.
        let dialect = self.sticky().dialect();
        let read_only_txn = self.check_table_query(
            &table::table_sql(dialect.as_ref(), schema, table, query, TABLE_PAGE_LIMIT),
            query,
        )?;
        // Advisory check before the await, as in `fetch_table`.
        if self.registry.has_active_on(registry::Lane::Shared) {
            return Err(busy());
        }
        let live = self
            .registry
            .result(result_id)
            .ok_or_else(|| Box::new(DbError::internal("result set not found: reload the table")))?;
        if matches!(
            live.state(),
            alamo_core::result_set::ResultSetState::Streaming
                | alamo_core::result_set::ResultSetState::Paused
        ) {
            return Err(busy());
        }
        let shared = self.shared_session().await?;
        let exec_id = self
            .registry
            .try_new_execution_on(registry::Lane::Shared, shared.canceller())
            .ok_or_else(busy)?;
        // OFFSET: the rows the server has already sent and still has (excluding
        // those inserted and deleted from the grid, see `server_offset`).
        let before = live.phys_rows();
        let sql = table::table_page_sql(
            dialect.as_ref(),
            schema,
            table,
            query,
            TABLE_PAGE_LIMIT,
            live.server_offset(),
        );
        self.registry
            .set_exec_state(exec_id, registry::ExecState::Running);
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(4);
        self.registry.set_control(exec_id, control_tx);

        // Same net as `fetch_table`: if the read-only transaction
        // does not start, the page is not read.
        if read_only_txn {
            if let Err(e) = shared.begin_read_only_txn().await {
                self.registry
                    .set_exec_state(exec_id, registry::ExecState::Failed);
                return Err(e);
            }
        }
        let expected = live.schema();
        let mut mismatch = false;
        let mut layout_bytes = 0;
        let registry = Arc::clone(&self.registry);
        let outcome = shared
            .execute_streaming(&sql, &FetchOptions::default(), &mut control_rx, &mut |ev| {
                use alamo_core::stream::StreamEvent;
                match ev {
                    StreamEvent::Schema { schema, .. } => {
                        // The table changed between one page and the next:
                        // rows of a different shape are not appended.
                        mismatch = schema.len() != expected.len()
                            || schema
                                .iter()
                                .zip(expected.iter())
                                .any(|(a, b)| a.name != b.name || a.type_display != b.type_display);
                        if mismatch {
                            registry.cancel(exec_id);
                        }
                    }
                    StreamEvent::Chunk {
                        chunk,
                        layout_bytes_total,
                        ..
                    } => {
                        if !mismatch && live.append_chunk(chunk) {
                            layout_bytes = layout_bytes_total;
                        }
                    }
                    // A page does not reach the budget; if it happens it stops
                    // there, without a banner: the rows that arrived stay.
                    StreamEvent::BudgetReached { .. } => registry.budget_stop(exec_id),
                }
            })
            .await
            .map_err(|e| *e);
        // COMMIT even after an error: on an aborted transaction it is equivalent
        // to ROLLBACK (as in `exec::run_execution`).
        let end_error = if read_only_txn {
            shared.end_read_only_txn().await.err().map(|e| *e)
        } else {
            None
        };

        let error = match outcome {
            Err(e) => Some(e),
            Ok(out) => out.error,
        }
        .or(end_error);
        if mismatch {
            self.registry
                .set_exec_state(exec_id, registry::ExecState::Failed);
            return Err(Box::new(DbError::internal(
                "the table structure has changed: reload the table",
            )));
        }
        if let Some(e) = error {
            self.registry
                .set_exec_state(exec_id, registry::ExecState::Failed);
            return Err(Box::new(e));
        }
        self.registry
            .set_exec_state(exec_id, registry::ExecState::Completed);
        let added = live.phys_rows() - before;
        live.hide_duplicates(before..before + added);
        let rows = live.rows();
        Ok(TableMoreOut {
            rows,
            added,
            layout_bytes,
            more: added >= TABLE_PAGE_LIMIT,
        })
    }

    /// Preview of the grid editing `UPDATE` (M4): same deterministic
    /// generation as `update_table`, from the same parameters — the confirmation
    /// shows exactly what will be executed.
    /// The SELECT that `fetch_table` would run for that query: same
    /// generator and same limit, so the table tab can show the
    /// SQL of its filters — nothing hidden.
    pub fn table_sql(&self, schema: &str, table: &str, query: &TableQuery) -> String {
        let dialect = self.sticky().dialect();
        table::table_sql(dialect.as_ref(), schema, table, query, TABLE_PAGE_LIMIT)
    }

    pub fn update_sql(
        &self,
        schema: &str,
        table: &str,
        set: &[table::CellAssign],
        key: &[table::CellAssign],
    ) -> Result<String, Box<DbError>> {
        Self::validate_update(set, key)?;
        let dialect = self.sticky().dialect();
        Ok(table::update_sql(dialect.as_ref(), schema, table, set, key))
    }

    /// `UPDATE` of a row from grid editing (M4). On a normal profile:
    /// shared lane in auto-commit, like `fetch_table`. With manual commit
    /// (§9.7: prod, or the toolbar switch) the hidden auto-commit would be
    /// a circumvention of the regime: the UPDATE runs on the **sticky**,
    /// joins its transaction (implicit BEGIN if none is open) and requires
    /// the user's explicit commit. Same events as `execute`: the outcome comes back as
    /// `Affected { rows }`.
    pub async fn update_table(
        &self,
        schema: &str,
        table: &str,
        set: &[table::CellAssign],
        key: &[table::CellAssign],
        opts: FetchOptions,
        emit: impl Fn(ExecEvent) + Send + Sync + 'static,
    ) -> Result<u64, Box<DbError>> {
        if self.read_only {
            return Err(Box::new(DbError::internal(
                "read-only profile: editing is disabled (§9.7)",
            )));
        }
        Self::validate_update(set, key)?;
        let dialect = self.sticky().dialect();
        let sql = table::update_sql(dialect.as_ref(), schema, table, set, key);
        let emit: Emit = Arc::new(emit);
        let (conn, exec_id, txn) = if self.manual_commit() {
            // §9.7: same lane and same tracking as `execute` — the UPDATE
            // is a write like the others and goes through its gates.
            let exec_id = self
                .registry
                .try_new_execution(self.sticky().canceller())
                .ok_or_else(|| {
                    Box::new(DbError::internal(
                        "running on the main connection: wait for it to finish",
                    ))
                })?;
            let txn = exec::TxnCtx {
                tracker: Arc::clone(&self.txn),
                begin_first: !self.txn.snapshot().open,
            };
            (self.sticky(), exec_id, Some(txn))
        } else {
            // Advisory check BEFORE the await, as in `fetch_table`: on
            // MySQL `healthy()` takes the mutex of the shared session and
            // the call would hang instead of answering right away.
            // The authority remains the atomic `try_new_execution_on`.
            if self.registry.has_active_on(registry::Lane::Shared) {
                return Err(Box::new(DbError::internal(
                    "the shared connection is busy: wait for the table to load",
                )));
            }
            let shared = self.shared_session().await?;
            let exec_id = self
                .registry
                .try_new_execution_on(registry::Lane::Shared, shared.canceller())
                .ok_or_else(|| {
                    Box::new(DbError::internal(
                        "the shared connection is busy: wait for the table to load",
                    ))
                })?;
            (shared, exec_id, None)
        };
        tokio::spawn(exec::run_execution(
            conn,
            Arc::clone(&self.registry),
            exec_id,
            sql,
            exec::ExecConfig {
                opts,
                pause_timeout: self.pause_timeout,
                txn,
                read_only_txn: false,
            },
            emit,
        ));
        Ok(exec_id)
    }

    /// Client gate of read-only (§9.7 point 4): CERTAIN writes stop
    /// here, with a readable error before touching the server. The
    /// unknown (WITH, CALL, DO…) passes: what stops it, if it really
    /// writes, is the server-side read-only session — blocking it here
    /// would prevent legitimate reads the classifier cannot read.
    ///
    /// Stopping at Write|Ddl only is not enough though: the server net is a
    /// session variable, and a `SET`/`RESET` on the variable or a
    /// `BEGIN READ WRITE` (Control class) would disarm it — at that point
    /// the unknown would really write. Those control statements are
    /// stopped here, or the double net is a single net.
    ///
    /// The text is not enough to recognise every disarm: on PG
    /// `SELECT set_config('default_transaction_read_only', 'off', false)`,
    /// a `DO` or a function do it without being readable. That is why on PG
    /// every execution runs in a READ ONLY transaction of the app (see
    /// `exec::ExecConfig::read_only_txn`), which no statement can
    /// make writable: here only the statements that would leave it are
    /// stopped. On MySQL the equivalent of `set_config` is dynamic
    /// SQL (`PREPARE`/`EXECUTE`), which is stopped. What remains outside is a
    /// stored procedure that disarms by itself (`CALL`): against that you need
    /// a database user without write permissions.
    /// WHERE bar of the table tab: the user's text goes into a
    /// SELECT of the app, and the table tab does not go through `execute`. Without
    /// this check `1=1; DELETE ...` would become a second
    /// statement (`exec::run_execution` splits on `;`) outside read-only
    /// and confirmations. `sql` is the complete SELECT: the split sees the
    /// text in its context (open strings and comments included).
    ///
    /// Returns whether the read must be done in a READ ONLY transaction
    /// (read-only profile on PG): a function in the WHERE could write.
    /// Without hand-written text the SELECT is entirely generated by the app
    /// and nothing changes compared to before.
    fn check_table_query(&self, sql: &str, query: &TableQuery) -> Result<bool, Box<DbError>> {
        if query.where_sql.trim().is_empty() {
            return Ok(false);
        }
        let caps = self.capabilities();
        if alamo_core::splitter::split_statements(sql, caps.backslash_escapes).len() != 1 {
            return Err(Box::new(DbError::internal(
                "the WHERE filter must be a single condition: no `;` \
                 (for multiple statements use a SQL tab)",
            )));
        }
        self.check_read_only(sql)?;
        Ok(self.read_only && caps.read_only_txn)
    }

    fn check_read_only(&self, sql: &str) -> Result<(), Box<DbError>> {
        use alamo_core::classify::{
            begins_or_ends_txn, classify_statement, disarms_read_only, is_dynamic_sql,
            StatementClass,
        };
        if !self.read_only {
            return Ok(());
        }
        let caps = self.capabilities();
        for stmt in alamo_core::splitter::split_statements(sql, caps.backslash_escapes) {
            let info = classify_statement(&stmt.sql, caps.transactional_ddl);
            let snippet = || -> String { stmt.sql.trim().chars().take(60).collect() };
            if matches!(info.class, StatementClass::Write | StatementClass::Ddl) {
                return Err(Box::new(DbError::internal(format!(
                    "read-only profile: \"{}\" is a write",
                    snippet()
                ))));
            }
            if info.class == StatementClass::Control && disarms_read_only(&stmt.sql) {
                return Err(Box::new(DbError::internal(format!(
                    "read-only profile: \"{}\" would disable the session's read-only mode",
                    snippet()
                ))));
            }
            if caps.read_only_txn && begins_or_ends_txn(&stmt.sql) {
                return Err(Box::new(DbError::internal(format!(
                    "read-only profile: every execution already runs in a read-only \
                     transaction opened and closed by Alamo, \"{}\" is not allowed",
                    snippet()
                ))));
            }
            if !caps.read_only_txn && is_dynamic_sql(&stmt.sql) {
                return Err(Box::new(DbError::internal(format!(
                    "read-only profile: \"{}\" is dynamic SQL, which could \
                     disable read-only mode without being recognised",
                    snippet()
                ))));
            }
        }
        Ok(())
    }

    /// Estimate of the affected rows via `EXPLAIN` (§9.7), for the
    /// confirmation dialogs. Best-effort by contract: `None` for multi-statement
    /// batches, statements that cannot be explained or EXPLAIN errors — the
    /// confirmation stays usable without an estimate, never blocked by it. Runs
    /// on the introspection lane: the sticky stays free and its transaction
    /// (§9.7) is not touched. Runs BEFORE the confirmation: it must not execute anything.
    pub async fn explain_estimate(&self, sql: &str) -> Result<Option<u64>, Box<DbError>> {
        let caps = self.capabilities();
        let stmts = alamo_core::splitter::split_statements(sql, caps.backslash_escapes);
        let [stmt] = stmts.as_slice() else {
            return Ok(None);
        };
        // The dialog asks for the estimate BEFORE the confirmation: the statement
        // must start with a DML/read keyword, or `EXPLAIN <sql>` could
        // become an `EXPLAIN (ANALYZE) …` that really executes. The driver
        // then uses a single-statement protocol: if the splitter got it
        // wrong (dialects, comments), the server refuses instead of
        // executing the rest.
        if !alamo_core::classify::explainable(&stmt.sql) {
            return Ok(None);
        }
        let conn = self.introspect_session().await?;
        // MySQL: unqualified names of the user's SQL resolve in the
        // current database of the STICKY (last USE). The introspection
        // lane always qualifies its catalog queries: changing its
        // default is harmless. `current_database` however takes the mutex
        // of the sticky: if the sticky is streaming (another tab of the same
        // connection) the estimate would hang until the end of the query — better
        // no estimate right away than an estimate in minutes. And if the sync
        // fails, no fallback on the lane's STALE default: an
        // unqualified statement would resolve in the wrong database and the
        // estimate would lie — it degrades to None, as per contract.
        if caps.can_change_database_in_session {
            if self.registry.has_active_execution() {
                return Ok(None);
            }
            let Ok(db) = self.current_database().await else {
                return Ok(None);
            };
            if let Some(db) = db {
                if conn.use_database(&db).await.is_err() {
                    return Ok(None);
                }
            }
        }
        Ok(conn.explain_rows_estimate(&stmt.sql).await.unwrap_or(None))
    }

    /// Preview of the multi-cell batch (decision 12): same deterministic
    /// generation as `update_table_batch`, from the same parameters.
    pub fn update_sql_batch(
        &self,
        schema: &str,
        table: &str,
        updates: &[table::RowUpdate],
    ) -> Result<String, Box<DbError>> {
        self.write_sql_batch(schema, table, &Self::as_writes(updates))
    }

    /// Preview of the mixed batch (UPDATE of the cells + INSERT of the new
    /// rows): same deterministic generation as `write_table_batch`.
    pub fn write_sql_batch(
        &self,
        schema: &str,
        table: &str,
        writes: &[table::TableWrite],
    ) -> Result<String, Box<DbError>> {
        Self::validate_writes(writes)?;
        let dialect = self.sticky().dialect();
        Ok(Self::ordered_writes(writes)
            .map(|w| w.sql(dialect.as_ref(), schema, table))
            .collect::<Vec<_>>()
            .join(";\n"))
    }

    fn as_writes(updates: &[table::RowUpdate]) -> Vec<table::TableWrite> {
        updates.iter().cloned().map(Into::into).collect()
    }

    /// Execution order: DELETE, UPDATE, INSERT (see `TableWrite`).
    /// `applied` and the error index follow this order.
    pub fn ordered_writes(
        writes: &[table::TableWrite],
    ) -> impl Iterator<Item = &table::TableWrite> {
        let deletes = writes
            .iter()
            .filter(|w| matches!(w, table::TableWrite::Delete { .. }));
        let updates = writes
            .iter()
            .filter(|w| matches!(w, table::TableWrite::Update { .. }));
        let inserts = writes
            .iter()
            .filter(|w| matches!(w, table::TableWrite::Insert { .. }));
        deletes.chain(updates).chain(inserts)
    }

    /// Multi-cell/paste batch (decision 12): one `UPDATE` per row, by
    /// PK key. Blocking and without events — the dialog shows the outcome from
    /// the return value, no streaming needed for N targeted UPDATEs.
    ///
    /// Transactional semantics:
    /// - normal profile: shared lane inside explicit `BEGIN`…`COMMIT`
    ///   (the app's SQL, the no-rewrite rule §5.2 applies to the user's
    ///   SQL) — all or nothing, `ROLLBACK` on error;
    /// - manual commit (§9.7: prod or switch): sticky, inside the transaction
    ///   (implicit BEGIN if needed), EXPLICIT commit by the user; on error
    ///   the batch stops and the previous ones stay in the open transaction,
    ///   to be closed with Commit or Rollback in the toolbar.
    ///
    /// Statement errors (SQL or transport) end up in
    /// `BatchUpdateOut::error`; the `Err` of this function is only for
    /// upstream rejections (read-only, validation, busy lane) and for
    /// failures of the transactional frame on the shared lane.
    pub async fn update_table_batch(
        &self,
        schema: &str,
        table: &str,
        updates: &[table::RowUpdate],
    ) -> Result<BatchUpdateOut, Box<DbError>> {
        self.write_table_batch(schema, table, &Self::as_writes(updates), None)
            .await
    }

    /// Mixed batch of the table tab: UPDATE of the cells and INSERT of the
    /// new rows, in the order of `ordered_writes`, with the same transactional
    /// semantics as `update_table_batch`.
    ///
    /// With `append` the inserted rows are re-read in the batch itself
    /// (`RETURNING *`, or a SELECT by key / `LAST_INSERT_ID()`) and
    /// appended to the open result set, so the grid does not reload and does not
    /// lose scroll and selection (`BatchUpdateOut::appended`).
    pub async fn write_table_batch(
        &self,
        schema: &str,
        table: &str,
        writes: &[table::TableWrite],
        append: Option<&AppendTo>,
    ) -> Result<BatchUpdateOut, Box<DbError>> {
        if self.read_only {
            return Err(Box::new(DbError::internal(
                "read-only profile: editing is disabled (§9.7)",
            )));
        }
        Self::validate_writes(writes)?;
        let dialect = self.sticky().dialect();
        let live = match append {
            Some(a) => Some(self.registry.result(a.result_id).ok_or_else(|| {
                Box::new(DbError::internal("result set not found: reload the table"))
            })?),
            None => None,
        };
        let returning = dialect.capabilities().insert_returning;
        let items: Vec<BatchItem> = Self::ordered_writes(writes)
            .map(|w| {
                let sql = w.sql(dialect.as_ref(), schema, table);
                match (w, append) {
                    (table::TableWrite::Insert { .. }, Some(_)) if returning => BatchItem {
                        sql: format!("{sql} RETURNING *"),
                        fetch: Fetch::Returning,
                    },
                    (table::TableWrite::Insert { values }, Some(a)) => BatchItem {
                        sql,
                        fetch: match table::reread_inserted_sql(
                            dialect.as_ref(),
                            schema,
                            table,
                            values,
                            &a.key_columns,
                        ) {
                            Some(q) => Fetch::FollowUp(q),
                            None => Fetch::None,
                        },
                    },
                    _ => BatchItem {
                        sql,
                        fetch: Fetch::None,
                    },
                }
            })
            .collect();
        let mut collector = live.as_ref().map(|l| Collector {
            expected: l.schema(),
            chunks: Vec::new(),
            mismatch: false,
        });
        let mut out = self.run_batch(&items, &mut collector).await?;
        let (Some(live), Some(col), Some(a)) = (live, collector, append) else {
            return Ok(out);
        };
        // Only what remained written is aligned: on prod also the ones
        // that succeeded before the error (they stay in the open transaction).
        if !out.rolled_back {
            let schema = live.schema();
            let key_cols: Option<Vec<usize>> = a
                .key_columns
                .iter()
                .map(|c| schema.iter().position(|m| &m.name == c))
                .collect();
            let key_cols = key_cols.unwrap_or_default();
            // Inserted: appended, then moved where the grid showed them —
            // before the deletes leave the view, `at` counts them.
            if !col.mismatch {
                let start = live.phys_rows();
                for chunk in col.chunks {
                    let n = chunk.rows();
                    if live.append_local(chunk, &key_cols) {
                        out.appended += n;
                    }
                }
                if !a.at.is_empty() && a.at.len() == out.appended as usize {
                    live.place_rows(start..start + u64::from(out.appended), &a.at);
                }
            }
            // Deleted: out of the view, found again by key.
            let done = out.applied.len();
            let keys: Vec<Vec<Option<String>>> = Self::ordered_writes(writes)
                .take(done)
                .filter_map(|w| match w {
                    table::TableWrite::Delete { key } => Some(
                        a.key_columns
                            .iter()
                            .map(|c| {
                                key.iter()
                                    .find(|k| &k.column == c)
                                    .and_then(|k| k.value.clone())
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .collect();
            if !keys.is_empty() && !key_cols.is_empty() {
                out.removed = live.remove_rows(&key_cols, &keys);
            }
        }
        out.rows = Some(live.rows());
        Ok(out)
    }

    async fn run_batch(
        &self,
        stmts: &[BatchItem],
        col: &mut Option<Collector>,
    ) -> Result<BatchUpdateOut, Box<DbError>> {
        if self.manual_commit() {
            let exec_id = self
                .registry
                .try_new_execution(self.sticky().canceller())
                .ok_or_else(|| {
                    Box::new(DbError::internal(
                        "running on the main connection: wait for it to finish",
                    ))
                })?;
            let out = self.batch_on_sticky(stmts, col).await;
            let state = match &out {
                Ok(o) if o.error.is_none() => registry::ExecState::Completed,
                _ => registry::ExecState::Failed,
            };
            self.registry.set_exec_state(exec_id, state);
            out
        } else {
            if self.registry.has_active_on(registry::Lane::Shared) {
                return Err(Box::new(DbError::internal(
                    "the shared connection is busy: wait for the table to load",
                )));
            }
            let shared = self.shared_session().await?;
            let exec_id = self
                .registry
                .try_new_execution_on(registry::Lane::Shared, shared.canceller())
                .ok_or_else(|| {
                    Box::new(DbError::internal(
                        "the shared connection is busy: wait for the table to load",
                    ))
                })?;
            let out = Self::batch_atomic(&shared, stmts, col).await;
            let state = match &out {
                Ok(o) if o.error.is_none() => registry::ExecState::Completed,
                _ => registry::ExecState::Failed,
            };
            self.registry.set_exec_state(exec_id, state);
            out
        }
    }

    /// After a save from the grid: re-reads from the database the rows of
    /// `updates` (by PK key, the new one if the UPDATE changed it) and
    /// replaces them in the result set `result_id`, so the tab does not reload
    /// everything. True server values: formats, defaults, triggers.
    ///
    /// With manual commit (§9.7) the UPDATEs are in the sticky's open transaction:
    /// they are re-read there, a SELECT from the shared lane would see the old values.
    pub async fn refresh_table_rows(
        &self,
        schema: &str,
        table: &str,
        result_id: u64,
        updates: &[table::RowUpdate],
    ) -> Result<RefreshRowsOut, Box<DbError>> {
        let live = self
            .registry
            .result(result_id)
            .ok_or_else(|| Box::new(DbError::internal("result set not found: reload the table")))?;
        let expected = live.schema();
        let col_of = |name: &str| expected.iter().position(|c| c.name == name);
        let Some(first) = updates.first() else {
            return Ok(RefreshRowsOut {
                refreshed: 0,
                missing: 0,
            });
        };
        let key_cols: Option<Vec<usize>> = first.key.iter().map(|k| col_of(&k.column)).collect();
        let Some(key_cols) = key_cols else {
            return Err(Box::new(DbError::internal(
                "key column missing from the result: reload the table",
            )));
        };
        let values = |key: &[table::CellAssign]| -> Vec<Option<String>> {
            key.iter().map(|k| k.value.clone()).collect()
        };
        let old_keys: Vec<_> = updates.iter().map(|u| values(&u.key)).collect();
        // PK set to DEFAULT: the new value is chosen by the server and it is not
        // known which key to re-read with — that row stays "missing" and the UI
        // reloads the table.
        let new_keys: Vec<_> = updates.iter().map(table::key_after).collect();
        let unknown: Vec<bool> = new_keys
            .iter()
            .map(|k| k.iter().any(|a| a.default))
            .collect();
        let query_keys: Vec<Vec<table::CellAssign>> = new_keys
            .iter()
            .zip(&unknown)
            .filter(|(_, u)| !**u)
            .map(|(k, _)| k.clone())
            .collect();
        let targets = live.find_rows(&key_cols, &old_keys);

        let busy = || Box::new(DbError::internal("connection busy: reload the table"));
        let (conn, exec_id) = if self.manual_commit() {
            let id = self
                .registry
                .try_new_execution(self.sticky().canceller())
                .ok_or_else(busy)?;
            (self.sticky(), id)
        } else {
            if self.registry.has_active_on(registry::Lane::Shared) {
                return Err(busy());
            }
            let shared = self.shared_session().await?;
            let id = self
                .registry
                .try_new_execution_on(registry::Lane::Shared, shared.canceller())
                .ok_or_else(busy)?;
            (shared, id)
        };
        let dialect = self.sticky().dialect();
        let mut reread = alamo_core::result_set::ResultSet::new(Arc::clone(&expected));
        let mut mismatch = false;
        let mut error = None;
        for keys in query_keys.chunks(REFRESH_KEYS_PER_QUERY) {
            let sql = table::rows_by_key_sql(dialect.as_ref(), schema, table, keys);
            let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
            let outcome = conn
                .execute_streaming(&sql, &FetchOptions::default(), &mut rx, &mut |ev| {
                    use alamo_core::stream::StreamEvent;
                    match ev {
                        StreamEvent::Schema { schema, .. } => {
                            mismatch |= schema.len() != expected.len()
                                || schema.iter().zip(expected.iter()).any(|(a, b)| {
                                    a.name != b.name || a.type_display != b.type_display
                                });
                        }
                        StreamEvent::Chunk { chunk, .. } => {
                            if !mismatch {
                                reread.push_chunk(chunk);
                            }
                        }
                        StreamEvent::BudgetReached { .. } => {}
                    }
                })
                .await
                .map_err(|e| *e);
            match outcome {
                Err(e) => error = Some(e),
                Ok(out) => error = out.error,
            }
            if error.is_some() || mismatch {
                break;
            }
        }
        let failed = error.is_some() || mismatch;
        self.registry.set_exec_state(
            exec_id,
            if failed {
                registry::ExecState::Failed
            } else {
                registry::ExecState::Completed
            },
        );
        if let Some(e) = error {
            return Err(Box::new(e));
        }
        if mismatch {
            return Err(Box::new(DbError::internal(
                "the table structure has changed: reload the table",
            )));
        }

        // Row re-read by (new) key, then replacement on the physical row
        // found with the old key.
        let n_cols = expected.len();
        let mut by_key = std::collections::HashMap::new();
        for row in 0..reread.rows() {
            let full: Vec<Option<String>> = (0..n_cols).map(|c| reread.cell_full(row, c)).collect();
            let key: Vec<Option<String>> = key_cols.iter().map(|&c| full[c].clone()).collect();
            by_key.entry(key).or_insert(full);
        }
        let mut out = RefreshRowsOut {
            refreshed: 0,
            missing: 0,
        };
        for (i, target) in targets.iter().enumerate() {
            let fresh = if unknown[i] {
                None
            } else {
                by_key.get(&values(&new_keys[i]))
            };
            match (target, fresh) {
                (Some(phys), Some(full)) => {
                    live.patch_row(*phys, full.clone());
                    out.refreshed += 1;
                }
                _ => out.missing += 1,
            }
        }
        Ok(out)
    }

    /// Batch on the shared lane, atomic: `BEGIN` → UPDATE… → `COMMIT`, on a
    /// statement error `ROLLBACK` and the error is reported with its index.
    async fn batch_atomic(
        conn: &DbConn,
        stmts: &[BatchItem],
        col: &mut Option<Collector>,
    ) -> Result<BatchUpdateOut, Box<DbError>> {
        Self::batch_frame_stmt(conn, "BEGIN").await?;
        let mut applied = Vec::with_capacity(stmts.len());
        for (i, item) in stmts.iter().enumerate() {
            let outcome = Self::run_item(conn, item, col)
                .await
                .and_then(|(out, got)| match out.error {
                    Some(e) => Err(Box::new(e)),
                    None => Ok(out.rows_affected.unwrap_or(got)),
                });
            match outcome {
                Ok(rows) => applied.push(rows),
                Err(e) => {
                    // Best-effort rollback: if it fails too (dead
                    // connection) the primary error stays the statement's;
                    // the lazy lane reopens healthy at the next use.
                    let _ = Self::batch_frame_stmt(conn, "ROLLBACK").await;
                    return Ok(BatchUpdateOut {
                        applied,
                        error_index: Some(i as u32),
                        error: Some(events::ErrorOut::from(&*e)),
                        in_transaction: false,
                        rolled_back: true,
                        appended: 0,
                        removed: 0,
                        rows: None,
                    });
                }
            }
        }
        Self::batch_frame_stmt(conn, "COMMIT").await?;
        Ok(BatchUpdateOut {
            applied,
            error_index: None,
            error: None,
            in_transaction: false,
            rolled_back: false,
            appended: 0,
            removed: 0,
            rows: None,
        })
    }

    /// Batch on the sticky with manual commit (§9.7): implicit BEGIN if
    /// there is no open transaction, per-statement tracking, NEVER commit —
    /// the user closes it. On error the previous ones stay in the transaction.
    async fn batch_on_sticky(
        &self,
        stmts: &[BatchItem],
        col: &mut Option<Collector>,
    ) -> Result<BatchUpdateOut, Box<DbError>> {
        if !self.txn.snapshot().open {
            match Self::batch_stmt_outcome(&self.sticky(), "BEGIN").await {
                Ok(out) => self
                    .txn
                    .apply_statement("BEGIN", true, out.in_transaction, None),
                Err(e) => {
                    // Like exec.rs: never execute outside a transaction what
                    // the user expects to be protected.
                    return Ok(BatchUpdateOut {
                        applied: Vec::new(),
                        error_index: None,
                        error: Some(events::ErrorOut::from(&*e)),
                        in_transaction: self.txn.snapshot().open,
                        rolled_back: false,
                        appended: 0,
                        removed: 0,
                        rows: None,
                    });
                }
            }
        }
        let mut applied = Vec::with_capacity(stmts.len());
        for (i, item) in stmts.iter().enumerate() {
            let sql = &item.sql;
            match Self::run_item(&self.sticky(), item, col).await {
                Ok((out, got)) if out.error.is_none() => {
                    let rows = out.rows_affected.unwrap_or(got);
                    self.txn
                        .apply_statement(sql, true, out.in_transaction, Some(rows));
                    applied.push(rows);
                }
                Ok((out, _)) => {
                    self.txn
                        .apply_statement(sql, false, out.in_transaction, None);
                    let e = out.error.expect("error branch");
                    return Ok(BatchUpdateOut {
                        applied,
                        error_index: Some(i as u32),
                        error: Some(events::ErrorOut::from(&e)),
                        in_transaction: self.txn.snapshot().open,
                        rolled_back: false,
                        appended: 0,
                        removed: 0,
                        rows: None,
                    });
                }
                Err(e) => {
                    self.txn.apply_statement(sql, false, None, None);
                    return Ok(BatchUpdateOut {
                        applied,
                        error_index: Some(i as u32),
                        error: Some(events::ErrorOut::from(&*e)),
                        in_transaction: self.txn.snapshot().open,
                        rolled_back: false,
                        appended: 0,
                        removed: 0,
                        rows: None,
                    });
                }
            }
        }
        Ok(BatchUpdateOut {
            applied,
            error_index: None,
            error: None,
            in_transaction: self.txn.snapshot().open,
            rolled_back: false,
            appended: 0,
            removed: 0,
            rows: None,
        })
    }

    /// A batch statement, with the re-read of the inserted row if
    /// requested. Returns the outcome and the rows re-read by this statement
    /// (0 if nothing to re-read). An error of the re-read SELECT is not
    /// an error of the batch: the INSERT went through, the row simply
    /// is not appended and the UI reloads.
    async fn run_item(
        conn: &DbConn,
        item: &BatchItem,
        col: &mut Option<Collector>,
    ) -> Result<(alamo_core::stream::StreamOutcome, u64), Box<DbError>> {
        match &item.fetch {
            Fetch::None => Ok((Self::batch_stmt_outcome(conn, &item.sql).await?, 0)),
            Fetch::Returning => Self::fetch_stmt_outcome(conn, &item.sql, col).await,
            Fetch::FollowUp(select) => {
                let out = Self::batch_stmt_outcome(conn, &item.sql).await?;
                if out.error.is_none() {
                    let _ = Self::fetch_stmt_outcome(conn, select, col).await;
                }
                Ok((out, 0))
            }
        }
    }

    /// Statement that returns rows (RETURNING / re-read SELECT):
    /// the chunks end up in the collector if the schema is that of the open
    /// result set, otherwise the collector marks itself `mismatch`.
    async fn fetch_stmt_outcome(
        conn: &DbConn,
        sql: &str,
        col: &mut Option<Collector>,
    ) -> Result<(alamo_core::stream::StreamOutcome, u64), Box<DbError>> {
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let expected = col.as_ref().map(|c| Arc::clone(&c.expected));
        let mut got: Vec<alamo_core::chunk::Chunk> = Vec::new();
        let mut mismatch = false;
        let out = conn
            .execute_streaming(sql, &FetchOptions::default(), &mut rx, &mut |ev| {
                use alamo_core::stream::StreamEvent;
                match ev {
                    StreamEvent::Schema { schema, .. } => {
                        if let Some(exp) = &expected {
                            mismatch |= schema.len() != exp.len()
                                || schema.iter().zip(exp.iter()).any(|(a, b)| {
                                    a.name != b.name || a.type_display != b.type_display
                                });
                        }
                    }
                    StreamEvent::Chunk { chunk, .. } => {
                        if !mismatch {
                            got.push(chunk);
                        }
                    }
                    StreamEvent::BudgetReached { .. } => {}
                }
            })
            .await
            .map_err(|e| *e)
            .map_err(Box::new)?;
        let rows: u64 = got.iter().map(|c| c.rows() as u64).sum();
        if let Some(c) = col {
            if mismatch {
                c.mismatch = true;
            } else {
                c.chunks.extend(got);
            }
        }
        Ok((out, rows))
    }

    /// A batch statement: executes and returns the affected rows.
    /// The SQL error inside the outcome becomes `Err` — the batch only cares
    /// about succeeded/failed.
    async fn batch_stmt(conn: &DbConn, sql: &str) -> Result<u64, Box<DbError>> {
        let out = Self::batch_stmt_outcome(conn, sql).await?;
        match out.error {
            Some(e) => Err(Box::new(e)),
            None => Ok(out.rows_affected.unwrap_or(0)),
        }
    }

    /// Statement of the transactional frame (BEGIN/COMMIT/ROLLBACK) on the
    /// shared lane: here the SQL error too is a hard error of the batch.
    async fn batch_frame_stmt(conn: &DbConn, sql: &str) -> Result<(), Box<DbError>> {
        Self::batch_stmt(conn, sql).await.map(|_| ())
    }

    async fn batch_stmt_outcome(
        conn: &DbConn,
        sql: &str,
    ) -> Result<alamo_core::stream::StreamOutcome, Box<DbError>> {
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        conn.execute_streaming(sql, &FetchOptions::default(), &mut rx, &mut |_| {})
            .await
            .map_err(|e| *e)
            .map_err(Box::new)
    }

    fn validate_writes(writes: &[table::TableWrite]) -> Result<(), Box<DbError>> {
        if writes.is_empty() {
            return Err(Box::new(DbError::internal("empty write batch")));
        }
        for w in writes {
            match w {
                table::TableWrite::Update { set, key } => Self::validate_update(set, key)?,
                table::TableWrite::Insert { values } => Self::validate_insert(values)?,
                table::TableWrite::Delete { key } => Self::validate_key(key, "DELETE")?,
            }
        }
        Ok(())
    }

    /// A column twice in the same INSERT is an app error, not
    /// an SQL error to show to the user.
    fn validate_insert(values: &[table::CellAssign]) -> Result<(), Box<DbError>> {
        let mut seen = std::collections::HashSet::new();
        for a in values {
            if !seen.insert(a.column.as_str()) {
                return Err(Box::new(DbError::internal(format!(
                    "INSERT with column '{}' repeated",
                    a.column
                ))));
            }
        }
        Ok(())
    }

    /// An `UPDATE` without `SET` makes no sense; without `WHERE` it is a mass
    /// write that cell editing must never be able to produce (§9.7).
    fn validate_update(
        set: &[table::CellAssign],
        key: &[table::CellAssign],
    ) -> Result<(), Box<DbError>> {
        if set.is_empty() {
            return Err(Box::new(DbError::internal("UPDATE without assignments")));
        }
        Self::validate_key(key, "UPDATE")
    }

    /// PK key of an UPDATE/DELETE: without it, it would be a mass write
    /// that the grid must never be able to produce (§9.7).
    fn validate_key(key: &[table::CellAssign], stmt: &str) -> Result<(), Box<DbError>> {
        if key.is_empty() {
            return Err(Box::new(DbError::internal(format!(
                "{stmt} without a key: the grid is read-only without a PK (decision 6)"
            ))));
        }
        if key.iter().any(|k| k.default) {
            return Err(Box::new(DbError::internal(format!(
                "DEFAULT not allowed in the {stmt} key"
            ))));
        }
        Ok(())
    }

    /// Current database of the **sticky** (context shown in the UI): on MySQL
    /// it reflects the user's last `USE`. To be called with the sticky free — on
    /// MySQL the lock would wait for the end of the running execution.
    pub async fn current_database(&self) -> Result<Option<String>, Box<DbError>> {
        self.sticky().current_database().await
    }

    /// `USE` on the **sticky** (MySQL only): the database select in the toolbar
    /// goes through here. It only touches the context of the user's SQL — table
    /// tabs and introspection always qualify. To be called with the sticky
    /// free.
    pub async fn use_database(&self, db: &str) -> Result<(), Box<DbError>> {
        self.sticky().use_database(db).await
    }

    pub fn cancel(&self, exec_id: u64) {
        self.registry.cancel(exec_id);
    }

    /// Escalation of §7.2: `pg_terminate_backend` / `KILL <id>` on a
    /// new connection. Destructive — afterwards, this session must be thrown away and
    /// reconnected. `false` = the server refused.
    pub async fn terminate(&self) -> Result<bool, Box<DbError>> {
        let killed = self.sticky().terminate().await?;
        if killed {
            // The server-side session is dead: any transaction of the
            // sticky has been aborted by the server, and the UI must not
            // keep showing it open (§9.7). If the server refuses, the
            // connection is still alive and the state stays as it is.
            self.txn.reset();
        }
        Ok(killed)
    }

    pub fn budget_resume(&self, exec_id: u64) {
        // The result set goes back to `Streaming` at the next chunk; the Paused
        // state lives only between BudgetReached and the user's answer.
        self.registry.budget_resume(exec_id);
    }

    pub fn budget_stop(&self, exec_id: u64) {
        self.registry.budget_stop(exec_id);
    }

    /// Next page of a query paused on rows (scroll to the bottom).
    pub fn budget_more(&self, exec_id: u64) {
        self.registry.budget_more(exec_id);
    }

    pub fn result(&self, result_id: u64) -> Option<LiveResult> {
        self.registry.result(result_id)
    }

    pub fn release_result(&self, result_id: u64) {
        self.registry.release_result(result_id);
    }
}
