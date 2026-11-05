//! # alamo-driver-mysql
//!
//! MySQL/MariaDB implementation (internal flag, §2): connection, dialect,
//! decoding from the protocol to the columnar layout of `alamo-core`.
//!
//! The three divergences that cost code (§15.1): authentication
//! (`caching_sha2_password`, which MariaDB does not implement), MariaDB
//! `JSON` which is TEXT on the wire, product identification. The `§N`
//! references point to `ARCHITECTURE.md` at the repo root.

#![forbid(unsafe_code)]

pub mod dialect;
pub mod error;
pub mod execute;
pub mod introspect;
pub mod tls;
pub mod typecode;

pub use dialect::MyDialect;
pub use error::MyError;
pub use execute::{
    connection_budget, current_database, execute_streaming, explain_rows_estimate, fetch_warnings,
    set_idle_txn_timeout, set_session_read_only, set_statement_timeout, use_database,
};
pub use tls::{tls_status, TlsStatus};

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, SslOpts};

use alamo_core::tls::{TlsMode, TlsOptions};
use alamo_core::NetOptions;

/// An open connection plus what is needed to cancel its queries.
///
/// §7.1: cancellation in MySQL is `KILL QUERY <connection_id>` from a
/// **second** connection. The connection id is captured at handshake; the
/// URL is used to open the connection that fires the `KILL`.
///
/// ⚠️ Best-effort as in PG: on managed services the privilege may be
/// missing and fail silently — `Capabilities::can_cancel` must be verified
/// at connect, not assumed.
pub struct MySession {
    pub conn: Conn,
    /// Connection id from the handshake: the target of `KILL QUERY`.
    pub connection_id: u32,
    /// True if the server is MariaDB. **From the handshake, never from
    /// parsing the version string** (§15.1 divergence 3).
    pub is_mariadb: bool,
    /// Server start (see `server_start`): connection ids are only valid
    /// within the same server lifetime.
    pub server_start: Option<i64>,
    /// Which server this is (see `server_identity`): with `server_start`, it
    /// tells whether an old connection id still means the same connection.
    pub server_identity: Option<String>,
    url: String,
    /// TLS regime of the profile: cancel/terminate reopen connections and
    /// must speak the same TLS (§12).
    tls: TlsOptions,
}

impl MySession {
    /// Plaintext connection, never attempting TLS: tests and tools. The
    /// profile path goes through `connect`.
    pub async fn connect_plain(url: &str) -> Result<MySession, MyError> {
        Self::connect(url, &TlsOptions::plain()).await
    }

    /// Connection with the profile's TLS regime (§12).
    ///
    /// `prefer` is the only mode with a fallback: if the server does not
    /// expose the SSL capability in the handshake we retry in plaintext —
    /// the libpq semantics, applied by hand because `mysql_async` lacks it.
    /// Any other error (certificate, auth) does NOT degrade.
    ///
    /// Authentication is divergence 1 of §15.1: with MySQL >= 8.0.4 the
    /// default is `caching_sha2_password` (first handshake: TLS or RSA
    /// exchange — `mysql_async` handles the RSA exchange on its own);
    /// MariaDB does not implement it and uses native/ed25519. Both paths go
    /// through here.
    pub async fn connect(url: &str, tls: &TlsOptions) -> Result<MySession, MyError> {
        Self::connect_net(url, tls, &NetOptions::default()).await
    }

    /// `connect` with the network choices of the Connections settings
    /// (§16). The connect timeout is the session's (it covers the extra
    /// queries too); here only the keepalive.
    pub async fn connect_net(
        url: &str,
        tls: &TlsOptions,
        net: &NetOptions,
    ) -> Result<MySession, MyError> {
        match Self::connect_with(url, ssl_opts(tls)?, net).await {
            Ok(sess) => Ok(sess.with_tls(tls.clone())),
            Err(e) if tls.mode == TlsMode::Prefer && server_refused_tls(&e) => {
                // The `prefer` degradation: from here on the session IS in
                // plaintext, and so must cancel/terminate be.
                Ok(Self::connect_with(url, None, net)
                    .await?
                    .with_tls(TlsOptions::plain()))
            }
            Err(e) => Err(e),
        }
    }

    fn with_tls(mut self, tls: TlsOptions) -> MySession {
        self.tls = tls;
        self
    }

    async fn connect_with(
        url: &str,
        ssl: Option<SslOpts>,
        net: &NetOptions,
    ) -> Result<MySession, MyError> {
        let opts = Opts::from_url(url).map_err(|e| MyError::from(mysql_async::Error::from(e)))?;
        // The columnar layout declares text as UTF-8 (§4.2): the session must
        // be forced to utf8mb4, otherwise a different charset requested by
        // the URL or inherited from the server default delivers non-UTF8
        // bytes declared as text. `setup` overrides any URL parameter and is
        // re-run after the `init` queries and on every reset/change_user:
        // the last SET NAMES always wins.
        // TCP keepalive (see `TCP_KEEPALIVE_IDLE`): keeps the connection alive
        // through NATs/firewalls during the long silences between queries.
        let opts = OptsBuilder::from_opts(opts)
            .setup(vec!["SET NAMES utf8mb4"])
            .ssl_opts(ssl)
            .tcp_keepalive(Some(net.tcp_keepalive));
        let mut conn = Conn::new(opts).await.map_err(MyError::from)?;
        let connection_id = conn.id();
        let is_mariadb = detect_mariadb(&mut conn).await?;
        let server_start = server_start(&mut conn).await;
        let server_identity = server_identity(&mut conn, is_mariadb).await;
        Ok(MySession {
            conn,
            connection_id,
            is_mariadb,
            server_start,
            server_identity,
            url: url.to_owned(),
            tls: TlsOptions::plain(),
        })
    }

    /// `KILL QUERY` from the second connection (§7.1): interrupts the
    /// current statement, the target connection survives. `false` = the
    /// server refused (privileges) or the connection no longer exists.
    pub async fn cancel_query(&self) -> Result<bool, MyError> {
        kill(&self.url, self.connection_id, false, &self.tls).await
    }

    /// Escalation (§7.2): `KILL <id>` kills the **connection**, not just the
    /// query — the equivalent of `pg_terminate_backend`.
    pub async fn terminate(&self) -> Result<bool, MyError> {
        kill(&self.url, self.connection_id, true, &self.tls).await
    }

    /// The two pieces of data needed to cancel from outside (§7.1): the
    /// session keeps them in the registry, where the live connection does
    /// not reach.
    pub fn cancel_parts(&self) -> (String, u32) {
        (self.url.clone(), self.connection_id)
    }

    /// The effective TLS regime of the session (after any `prefer`
    /// degradation): the registry keeps it next to `cancel_parts` (§7.1).
    pub fn tls_options(&self) -> TlsOptions {
        self.tls.clone()
    }

    /// `KILL <id>` from THIS connection for the connections among `ids` we
    /// have abandoned (dead for us, alive for the server), but only those
    /// that the server shows idle (`Sleep`) and of this same user: one
    /// running something, or another user's, is never touched — behind a
    /// proxy that shares server connections between clients the id could
    /// belong to someone else. No privileges needed: one's own connections
    /// can always be closed. Best-effort: how many were closed.
    pub async fn kill_idle(&mut self, ids: &[u32]) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let list = ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let idle: Vec<u64> = match self
            .conn
            .query(format!(
                "SELECT ID FROM information_schema.PROCESSLIST \
                 WHERE ID IN ({list}) AND COMMAND = 'Sleep' \
                   AND USER = {}",
                execute::OWN_USER_SQL
            ))
            .await
        {
            Ok(rows) => rows,
            Err(_) => return 0,
        };
        let mut killed = 0;
        for id in idle {
            if self.conn.query_drop(format!("KILL {id}")).await.is_ok() {
                killed += 1;
            }
        }
        killed
    }

    /// Liveness of the connection, for the lazy reopening of the auxiliary
    /// ones (§3). Costs a roundtrip: the MySQL protocol has no local flag.
    /// Out of phase (see `in_sync`) = dead, without a roundtrip: the ping
    /// would read the reply left behind and say yes.
    pub async fn ping(&mut self) -> bool {
        self.in_sync().is_ok() && self.conn.ping().await.is_ok()
    }

    /// A command interrupted halfway (abandoned future, e.g. by the time cap
    /// of a ping) leaves a never-read reply on the socket: the next command
    /// reads that one instead of its own, and from there every reply shifts
    /// by one — until a row with the columns of another query panics in the
    /// mysql_async conversion. The driver notices (`is_disconnected`: raised
    /// at command start, lowered only when the command completes or on a
    /// server error) but still uses the connection: it must be refused
    /// here. A transport error, because the connection is lost and must be
    /// reopened.
    pub fn in_sync(&self) -> Result<(), MyError> {
        if !self.conn.is_disconnected() {
            return Ok(());
        }
        Err(MyError(mysql_async::Error::Io(mysql_async::IoError::Io(
            std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "connection interrupted in the middle of a command: it must be reopened",
            ),
        ))))
    }
}

/// Translation `TlsOptions` → `mysql_async`'s `SslOpts` (`None` =
/// plaintext). The semantics of the modes are in `alamo_core::tls`; the
/// embedded roots (webpki) only apply in the verifying modes, where a custom
/// CA of the profile **replaces** them — a server with an internal CA must
/// not pass through a coincidence with the public roots.
fn ssl_opts(tls: &TlsOptions) -> Result<Option<SslOpts>, MyError> {
    tls.check()
        .map_err(|m| MyError(mysql_async::Error::Other(m.into())))?;
    // `with_danger_skip_domain_validation` of mysql_async 0.36/0.37.1 does
    // not work with rustls >= 0.23.18: it recognises the name mismatch by
    // looking for "NotValidForName" in the message, which is now
    // "certificate not valid for name …". verify-ca would always fail with
    // an error that looks like a wrong certificate. Fixed on mysql_async
    // master (is_name_mismatch), not yet published: until then a clear
    // refusal. Fail-closed in both cases.
    if tls.mode == TlsMode::VerifyCa {
        return Err(MyError(mysql_async::Error::Other(
            "verify-ca is not yet available on MySQL/MariaDB: use verify-full \
             (certificate with the server host name)"
                .into(),
        )));
    }
    if !tls.mode.wants_tls() {
        return Ok(None);
    }
    let mut ssl = SslOpts::default();
    if let Some(name) = &tls.tls_server_name {
        // SSH tunnel: TCP points to 127.0.0.1, the certificate speaks of the
        // real database — hostname verification must be done on that name.
        ssl = ssl.with_danger_tls_hostname_override(Some(name.clone()));
    }
    if tls.mode.verifies_chain() {
        if let Some(ca) = &tls.root_cert {
            ssl = ssl
                .with_root_certs(vec![ca.clone().into()])
                .with_disable_built_in_roots(true);
        }
    } else {
        // prefer/require: encrypted channel without server authentication.
        ssl = ssl
            .with_danger_accept_invalid_certs(true)
            .with_danger_skip_domain_validation(true);
    }
    Ok(Some(ssl))
}

/// The server does not expose the SSL capability in the handshake: the only
/// error on which `prefer` degrades to plaintext.
fn server_refused_tls(e: &MyError) -> bool {
    matches!(
        e.0,
        mysql_async::Error::Driver(mysql_async::DriverError::NoClientSslFlagFromServer)
    )
}

/// `KILL [QUERY] <id>` on a connection opened on purpose (§7.1): by
/// definition outside the connection that is executing, with the TLS of the
/// target session (§12).
pub async fn kill(
    url: &str,
    connection_id: u32,
    whole_connection: bool,
    tls: &TlsOptions,
) -> Result<bool, MyError> {
    let opts = Opts::from_url(url).map_err(|e| MyError::from(mysql_async::Error::from(e)))?;
    let opts = OptsBuilder::from_opts(opts).ssl_opts(ssl_opts(tls)?);
    let mut killer = Conn::new(opts).await.map_err(MyError::from)?;
    let verb = if whole_connection {
        "KILL"
    } else {
        "KILL QUERY"
    };
    let res = killer.query_drop(format!("{verb} {connection_id}")).await;
    let _ = killer.disconnect().await;
    Ok(res.is_ok())
}

/// Server start instant in Unix seconds, from `Uptime` and the client's
/// clock: two connections with close values talk to the same server
/// lifetime. After a restart the connection ids start over, and an old id
/// may belong to another connection of the same user (HeidiSQL): whoever
/// `KILL`s an old id compares this first. `None` if it cannot be read.
async fn server_start(conn: &mut Conn) -> Option<i64> {
    let row: Option<(String, String)> = conn
        .query_first::<mysql_async::Row, _>("SHOW GLOBAL STATUS LIKE 'Uptime'")
        .await
        .ok()
        .flatten()
        .and_then(|r| error::row_as(r).ok());
    let uptime: i64 = row?.1.parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(i64::try_from(now).ok()? - uptime)
}

/// The server's own id: `@@server_uuid` on MySQL (unique per installation,
/// kept across restarts), `@@hostname` on MariaDB, which has no UUID.
/// `None` if it cannot be read.
async fn server_identity(conn: &mut Conn, mariadb: bool) -> Option<String> {
    let sql = if mariadb {
        "SELECT @@hostname"
    } else {
        "SELECT @@server_uuid"
    };
    conn.query_first::<Option<String>, _>(sql)
        .await
        .ok()
        .flatten()
        .flatten()
}

/// §15.1 divergence 3: MariaDB historically prepends `5.5.5-` to the
/// version, so parsing the string is fragile by definition. `mysql_async`
/// does not expose the handshake flag; two signals are combined:
/// `@@version_comment` (which MariaDB sets to "...MariaDB...", but is
/// **configurable** by the administrator) and `VERSION()` (which contains
/// "MariaDB" or the compatibility prefix `5.5.5-`). MariaDB if AT LEAST ONE
/// of the two says yes: this way a customised version_comment does not hide
/// the product.
async fn detect_mariadb(conn: &mut Conn) -> Result<bool, MyError> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_first::<mysql_async::Row, _>("SELECT @@version_comment, VERSION()")
        .await
        .map_err(MyError::from)
        .and_then(error::first_as)?;
    let (comment, version) = row.unwrap_or((None, None));
    let by_comment = comment
        .map(|c| c.to_ascii_lowercase().contains("mariadb"))
        .unwrap_or(false);
    let by_version = version
        .map(|v| v.to_ascii_lowercase().contains("mariadb") || v.starts_with("5.5.5-"))
        .unwrap_or(false);
    Ok(by_comment || by_version)
}
