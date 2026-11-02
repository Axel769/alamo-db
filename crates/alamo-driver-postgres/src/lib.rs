//! # alamo-driver-postgres
//!
//! PostgreSQL implementation: connection, dialect, decoding from the protocol
//! to the columnar layout of `alamo-core`.
//!
//! The `§N` references point to `ARCHITECTURE.md` at the repo root.

#![forbid(unsafe_code)]

pub mod decode;
pub mod dialect;
pub mod error;
pub mod execute;
pub mod introspect;
pub mod oid;
pub mod tls;

pub use dialect::PgDialect;
pub use error::PgError;
pub use execute::{
    execute_collect, execute_streaming, explain_rows_estimate, measure_ceiling, ExecutionStats,
    FetchOptions, StopReason, StreamControl, StreamEvent, StreamOutcome,
};

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures_util::future::poll_fn;
use tokio::task::JoinHandle;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, Client, NoTls, Socket};
use tokio_postgres_rustls::MakeRustlsConnect;

use alamo_core::error::DbError;
use alamo_core::tls::TlsOptions;

/// An open connection plus what is needed to cancel its queries.
///
/// §7.1: **at connect the cancellation handle is captured and kept**.
/// Cancellation in PG is a `CancelRequest` packet on a **new** connection,
/// built with the backend PID and the secret key received at handshake:
/// afterwards, that data cannot be recovered.
///
/// ⚠️ It is best-effort and asynchronous: it may arrive after the query has
/// ended, and **behind a pooler (PgBouncer) it may not reach the right
/// backend and simply not work**. That is why `Capabilities::can_cancel`
/// must be verified, not assumed: better a disabled Cancel button with an
/// explanatory tooltip than a button that pretends.
pub struct PgSession {
    pub client: Client,
    pub cancel_token: tokio_postgres::CancelToken,
    /// Task that pumps the protocol. If it dies, the connection is lost.
    pub connection: JoinHandle<()>,
    /// Asynchronous `NOTICE`/`WARNING` (§6): PL/pgSQL `RAISE NOTICE` is *the*
    /// way debugging is done — the connection task queues them here and the
    /// execution drains them towards the UI.
    notices: Arc<Mutex<VecDeque<DbError>>>,
    /// Backend PID, for the §7.2 escalation (`pg_terminate_backend`).
    /// Captured at connect: afterwards it can no longer be queried if the
    /// connection is busy.
    backend_pid: Option<i32>,
    /// `backend_start` of the same backend: with the PID it identifies THE
    /// session. The PID alone may have been reused by the system for
    /// another session after ours died. `None` = not readable (falls back
    /// on the PID alone).
    backend_start: Option<String>,
    /// Only used by `terminate_backend`: the termination travels on a new
    /// connection, like the cancel (§7).
    url: String,
    /// TLS connector of the session (`None` = plaintext): cancel, terminate
    /// and every out-of-band connection MUST use it — opening them in
    /// plaintext on a TLS profile would be a silent downgrade (§12).
    tls: Option<MakeRustlsConnect>,
}

impl PgSession {
    /// Plaintext connection, never attempting TLS: bench and tests. The
    /// profile path goes through `connect`.
    pub async fn connect_plain(url: &str) -> Result<PgSession, PgError> {
        Self::connect(url, &TlsOptions::plain()).await
    }

    /// Connection with the profile's TLS regime (§12). The connector is
    /// kept: cancel/terminate reopen connections and must speak the same
    /// TLS. If the server refuses TLS with mode `prefer`, the driver itself
    /// degrades to plaintext (libpq semantics); from `require` upwards the
    /// connect fails.
    pub async fn connect(url: &str, tls_opts: &TlsOptions) -> Result<PgSession, PgError> {
        match tls::make_connector(tls_opts)? {
            Some(connector) => Self::connect_inner(url, connector.clone(), Some(connector)).await,
            None => Self::connect_inner(url, NoTls, None).await,
        }
    }

    async fn connect_inner<T>(
        url: &str,
        connector: T,
        tls: Option<MakeRustlsConnect>,
    ) -> Result<PgSession, PgError>
    where
        T: MakeTlsConnect<Socket>,
        T::Stream: Send + 'static,
    {
        // TCP keepalive (see `TCP_KEEPALIVE_IDLE`): probe after 30 s of
        // silence, then every 10 s; 3 unanswered probes = dead connection.
        let mut config: tokio_postgres::Config = url.parse().map_err(PgError::from)?;
        config
            .keepalives(true)
            .keepalives_idle(alamo_core::TCP_KEEPALIVE_IDLE)
            .keepalives_interval(std::time::Duration::from_secs(10))
            .keepalives_retries(3);
        let (client, mut connection) = config.connect(connector).await.map_err(PgError::from)?;
        let cancel_token = client.cancel_token();
        let notices: Arc<Mutex<VecDeque<DbError>>> = Arc::default();
        let sink = Arc::clone(&notices);
        let handle = tokio::spawn(async move {
            // `connection.await` would discard the asynchronous messages: the
            // explicit loop on `poll_message` is needed. Exit = connection
            // dropped (at M1+ it becomes an event towards the UI, §9.3).
            loop {
                match poll_fn(|cx| connection.poll_message(cx)).await {
                    Some(Ok(AsyncMessage::Notice(n))) => {
                        sink.lock().unwrap().push_back(error::from_pg_db_error(&n));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
        });
        // Best-effort: without a PID the §7.2 escalation stays disabled, the
        // session works anyway.
        let backend_pid = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .ok()
            .map(|row| row.get::<_, i32>(0));
        let backend_start = client
            .query_opt(
                "SELECT backend_start::text FROM pg_catalog.pg_stat_activity \
                 WHERE pid = pg_catalog.pg_backend_pid()",
                &[],
            )
            .await
            .ok()
            .flatten()
            .and_then(|row| row.get::<_, Option<String>>(0));

        Ok(PgSession {
            client,
            cancel_token,
            connection: handle,
            notices,
            backend_pid,
            backend_start,
            url: url.to_owned(),
            tls,
        })
    }

    /// The session's TLS connector, for the out-of-band connections opened
    /// from outside (cancel in the registry, §7.1).
    pub fn tls_connector(&self) -> Option<MakeRustlsConnect> {
        self.tls.clone()
    }

    /// Empties the queue of the notices received so far, in arrival order.
    pub fn drain_notices(&self) -> Vec<DbError> {
        self.notices.lock().unwrap().drain(..).collect()
    }

    /// §7.2 escalation: `pg_terminate_backend` on a new connection.
    ///
    /// Destructive by definition — it kills the server-side session, open
    /// transaction and state included. It must only be offered when the
    /// cancel had no effect (pooler, non-interruptible query), never as the
    /// first shot.
    pub async fn terminate_backend(&self) -> Result<bool, PgError> {
        let Some(pid) = self.backend_pid else {
            return Ok(false);
        };
        // Same TLS as the session (§12): the escalation connection must not
        // degrade to plaintext.
        let start = self.backend_start.as_deref();
        let terminated = match self.tls.clone() {
            Some(connector) => Self::terminate_with(&self.url, pid, start, connector).await?,
            None => Self::terminate_with(&self.url, pid, start, NoTls).await?,
        };
        Ok(terminated)
    }

    /// Terminates only if PID and `backend_start` are still the ones
    /// captured at connect; otherwise the session is already dead and the
    /// PID belongs to another one: `false`, nothing to do.
    async fn terminate_with<T>(
        url: &str,
        pid: i32,
        backend_start: Option<&str>,
        connector: T,
    ) -> Result<bool, PgError>
    where
        T: MakeTlsConnect<Socket>,
        T::Stream: Send + 'static,
    {
        let (client, connection) = tokio_postgres::connect(url, connector)
            .await
            .map_err(PgError::from)?;
        let pump = tokio::spawn(async move {
            let _ = connection.await;
        });
        let row = match backend_start {
            Some(start) => client
                .query_opt(
                    "SELECT pg_catalog.pg_terminate_backend(pid) \
                     FROM pg_catalog.pg_stat_activity \
                     WHERE pid = $1 AND backend_start::text = $2",
                    &[&pid, &start],
                )
                .await
                .map_err(PgError::from)?,
            None => Some(
                client
                    .query_one("SELECT pg_catalog.pg_terminate_backend($1)", &[&pid])
                    .await
                    .map_err(PgError::from)?,
            ),
        };
        let terminated = row.is_some_and(|r| r.get::<_, bool>(0));
        drop(client);
        pump.abort();
        Ok(terminated)
    }
}
