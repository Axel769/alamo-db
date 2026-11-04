//! Driver registry (§2.2): the session talks to **one connection**, whatever
//! the driver behind it. `Connection` is what a lane can do, `Driver` opens
//! them, `Drivers` lists the ones a build ships: PostgreSQL and
//! MySQL/MariaDB here, more from crates registered by the app (open core,
//! §13).
//!
//! Goal of §2.1 applied to the backend: zero `if dialect == ...` scattered
//! through the session — the differences live in the implementations.
//!
//! No default methods on purpose: a new driver that forgot read-only
//! (`set_session_read_only`) or the catalog `search_path` would run without
//! the protection and nobody would notice. The compiler lists every method.

mod mysql;
mod pg;

use std::any::Any;
use std::sync::Arc;

use alamo_core::dialect::{Capabilities, Dialect};
use alamo_core::error::DbError;
use alamo_core::schema_info::{
    ColumnInfo, ConnectionBudget, ForeignKey, RelationInfo, SchemaInfo, SchemaStats,
};
use alamo_core::stream::{FetchOptions, StreamControl, StreamEvent, StreamOutcome};
use alamo_core::tls::{TlsInfo, TlsMode, TlsOptions};
use futures_util::future::BoxFuture;

pub(crate) use pg::PgConn;

/// A connection as the server knows it (§3.2): the id is meaningful only
/// within one life of one server, which `server` and `start` identify. The
/// app keeps the ids of a closed session to close them at the next connect
/// (`Session::kill_abandoned`), so they are serializable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerConnId {
    /// Which server (`@@server_uuid` on MySQL, `@@hostname` on MariaDB).
    pub server: String,
    /// Server start instant, Unix seconds (from the server's `Uptime`).
    pub start: i64,
    pub id: u32,
}

/// Cap of a verification ping: beyond it, the connection is given up as dead.
const PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Result of a driver call: the error is boxed, like everywhere in the session.
pub type DbResult<T> = Result<T, Box<DbError>>;

/// A connection to the server, shared: the execution tasks carry a clone.
pub type DbConn = Arc<dyn Connection>;

/// What the app knows of an engine before connecting: the connection
/// dialog and the import check are built from it, not from a list of names.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    /// Stable identifier: the `dialect` saved in the connection profiles
    /// (`postgres`, `mysql`).
    pub id: &'static str,
    /// Name in the connection dialog ("MySQL / MariaDB").
    pub name: &'static str,
    /// Name next to a host in the lists ("MySQL").
    pub short_name: &'static str,
    pub default_port: u16,
    pub default_user: &'static str,
    /// PG: every connection is born inside a database. MySQL connects
    /// without a default schema too.
    pub database_required: bool,
    /// The `sslmode` values the driver honours (MySQL has no `verify-ca`).
    pub tls_modes: &'static [TlsMode],
    /// Syntax the UI's editors highlight and quote with (`postgres`,
    /// `mysql`): an engine with a known syntax reuses it.
    pub sql_syntax: &'static str,
    /// The dialect's, before any connection: the entries that depend on the
    /// connection (`can_cancel`) are only known on a session.
    pub capabilities: Capabilities,
}

/// Where to connect, from a profile: the app knows the profiles, the driver
/// how its engine wants them written.
#[derive(Debug, Clone, Copy)]
pub struct ConnectTarget<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    /// Empty = none (trust, keychain entry missing).
    pub password: &'a str,
    /// Empty = none (only where `EngineInfo::database_required` is false).
    pub database: &'a str,
    pub tls: TlsMode,
    /// SSH tunnel (§12): the TCP goes to this port on 127.0.0.1, while the
    /// TLS certificate is still verified against `host`.
    pub tunnel_port: Option<u16>,
}

/// Opens connections of one engine.
pub trait Driver: Send + Sync {
    fn info(&self) -> EngineInfo;

    /// The string `connect` receives for `target`.
    fn conn_string(&self, target: &ConnectTarget<'_>) -> String;

    /// A connection with `tls` applied (§12), nothing else: the session
    /// applies the profile's regime (read-only, prod) on top.
    fn connect<'a>(&'a self, url: &'a str, tls: &'a TlsOptions) -> BoxFuture<'a, DbResult<DbConn>>;
}

/// The drivers of a build. The app starts from `builtin` and the official
/// Pro build registers its own on top; the session receives the one chosen
/// by the profile.
#[derive(Clone)]
pub struct Drivers(Vec<Arc<dyn Driver>>);

impl Drivers {
    pub fn builtin() -> Drivers {
        Drivers(vec![Arc::new(pg::PgDriver), Arc::new(mysql::MyDriver)])
    }

    /// Adds `driver`, replacing the one with the same id.
    pub fn register(&mut self, driver: Arc<dyn Driver>) {
        let id = driver.info().id;
        self.0.retain(|d| d.info().id != id);
        self.0.push(driver);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Driver>> {
        self.0.iter().find(|d| d.info().id == id).cloned()
    }

    /// The engines of the build, in registration order.
    pub fn engines(&self) -> Vec<EngineInfo> {
        self.0.iter().map(|d| d.info()).collect()
    }
}

/// Built-in driver for a bare connection string (bench, smoke, tests, where
/// there is no profile): `mysql://` is unambiguous, everything else
/// (`postgres://` URL or libpq key=value) goes to PostgreSQL.
pub fn builtin_for_url(url: &str) -> Arc<dyn Driver> {
    if url.trim_start().starts_with("mysql://") {
        Arc::new(mysql::MyDriver)
    } else {
        Arc::new(pg::PgDriver)
    }
}

/// Out-of-band cancellation (§7): lives in the registry, where the busy
/// connection never reaches — which is why it is an object of its own, built
/// at connect time with everything it needs.
pub trait Canceller: Send + Sync {
    /// Best-effort by definition: it may arrive after the query has ended and
    /// behind poolers/managed services it may not work at all (§7.1).
    /// `false` = the cancel request was not delivered to the server:
    /// advisory outcome, the fallback stays the escalation of §7.2.
    fn cancel(&self) -> BoxFuture<'static, bool>;
}

/// What a lane can do. Methods that only make sense on some engines say so;
/// the others implement them as a no-op or an error, never by omission.
pub trait Connection: Send + Sync + 'static {
    /// For the M0 bench, which needs the concrete type.
    fn as_any(&self) -> &dyn Any;

    /// The dialect of the connection (§2.1).
    fn dialect(&self) -> Arc<dyn Dialect>;

    fn canceller(&self) -> Arc<dyn Canceller>;

    /// For the lazy reopening of the auxiliary connections (§3), with a time
    /// cap: on a dead network it must not hang the UI. A connection busy
    /// with another call counts as alive.
    fn healthy(&self) -> BoxFuture<'_, bool>;

    /// Without a roundtrip. `None` = the driver cannot know without using
    /// the connection: `ping` is needed.
    fn healthy_cheap(&self) -> Option<bool>;

    /// Real check, with a roundtrip and a time cap: `false` = dead or
    /// unreachable.
    fn ping(&self) -> BoxFuture<'_, bool>;

    fn execute_streaming<'a>(
        &'a self,
        sql: &'a str,
        opts: &'a FetchOptions,
        control: &'a mut tokio::sync::mpsc::Receiver<StreamControl>,
        on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
    ) -> BoxFuture<'a, DbResult<StreamOutcome>>;

    /// Estimate of the affected rows via `EXPLAIN` (§9.7): free, never a
    /// `COUNT(*)` (§5.2). `None` = the plan exposes no estimates.
    ///
    /// Contract: `sql` is ONE SINGLE statement starting with a DML keyword
    /// (`Session::explain_estimate` checks it with `classify::explainable`),
    /// sent with a single-statement protocol: a batch that slipped past the
    /// splitter fails instead of being executed.
    fn explain_rows_estimate<'a>(&'a self, sql: &'a str) -> BoxFuture<'a, DbResult<Option<u64>>>;

    /// Server-side read-only (§9.7 point 4): writes are refused by the
    /// server, a net even for statements the classifier cannot read.
    fn set_session_read_only(&self) -> BoxFuture<'_, DbResult<()>>;

    /// What protects the channel, measured on the connection (§12.2):
    /// `None` = plaintext, after a possible downgrade of `prefer`. Fields the
    /// engine cannot give stay `None` inside.
    fn tls_info(&self) -> BoxFuture<'_, DbResult<Option<TlsInfo>>>;

    /// READ ONLY transaction around a read-only batch, called only when
    /// `Capabilities::read_only_txn` is on.
    fn begin_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>>;

    fn end_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>>;

    /// The user's connection limit and how many it already has open, this
    /// one included (§3.2). `None` = no limit, or not readable.
    fn connection_budget(&self) -> BoxFuture<'_, DbResult<Option<ConnectionBudget>>>;

    /// The connection as the server knows it, for `kill_abandoned`. `None`
    /// where the server notices dead connections by itself, or the identity
    /// cannot be read.
    fn server_conn_id(&self) -> Option<ServerConnId>;

    /// Closes on the server the connections `old` of the same profile,
    /// abandoned because they were dead for us — not necessarily for the
    /// server, which may keep counting them against the user's limit (after
    /// a sleep, until `wait_timeout`). Only those of the same server life
    /// that the server shows idle and of the same user. Best-effort.
    fn kill_abandoned<'a>(&'a self, old: &'a [ServerConnId]) -> BoxFuture<'a, ()>;

    /// Closes right away a connection just opened and no longer needed, so
    /// its slot frees up without waiting for the server to notice. Untouched
    /// if someone else still holds a clone.
    fn close(self: Arc<Self>) -> BoxFuture<'static, ()>;

    /// Catalog lane: unqualified functions and operators of the
    /// introspection queries must resolve only to the built-ins
    /// (CVE-2018-1058). No-op where they cannot be shadowed.
    fn pin_catalog_search_path(&self) -> BoxFuture<'_, DbResult<()>>;

    /// Server-side timeout on the idle transaction (§9.7.2). Best-effort:
    /// the caller ignores the error.
    fn set_idle_txn_timeout(&self, secs: u32) -> BoxFuture<'_, DbResult<()>>;

    /// Asynchronous notices received so far (§6).
    fn drain_async_notices(&self) -> Vec<DbError>;

    /// Warnings at the end of a statement (§11). To be called **after a
    /// successful statement**: after an error the server's warning queue
    /// holds the error itself, which the UI has already shown.
    fn post_statement_warnings(&self) -> BoxFuture<'_, Vec<DbError>>;

    /// Escalation of §7.2: kills the session server-side, without waiting
    /// for the connection (this is the point where it is certainly busy).
    fn terminate(&self) -> BoxFuture<'_, DbResult<bool>>;

    /// Changes the default database of the connection, where
    /// `Capabilities::can_change_database_in_session`; an error elsewhere.
    fn use_database<'a>(&'a self, db: &'a str) -> BoxFuture<'a, DbResult<()>>;

    /// Current database of the connection, for the context in the UI.
    fn current_database(&self) -> BoxFuture<'_, DbResult<Option<String>>>;

    fn schemas(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>>;

    /// Databases of the server (on MySQL/MariaDB they are the schemas).
    fn databases(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>>;

    fn schema_stats(&self) -> BoxFuture<'_, DbResult<Vec<SchemaStats>>>;

    fn relations<'a>(&'a self, schema: &'a str) -> BoxFuture<'a, DbResult<Vec<RelationInfo>>>;

    fn relations_all(&self) -> BoxFuture<'_, DbResult<Vec<RelationInfo>>>;

    fn columns<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, DbResult<Vec<ColumnInfo>>>;

    fn foreign_keys<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, DbResult<Vec<ForeignKey>>>;

    /// Flat catalog for autocomplete: (schema, table, column).
    fn autocomplete_catalog<'a>(
        &'a self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, DbResult<Vec<(String, String, String)>>>;

    fn table_ddl<'a>(&'a self, schema: &'a str, table: &'a str) -> BoxFuture<'a, DbResult<String>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_driver_postgres::PgDialect;

    struct Fake(&'static str);

    impl Driver for Fake {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                id: self.0,
                name: self.0,
                short_name: self.0,
                default_port: 1,
                default_user: "",
                database_required: false,
                tls_modes: &[],
                sql_syntax: self.0,
                capabilities: PgDialect::default().capabilities(),
            }
        }
        fn conn_string(&self, _: &ConnectTarget<'_>) -> String {
            String::new()
        }
        fn connect<'a>(&'a self, _: &'a str, _: &'a TlsOptions) -> BoxFuture<'a, DbResult<DbConn>> {
            Box::pin(async { Err(Box::new(DbError::internal("fake"))) })
        }
    }

    #[test]
    fn builtin_has_both_engines() {
        let d = Drivers::builtin();
        assert_eq!(d.get("postgres").unwrap().info().id, "postgres");
        assert_eq!(d.get("mysql").unwrap().info().id, "mysql");
        assert!(d.get("oracle").is_none());
        let ids: Vec<_> = d.engines().iter().map(|e| e.id).collect();
        assert_eq!(ids, ["postgres", "mysql"]);
        // The two differences the UI reads from here (§2.1).
        let my = d.get("mysql").unwrap().info().capabilities;
        let pg = d.get("postgres").unwrap().info().capabilities;
        assert!(my.can_change_database_in_session && !pg.can_change_database_in_session);
        assert!(!my.transactional_ddl && pg.transactional_ddl);
    }

    #[test]
    fn register_adds_and_replaces_by_id() {
        let mut d = Drivers::builtin();
        d.register(Arc::new(Fake("oracle")));
        assert!(d.get("oracle").is_some());
        d.register(Arc::new(Fake("mysql")));
        assert_eq!(d.0.iter().filter(|x| x.info().id == "mysql").count(), 1);
        assert_eq!(d.0.len(), 3);
    }

    #[test]
    fn bare_url_routing() {
        assert_eq!(builtin_for_url(" mysql://u@h/db").info().id, "mysql");
        assert_eq!(builtin_for_url("postgres://u@h/db").info().id, "postgres");
        assert_eq!(builtin_for_url("host=h user=u").info().id, "postgres");
    }
}
