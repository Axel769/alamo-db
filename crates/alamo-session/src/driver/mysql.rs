//! MySQL and MariaDB driver (§2.2), on `alamo-driver-mysql`.

use std::any::Any;
use std::sync::Arc;

use alamo_core::dialect::Dialect;
use alamo_core::error::DbError;
use alamo_core::schema_info::{
    ColumnInfo, ConnectionBudget, ForeignKey, RelationInfo, SchemaInfo, SchemaStats,
};
use alamo_core::stream::{FetchOptions, StreamControl, StreamEvent, StreamOutcome};
use alamo_core::tls::{CertSummary, TlsInfo, TlsMode, TlsOptions};
use alamo_driver_mysql::introspect;
use alamo_driver_mysql::{MyDialect, MyError, MySession};
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use tokio::sync::{Mutex, MutexGuard};

use super::{
    Canceller, ConnectTarget, Connection, DbConn, DbResult, Driver, EngineInfo, ServerConnId,
    PING_TIMEOUT,
};

pub(super) struct MyDriver;

impl Driver for MyDriver {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "mysql",
            name: "MySQL / MariaDB",
            short_name: "MySQL",
            default_port: 3306,
            default_user: "root",
            database_required: false,
            // verify-ca is refused by `alamo-driver-mysql` (see `ssl_opts`).
            tls_modes: &[
                TlsMode::Prefer,
                TlsMode::Disable,
                TlsMode::Require,
                TlsMode::VerifyFull,
            ],
            sql_syntax: "mysql",
            // MariaDB is recognised at the handshake; the capabilities are the same.
            capabilities: MyDialect::default().capabilities(),
        }
    }

    /// `mysql://` URL: percent-encoding is mandatory, a password with `@`
    /// or `/` would break the parsing. Through a tunnel the URL points to
    /// the listener and the real name travels in
    /// `TlsOptions::tls_server_name`.
    fn conn_string(&self, t: &ConnectTarget<'_>) -> String {
        let mut s = format!("mysql://{}", percent_encode(t.user));
        if !t.password.is_empty() {
            s.push(':');
            s.push_str(&percent_encode(t.password));
        }
        let (host, port) = match t.tunnel_port {
            Some(lp) => ("127.0.0.1", lp),
            None => (t.host, t.port),
        };
        // IPv6 host: a literal with `:` (e.g. `::1`) goes in square brackets
        // or the URL becomes invalid.
        if host.contains(':') {
            s.push_str(&format!("@[{host}]:{port}"));
        } else {
            s.push_str(&format!("@{host}:{port}"));
        }
        // Optional database: without a default schema the URL path stays
        // empty and it is chosen later with USE.
        if !t.database.trim().is_empty() {
            s.push('/');
            s.push_str(&percent_encode(t.database));
        }
        s
    }

    fn connect<'a>(&'a self, url: &'a str, tls: &'a TlsOptions) -> BoxFuture<'a, DbResult<DbConn>> {
        async move {
            let sess = MySession::connect(url, tls).await.map_err(boxed)?;
            let (url, id) = sess.cancel_parts();
            let server_conn = match (&sess.server_identity, sess.server_start) {
                (Some(server), Some(start)) => Some(ServerConnId {
                    server: server.clone(),
                    start,
                    id,
                }),
                _ => None,
            };
            Ok(Arc::new(MyConn {
                url: url.into(),
                id,
                mariadb: sess.is_mariadb,
                tls: sess.tls_options(),
                server_conn,
                sess: Mutex::new(sess),
            }) as DbConn)
        }
        .boxed()
    }
}

fn boxed(e: MyError) -> Box<DbError> {
    Box::new(e.into_db_error())
}

/// Url and connection id stay **outside** the mutex: KILL and terminate are
/// needed precisely when the connection is busy and the lock would never be
/// acquired.
struct MyConn {
    sess: Mutex<MySession>,
    url: Arc<str>,
    id: u32,
    mariadb: bool,
    /// Effective TLS regime (after the possible downgrade of `prefer`):
    /// outside the mutex like url and id — the KILL is needed when the
    /// lock is busy (§7.1).
    tls: TlsOptions,
    /// Identity on the server, read at connect (see `kill_abandoned`).
    server_conn: Option<ServerConnId>,
}

/// `old` may be killed from `fresh`: another connection, on the same server
/// (same identity) in the same life (same start time, with a few seconds of
/// tolerance for latency). After a restart the ids start over and the old
/// one may belong to another live connection of the same user (HeidiSQL).
fn killable(fresh: &ServerConnId, old: &ServerConnId) -> bool {
    const SAME_START_TOLERANCE_SECS: i64 = 5;
    fresh.id != old.id
        && fresh.server == old.server
        && (fresh.start - old.start).abs() <= SAME_START_TOLERANCE_SECS
}

struct MyCancel {
    url: Arc<str>,
    id: u32,
    /// Effective TLS regime of the target session (§12), for the connection
    /// that fires the `KILL`.
    tls: TlsOptions,
}

impl Canceller for MyCancel {
    fn cancel(&self) -> BoxFuture<'static, bool> {
        let (url, id, tls) = (Arc::clone(&self.url), self.id, self.tls.clone());
        async move {
            alamo_driver_mysql::kill(&url, id, false, &tls)
                .await
                .unwrap_or(false)
        }
        .boxed()
    }
}

impl MyConn {
    fn my_dialect(&self) -> MyDialect {
        MyDialect {
            mariadb: self.mariadb,
            ..MyDialect::default()
        }
    }

    /// The connection for a command, refused if it is out of phase
    /// (`MySession::in_sync`): a ping interrupted by its cap leaves it with a
    /// response still to be read, and if the reopening fails (network not
    /// yet back after the PC went to sleep) it stays in use.
    async fn lock(&self) -> DbResult<MutexGuard<'_, MySession>> {
        let g = self.sess.lock().await;
        g.in_sync().map_err(boxed)?;
        Ok(g)
    }
}

impl Connection for MyConn {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        Arc::new(self.my_dialect())
    }

    fn canceller(&self) -> Arc<dyn Canceller> {
        Arc::new(MyCancel {
            url: Arc::clone(&self.url),
            id: self.id,
            tls: self.tls.clone(),
        })
    }

    /// The ping costs a roundtrip. Busy (another call holds the mutex) = in
    /// use, hence alive: no ping. Waiting for the lock made the cap expire
    /// behind a slow catalog query (`schema_stats`, autocomplete) and
    /// discarded a healthy connection — 5 s lost plus a reopening via
    /// SSH/TLS before being able to list the tables. If it were really
    /// dead, the call in progress fails and the next one reopens.
    fn healthy(&self) -> BoxFuture<'_, bool> {
        async move {
            match self.sess.try_lock() {
                Ok(mut s) => tokio::time::timeout(PING_TIMEOUT, s.ping())
                    .await
                    .unwrap_or(false),
                Err(_) => true,
            }
        }
        .boxed()
    }

    /// MySQL finds out the connection dropped only by using it.
    fn healthy_cheap(&self) -> Option<bool> {
        None
    }

    /// Here we wait for the lock: this is the real check
    /// (`ensure_connected`, called only with no executions in progress), not
    /// the shortcut of `healthy`.
    fn ping(&self) -> BoxFuture<'_, bool> {
        async move {
            tokio::time::timeout(PING_TIMEOUT, async { self.sess.lock().await.ping().await })
                .await
                .unwrap_or(false)
        }
        .boxed()
    }

    fn execute_streaming<'a>(
        &'a self,
        sql: &'a str,
        opts: &'a FetchOptions,
        control: &'a mut tokio::sync::mpsc::Receiver<StreamControl>,
        on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
    ) -> BoxFuture<'a, DbResult<StreamOutcome>> {
        async move {
            let dialect = self.my_dialect();
            let mut g = self.lock().await?;
            let kill_query = async {
                let _ = alamo_driver_mysql::kill(&self.url, self.id, false, &self.tls).await;
            };
            alamo_driver_mysql::execute_streaming(
                &mut g.conn,
                sql,
                &dialect,
                opts,
                control,
                kill_query,
                on_event,
            )
            .await
            .map_err(boxed)
        }
        .boxed()
    }

    fn explain_rows_estimate<'a>(&'a self, sql: &'a str) -> BoxFuture<'a, DbResult<Option<u64>>> {
        async move {
            alamo_driver_mysql::explain_rows_estimate(&mut self.lock().await?.conn, sql)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn set_session_read_only(&self) -> BoxFuture<'_, DbResult<()>> {
        async move {
            alamo_driver_mysql::set_session_read_only(&mut self.lock().await?.conn, self.mariadb)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    /// The session keeps the effective regime after the downgrade; the
    /// server tells the rest. No subject/issuer here: `mysql_async` hides
    /// the certificate, the status variables only carry its validity.
    fn tls_info(&self) -> BoxFuture<'_, DbResult<Option<TlsInfo>>> {
        async move {
            if !self.tls.mode.wants_tls() {
                return Ok(None);
            }
            let status = alamo_driver_mysql::tls_status(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)?;
            Ok(status.map(|s| TlsInfo {
                protocol: s.version,
                cipher: s.cipher,
                server_cert: Some(CertSummary {
                    subject: None,
                    issuer: None,
                    not_before: s.not_before,
                    not_after: s.not_after,
                }),
                verified: self.tls.mode.verifies_chain(),
            }))
        }
        .boxed()
    }

    /// `Capabilities::read_only_txn` is off: never called.
    fn begin_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>> {
        async { Ok(()) }.boxed()
    }

    fn end_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>> {
        async { Ok(()) }.boxed()
    }

    fn connection_budget(&self) -> BoxFuture<'_, DbResult<Option<ConnectionBudget>>> {
        async move {
            alamo_driver_mysql::connection_budget(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    /// After the PC went to sleep or a network change the TCP close never
    /// reaches the server and it keeps `old` in `Sleep` until `wait_timeout`
    /// (8 hours by default), counting it in the user's
    /// `max_user_connections` — at every reopening the zombies pile up until
    /// the user cannot log in any more, not even from HeidiSQL. The `KILL` is
    /// sent from this connection, without opening another one, only within
    /// the same server life (`killable`) and only to idle connections of the
    /// same user (`MySession::kill_idle`).
    fn kill_abandoned<'a>(&'a self, old: &'a [ServerConnId]) -> BoxFuture<'a, ()> {
        async move {
            let Some(me) = &self.server_conn else {
                return;
            };
            let ids: Vec<u32> = old
                .iter()
                .filter(|o| killable(me, o))
                .map(|o| o.id)
                .collect();
            if !ids.is_empty() {
                self.sess.lock().await.kill_idle(&ids).await;
            }
        }
        .boxed()
    }

    fn server_conn_id(&self) -> Option<ServerConnId> {
        self.server_conn.clone()
    }

    /// `COM_QUIT`, so the slot frees up without waiting for the server to
    /// notice the closed socket.
    fn close(self: Arc<Self>) -> BoxFuture<'static, ()> {
        async move {
            if let Ok(c) = Arc::try_unwrap(self) {
                let _ = c.sess.into_inner().conn.disconnect().await;
            }
        }
        .boxed()
    }

    /// No search path, and built-in functions cannot be shadowed.
    fn pin_catalog_search_path(&self) -> BoxFuture<'_, DbResult<()>> {
        async { Ok(()) }.boxed()
    }

    /// MariaDB `idle_transaction_timeout`; MySQL does not have it (no-op).
    fn set_idle_txn_timeout(&self, secs: u32) -> BoxFuture<'_, DbResult<()>> {
        async move {
            alamo_driver_mysql::set_idle_txn_timeout(
                &mut self.lock().await?.conn,
                self.mariadb,
                secs,
            )
            .await
            .map_err(boxed)
        }
        .boxed()
    }

    /// MySQL `max_execution_time` (ms, SELECT only); MariaDB
    /// `max_statement_time` (seconds, all statements). See the driver.
    fn set_statement_timeout(&self, ms: u32) -> BoxFuture<'_, DbResult<()>> {
        async move {
            alamo_driver_mysql::set_statement_timeout(
                &mut self.lock().await?.conn,
                self.mariadb,
                ms,
            )
            .await
            .map_err(boxed)
        }
        .boxed()
    }

    /// No asynchronous notices in MySQL: see `post_statement_warnings`.
    fn drain_async_notices(&self) -> Vec<DbError> {
        Vec::new()
    }

    /// `SHOW WARNINGS` on the same connection.
    fn post_statement_warnings(&self) -> BoxFuture<'_, Vec<DbError>> {
        async move {
            let Ok(mut g) = self.lock().await else {
                return Vec::new();
            };
            alamo_driver_mysql::fetch_warnings(&mut g.conn)
                .await
                .unwrap_or_default()
        }
        .boxed()
    }

    /// Does not touch the mutex: this is the point where the lock is
    /// certainly busy.
    fn terminate(&self) -> BoxFuture<'_, DbResult<bool>> {
        async move {
            alamo_driver_mysql::kill(&self.url, self.id, true, &self.tls)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn use_database<'a>(&'a self, db: &'a str) -> BoxFuture<'a, DbResult<()>> {
        async move {
            alamo_driver_mysql::use_database(&mut self.lock().await?.conn, db)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    /// Reflects the last `USE`, and may be none.
    fn current_database(&self) -> BoxFuture<'_, DbResult<Option<String>>> {
        async move {
            alamo_driver_mysql::current_database(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn schemas(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>> {
        async move {
            introspect::schemas(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn databases(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>> {
        self.schemas()
    }

    fn schema_stats(&self) -> BoxFuture<'_, DbResult<Vec<SchemaStats>>> {
        async move {
            introspect::schema_stats(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn relations<'a>(&'a self, schema: &'a str) -> BoxFuture<'a, DbResult<Vec<RelationInfo>>> {
        async move {
            introspect::relations(&mut self.lock().await?.conn, schema)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn relations_all(&self) -> BoxFuture<'_, DbResult<Vec<RelationInfo>>> {
        async move {
            introspect::relations_all(&mut self.lock().await?.conn)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn columns<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, DbResult<Vec<ColumnInfo>>> {
        async move {
            introspect::columns(&mut self.lock().await?.conn, schema, table)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn foreign_keys<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, DbResult<Vec<ForeignKey>>> {
        async move {
            introspect::foreign_keys(&mut self.lock().await?.conn, schema, table)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn autocomplete_catalog<'a>(
        &'a self,
        schema: Option<&'a str>,
    ) -> BoxFuture<'a, DbResult<Vec<(String, String, String)>>> {
        async move {
            Ok(
                introspect::autocomplete_catalog(&mut self.lock().await?.conn, schema)
                    .await
                    .map_err(boxed)?
                    .into_iter()
                    .map(|e| (e.schema, e.table, e.column))
                    .collect(),
            )
        }
        .boxed()
    }

    fn table_ddl<'a>(&'a self, schema: &'a str, table: &'a str) -> BoxFuture<'a, DbResult<String>> {
        async move {
            introspect::table_ddl(&mut self.lock().await?.conn, schema, table)
                .await
                .map_err(boxed)
        }
        .boxed()
    }
}

/// Percent-encoding of the URL userinfo/path components: everything that is
/// not unreserved (RFC 3986) gets encoded.
fn percent_encode(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{killable, ConnectTarget, Driver, MyDriver, ServerConnId};
    use alamo_core::tls::TlsMode;

    fn target() -> ConnectTarget<'static> {
        ConnectTarget {
            host: "localhost",
            port: 3306,
            user: "root",
            password: "",
            database: "alamo_bench",
            tls: TlsMode::Prefer,
            tunnel_port: None,
        }
    }

    #[test]
    fn conn_string_percent_encodes() {
        let s = MyDriver.conn_string(&ConnectTarget {
            user: "user@weird",
            password: "p@ss/w:rd",
            ..target()
        });
        assert_eq!(
            s,
            "mysql://user%40weird:p%40ss%2Fw%3Ard@localhost:3306/alamo_bench"
        );
    }

    #[test]
    fn conn_string_without_database() {
        let s = MyDriver.conn_string(&ConnectTarget {
            database: "",
            ..target()
        });
        assert_eq!(s, "mysql://root@localhost:3306");
    }

    #[test]
    fn conn_string_ipv6_host_in_brackets() {
        let s = MyDriver.conn_string(&ConnectTarget {
            host: "::1",
            ..target()
        });
        assert_eq!(s, "mysql://root@[::1]:3306/alamo_bench");
    }

    #[test]
    fn conn_string_via_tunnel() {
        let s = MyDriver.conn_string(&ConnectTarget {
            tunnel_port: Some(13306),
            ..target()
        });
        assert_eq!(s, "mysql://root@127.0.0.1:13306/alamo_bench");
    }

    fn at(server: &str, start: i64, id: u32) -> ServerConnId {
        ServerConnId {
            server: server.into(),
            start,
            id,
        }
    }

    #[test]
    fn kills_only_another_connection_of_the_same_server_life() {
        let me = at("uuid-a", 1000, 2);
        assert!(killable(&me, &at("uuid-a", 1000, 1)));
        assert!(killable(&me, &at("uuid-a", 997, 1)), "latency tolerance");
        assert!(
            !killable(&me, &at("uuid-a", 1000 - 3600, 1)),
            "server restarted"
        );
        assert!(!killable(&me, &at("uuid-b", 1000, 1)), "another server");
        assert!(!killable(&me, &at("uuid-a", 1000, 2)), "itself");
    }
}
