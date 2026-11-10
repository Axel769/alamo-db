//! PostgreSQL driver (§2.2), on `alamo-driver-postgres`.

use std::any::Any;
use std::sync::Arc;

use alamo_core::dialect::Dialect;
use alamo_core::error::DbError;
use alamo_core::meta::ColumnSource;
use alamo_core::schema_info::{
    ColumnInfo, ConnectionBudget, ForeignKey, RelationInfo, SchemaInfo, SchemaStats, SourceColumn,
};
use alamo_core::stream::{FetchOptions, StreamControl, StreamEvent, StreamOutcome};
use alamo_core::tls::{TlsInfo, TlsMode, TlsOptions};
use alamo_core::NetOptions;
use alamo_driver_postgres::introspect;
use alamo_driver_postgres::{PgDialect, PgError, PgSession};
use futures_util::future::BoxFuture;
use futures_util::FutureExt;

use super::{
    Canceller, ConnectTarget, Connection, DbConn, DbResult, Driver, EngineInfo, ServerConnId,
    PING_TIMEOUT,
};

pub(super) struct PgDriver;

impl Driver for PgDriver {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "postgres",
            name: "PostgreSQL",
            short_name: "PostgreSQL",
            default_port: 5432,
            default_user: "postgres",
            database_required: true,
            tls_modes: &[
                TlsMode::Prefer,
                TlsMode::Disable,
                TlsMode::Require,
                TlsMode::VerifyCa,
                TlsMode::VerifyFull,
            ],
            sql_syntax: "postgres",
            capabilities: PgDialect::default().capabilities(),
        }
    }

    /// Keyword/value form (tokio-postgres accepts it): avoids the URL
    /// percent-encoding, the classic place where a password with `@` or `/`
    /// breaks everything.
    fn conn_string(&self, t: &ConnectTarget<'_>) -> String {
        // tokio-postgres only knows disable/prefer/require: it is the field
        // that decides WHETHER to attempt TLS. Verification (verify-ca/
        // verify-full) lives in the rustls connector built from `TlsOptions`,
        // so here the verifying modes become require.
        let sslmode = match t.tls {
            TlsMode::Disable => "disable",
            TlsMode::Prefer => "prefer",
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => "require",
        };
        let mut s = format!(
            "host={} port={} user={} dbname={} sslmode={}",
            quote(t.host),
            t.tunnel_port.unwrap_or(t.port),
            quote(t.user),
            quote(t.database),
            sslmode
        );
        if t.tunnel_port.is_some() {
            // SSH tunnel: the TCP goes to the local listener (`hostaddr`),
            // but `host` stays the real name — it is what tokio-postgres
            // uses for the TLS certificate verification.
            s.push_str(" hostaddr=127.0.0.1");
        }
        if !t.password.is_empty() {
            s.push_str(&format!(" password={}", quote(t.password)));
        }
        s
    }

    fn connect<'a>(
        &'a self,
        url: &'a str,
        tls: &'a TlsOptions,
        net: &'a NetOptions,
    ) -> BoxFuture<'a, DbResult<DbConn>> {
        async move {
            let sess = PgSession::connect_net(url, tls, net).await.map_err(boxed)?;
            Ok(Arc::new(PgConn(Arc::new(sess))) as DbConn)
        }
        .boxed()
    }
}

/// libpq-style quoting: value in single quotes, `\` and `'` escaped.
fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn boxed(e: PgError) -> Box<DbError> {
    Box::new(e.into_db_error())
}

fn boxed_pg(e: tokio_postgres::Error) -> Box<DbError> {
    boxed(PgError::from(e))
}

/// The `PgSession` is shared: the M0 bench reads its client directly
/// (`Session::pg`).
pub(crate) struct PgConn(pub(crate) Arc<PgSession>);

struct PgCancel {
    token: tokio_postgres::CancelToken,
    /// The session's connector (§12): the `CancelRequest` travels on a new
    /// connection, which must speak the same TLS.
    tls: Option<tokio_postgres_rustls::MakeRustlsConnect>,
}

impl Canceller for PgCancel {
    fn cancel(&self) -> BoxFuture<'static, bool> {
        let token = self.token.clone();
        let tls = self.tls.clone();
        async move {
            match tls {
                Some(connector) => token.cancel_query(connector).await.is_ok(),
                None => token.cancel_query(tokio_postgres::NoTls).await.is_ok(),
            }
        }
        .boxed()
    }
}

impl PgConn {
    fn batch<'a>(&'a self, sql: &'a str) -> BoxFuture<'a, DbResult<()>> {
        async move { self.0.client.batch_execute(sql).await.map_err(boxed_pg) }.boxed()
    }
}

impl Connection for PgConn {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        Arc::new(PgDialect::default())
    }

    fn canceller(&self) -> Arc<dyn Canceller> {
        Arc::new(PgCancel {
            token: self.0.cancel_token.clone(),
            tls: self.0.tls_connector(),
        })
    }

    /// The check is free: tokio-postgres knows whether the connection is closed.
    fn healthy(&self) -> BoxFuture<'_, bool> {
        let alive = !self.0.client.is_closed();
        async move { alive }.boxed()
    }

    fn healthy_cheap(&self) -> Option<bool> {
        Some(!self.0.client.is_closed())
    }

    fn ping(&self) -> BoxFuture<'_, bool> {
        async move {
            !self.0.client.is_closed()
                && tokio::time::timeout(PING_TIMEOUT, self.0.client.simple_query(""))
                    .await
                    .is_ok_and(|r| r.is_ok())
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
            alamo_driver_postgres::execute::execute_streaming(
                &self.0.client,
                sql,
                opts,
                control,
                on_event,
            )
            .await
            .map_err(boxed)
        }
        .boxed()
    }

    /// On PG the statement resolves names with the user's default
    /// `search_path`, not with the pinned one of the catalog lane.
    fn explain_rows_estimate<'a>(&'a self, sql: &'a str) -> BoxFuture<'a, DbResult<Option<u64>>> {
        async move {
            alamo_driver_postgres::explain_rows_estimate(&self.0.client, sql)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn set_session_read_only(&self) -> BoxFuture<'_, DbResult<()>> {
        self.batch("SET default_transaction_read_only = on")
    }

    /// `prefer` may have fallen back to plaintext and tokio-postgres does
    /// not say so: the server does, with protocol and cipher. The
    /// certificate is the one the driver's verifier saw at the handshake.
    fn server_version(&self) -> Option<String> {
        self.0.server_version.clone()
    }

    fn tls_info(&self) -> BoxFuture<'_, DbResult<Option<TlsInfo>>> {
        async move {
            let row = self
                .0
                .client
                .query_one(
                    "SELECT ssl, version, cipher FROM pg_catalog.pg_stat_ssl \
                     WHERE pid = pg_catalog.pg_backend_pid()",
                    &[],
                )
                .await
                .map_err(boxed_pg)?;
            if !row.get::<_, bool>(0) {
                return Ok(None);
            }
            Ok(Some(TlsInfo {
                protocol: row.get(1),
                cipher: row.get(2),
                server_cert: self
                    .0
                    .server_cert_der()
                    .and_then(|der| crate::x509::summarize_der(&der).ok()),
                verified: self.0.tls_verifies_chain(),
            }))
        }
        .boxed()
    }

    /// The `SELECT 1` takes the snapshot: from then on `SET
    /// transaction_read_only = off` is refused by the server even as the
    /// user's first statement.
    fn begin_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>> {
        self.batch("BEGIN READ ONLY; SELECT 1")
    }

    fn end_read_only_txn(&self) -> BoxFuture<'_, DbResult<()>> {
        self.batch("COMMIT")
    }

    /// Not read yet on PG.
    fn connection_budget(&self) -> BoxFuture<'_, DbResult<Option<ConnectionBudget>>> {
        async { Ok(None) }.boxed()
    }

    /// The server notices a closed socket by itself.
    fn server_conn_id(&self) -> Option<ServerConnId> {
        None
    }

    fn kill_abandoned<'a>(&'a self, _old: &'a [ServerConnId]) -> BoxFuture<'a, ()> {
        async {}.boxed()
    }

    /// Dropping the client closes the connection.
    fn close(self: Arc<Self>) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    /// `search_path` = `pg_catalog` only (and `pg_temp` at the end, where it
    /// does not look up functions).
    fn pin_catalog_search_path(&self) -> BoxFuture<'_, DbResult<()>> {
        self.batch("SET search_path = pg_catalog, pg_temp")
    }

    fn set_idle_txn_timeout(&self, secs: u32) -> BoxFuture<'_, DbResult<()>> {
        async move {
            self.0
                .client
                .batch_execute(&format!(
                    "SET idle_in_transaction_session_timeout = '{secs}s'"
                ))
                .await
                .map_err(boxed_pg)
        }
        .boxed()
    }

    fn set_statement_timeout(&self, ms: u32) -> BoxFuture<'_, DbResult<()>> {
        async move {
            self.0
                .client
                .batch_execute(&format!("SET statement_timeout = {ms}"))
                .await
                .map_err(boxed_pg)
        }
        .boxed()
    }

    fn drain_async_notices(&self) -> Vec<DbError> {
        self.0.drain_notices()
    }

    /// PG sends its notices asynchronously (`drain_async_notices`).
    fn post_statement_warnings(&self) -> BoxFuture<'_, Vec<DbError>> {
        async { Vec::new() }.boxed()
    }

    fn terminate(&self) -> BoxFuture<'_, DbResult<bool>> {
        async move { self.0.terminate_backend().await.map_err(boxed) }.boxed()
    }

    /// The database is fixed at connect: changing it requires a new
    /// connection (§3.3).
    fn use_database<'a>(&'a self, _db: &'a str) -> BoxFuture<'a, DbResult<()>> {
        async {
            Err(Box::new(DbError::internal(
                "on PostgreSQL the database is chosen at connect",
            )))
        }
        .boxed()
    }

    fn current_database(&self) -> BoxFuture<'_, DbResult<Option<String>>> {
        async move {
            let row = self
                .0
                .client
                .query_one("SELECT current_database()", &[])
                .await
                .map_err(boxed_pg)?;
            Ok(Some(row.get::<_, String>(0)))
        }
        .boxed()
    }

    fn schemas(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>> {
        async move { introspect::schemas(&self.0.client).await.map_err(boxed) }.boxed()
    }

    fn databases(&self) -> BoxFuture<'_, DbResult<Vec<SchemaInfo>>> {
        async move { introspect::databases(&self.0.client).await.map_err(boxed) }.boxed()
    }

    fn schema_stats(&self) -> BoxFuture<'_, DbResult<Vec<SchemaStats>>> {
        async move {
            introspect::schema_stats(&self.0.client)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn relations<'a>(&'a self, schema: &'a str) -> BoxFuture<'a, DbResult<Vec<RelationInfo>>> {
        async move {
            introspect::relations(&self.0.client, schema)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn relations_all(&self) -> BoxFuture<'_, DbResult<Vec<RelationInfo>>> {
        async move {
            introspect::relations_all(&self.0.client)
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
            introspect::columns(&self.0.client, schema, table)
                .await
                .map_err(boxed)
        }
        .boxed()
    }

    fn resolve_sources<'a>(
        &'a self,
        sources: &'a [Option<ColumnSource>],
    ) -> BoxFuture<'a, DbResult<Vec<Option<SourceColumn>>>> {
        async move {
            let refs: Vec<(u32, i16)> = sources
                .iter()
                .filter_map(|s| match s {
                    Some(ColumnSource::Postgres {
                        table_oid,
                        column_attnum,
                    }) => Some((*table_oid, *column_attnum)),
                    _ => None,
                })
                .collect();
            let mut names = introspect::resolve_columns(&self.0.client, &refs)
                .await
                .map_err(boxed)?
                .into_iter();
            Ok(sources
                .iter()
                .map(|s| match s {
                    Some(ColumnSource::Postgres { .. }) => names.next().flatten(),
                    _ => None,
                })
                .collect())
        }
        .boxed()
    }

    fn foreign_keys<'a>(
        &'a self,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, DbResult<Vec<ForeignKey>>> {
        async move {
            introspect::foreign_keys(&self.0.client, schema, table)
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
            Ok(introspect::autocomplete_catalog(&self.0.client, schema)
                .await
                .map_err(boxed)?
                .into_iter()
                .map(|e| (e.schema, e.table, e.column))
                .collect())
        }
        .boxed()
    }

    fn table_ddl<'a>(&'a self, schema: &'a str, table: &'a str) -> BoxFuture<'a, DbResult<String>> {
        async move {
            introspect::table_ddl(&self.0.client, schema, table)
                .await
                .map_err(boxed)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ConnectTarget<'static> {
        ConnectTarget {
            host: "localhost",
            port: 5432,
            user: "postgres",
            password: "",
            database: "alamo_bench",
            tls: TlsMode::Prefer,
            tunnel_port: None,
        }
    }

    #[test]
    fn conn_string_escaping() {
        let s = PgDriver.conn_string(&ConnectTarget {
            user: "o'brien",
            password: "p@ss'w\\ord",
            ..target()
        });
        assert!(s.contains("user='o\\'brien'"));
        assert!(s.contains("password='p@ss\\'w\\\\ord'"));
        assert!(s.contains("sslmode=prefer"));
        assert!(!PgDriver.conn_string(&target()).contains("password"));
        // The verifying modes travel as require: verification lives in the
        // connector, not in the string (tokio-postgres does not know them).
        let verify = ConnectTarget {
            tls: TlsMode::VerifyFull,
            ..target()
        };
        assert!(PgDriver.conn_string(&verify).contains("sslmode=require"));
    }

    #[test]
    fn conn_string_via_tunnel() {
        // TCP to the listener (`hostaddr` + local port), `host` stays the
        // real name for the TLS verification.
        let s = PgDriver.conn_string(&ConnectTarget {
            tunnel_port: Some(15432),
            ..target()
        });
        assert!(s.contains("host='localhost'"));
        assert!(s.contains("hostaddr=127.0.0.1"));
        assert!(s.contains("port=15432"));
    }
}
