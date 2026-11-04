//! # alamo-tunnel
//!
//! SSH tunnel (§12): local port-forward via `russh` — a pure Rust protocol
//! implementation, consistent with the stack (no binding to the system OpenSSH).
//!
//! Model: a TCP listener on `127.0.0.1:random_port`; every accepted
//! connection becomes a `direct-tcpip` channel towards the database, on the
//! other side of the tunnel. The database session stays transport-agnostic:
//! the app rewrites host/port of the connection string to point at the
//! listener, and ALL connections — lanes, out-of-band cancel/KILL,
//! escalation — go through here by construction, because they share the
//! same string.
//!
//! Host key: verified against `~/.ssh/known_hosts` and the key confirmed by
//! the user in Alamo (see `host_key`). Changed or revoked key = connection
//! refused (possible MITM, never a warning to click away); never-seen host =
//! refusal with the fingerprint, which the app shows for confirmation and
//! then passes in `TunnelConfig::trusted_key`.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::{load_secret_key, ssh_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};

mod host_key;
use host_key::Verdict;

#[derive(Debug)]
pub enum TunnelError {
    /// The server key does NOT match the recorded one (in `known_hosts` or
    /// confirmed in Alamo): possible MITM, refuse and nothing else.
    /// `source` says which record.
    HostKeyChanged {
        source: String,
    },
    /// Key on an `@revoked` line of `known_hosts`.
    HostKeyRevoked {
        line: usize,
    },
    /// Never-seen server: no connection until the user confirms the
    /// fingerprint. `key` is the public key in OpenSSH format, to be saved
    /// if the user trusts it.
    HostKeyUnknown {
        algorithm: String,
        fingerprint: String,
        key: String,
    },
    Auth(String),
    Ssh(russh::Error),
    Keys(russh::keys::Error),
    Io(std::io::Error),
    Config(String),
}

impl fmt::Display for TunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunnelError::HostKeyChanged { source } => write!(
                f,
                "the SSH server key has CHANGED compared to {source}: \
                 possible man-in-the-middle attack, connection refused"
            ),
            TunnelError::HostKeyRevoked { line } => write!(
                f,
                "the SSH server key is REVOKED in known_hosts (line {line}): \
                 connection refused"
            ),
            TunnelError::HostKeyUnknown {
                algorithm,
                fingerprint,
                ..
            } => write!(
                f,
                "never-seen SSH server: fingerprint {fingerprint} ({algorithm}) to be confirmed"
            ),
            TunnelError::Auth(m) => write!(f, "SSH authentication failed: {m}"),
            TunnelError::Ssh(e) => write!(f, "SSH: {e}"),
            TunnelError::Keys(e) => write!(f, "SSH key: {e}"),
            TunnelError::Io(e) => write!(f, "SSH tunnel: {e}"),
            TunnelError::Config(m) => write!(f, "SSH tunnel: {m}"),
        }
    }
}

impl std::error::Error for TunnelError {}

impl From<russh::Error> for TunnelError {
    fn from(e: russh::Error) -> Self {
        TunnelError::Ssh(e)
    }
}

impl From<russh::keys::Error> for TunnelError {
    fn from(e: russh::keys::Error) -> Self {
        TunnelError::Keys(e)
    }
}

impl From<std::io::Error> for TunnelError {
    fn from(e: std::io::Error) -> Self {
        TunnelError::Io(e)
    }
}

/// How to authenticate to the bastion. Credentials come from the caller
/// (dialog or keychain, §12): this crate touches neither disk nor keychain,
/// except for reading the private key file.
#[derive(Clone)]
pub enum TunnelAuth {
    /// System SSH agent (`SSH_AUTH_SOCK`): tries every identity.
    Agent,
    Password(String),
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

/// The bastion: where it is, as whom to log in, how to trust its key.
/// Credentials come from the caller (dialog or keychain, §12).
#[derive(Clone)]
pub struct SshEndpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: TunnelAuth,
    /// `known_hosts` file for host key verification. `None` =
    /// `~/.ssh/known_hosts`, the one the app uses; an explicit path is for
    /// tests, which must not depend on the user's file.
    pub known_hosts: Option<PathBuf>,
    /// Bastion key confirmed by the user in Alamo (OpenSSH format,
    /// `ssh-ed25519 AAAA…`), saved with the SSH profile. Worth as much as a
    /// `known_hosts` line.
    pub trusted_key: Option<String>,
}

#[derive(Clone)]
pub struct TunnelConfig {
    pub ssh: SshEndpoint,
    /// The database as the BASTION sees it: name resolution happens on the
    /// other side of the tunnel.
    pub target_host: String,
    pub target_port: u16,
}

/// Fingerprint (`SHA256:…`, as `ssh-keygen -l`) of a key in OpenSSH format;
/// `None` if the text is not a key. For showing the saved bastion key.
pub fn fingerprint_of(openssh: &str) -> Option<String> {
    ssh_key::PublicKey::from_openssh(openssh)
        .ok()
        .map(|k| host_key::fingerprint(&k))
}

/// "Test tunnel": connects to the bastion, checks its key, authenticates and
/// disconnects. Says whether the bastion accepts us, without forwarding
/// anything.
pub async fn probe(ssh: &SshEndpoint) -> Result<(), TunnelError> {
    let handle = session(ssh).await?;
    // Best-effort goodbye: the handshake already answered the question.
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(())
}

/// Connection to the bastion, host key verified, user authenticated: the
/// part of the tunnel that can fail for reasons the user can act on.
async fn session(ssh: &SshEndpoint) -> Result<Handle<HostCheck>, TunnelError> {
    let config = Arc::new(client::Config {
        // A tunnel stays up for hours with long silences: the keepalive
        // prevents NAT/firewall from dropping the idle connection.
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    // Read BEFORE connecting: an unreadable known_hosts or a broken
    // saved key stop everything, never "no known host".
    let known_hosts = match ssh.known_hosts.clone().or_else(default_known_hosts) {
        Some(path) => host_key::read_known_hosts(&path).map_err(TunnelError::Config)?,
        None => String::new(),
    };
    let pinned = ssh
        .trusted_key
        .as_deref()
        .map(ssh_key::PublicKey::from_openssh)
        .transpose()
        .map_err(|e| TunnelError::Config(format!("invalid saved fingerprint: {e}")))?;
    let checker = HostCheck {
        host: ssh.host.clone(),
        port: ssh.port,
        known_hosts,
        pinned,
    };
    let mut handle = client::connect(config, (ssh.host.as_str(), ssh.port), checker).await?;
    authenticate(&mut handle, &ssh.user, &ssh.auth).await?;
    Ok(handle)
}

/// Active tunnel. Lives as long as the database session that uses it: on
/// drop the listener closes and the SSH connection goes down with it.
pub struct Tunnel {
    /// Listener port on 127.0.0.1: this is where the connection string points.
    pub local_port: u16,
    accept_task: tokio::task::JoinHandle<()>,
    /// Keeps the SSH connection alive: the channels opened by the accept
    /// task die if the handle is dropped.
    _handle: Arc<Handle<HostCheck>>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl Tunnel {
    pub async fn open(cfg: TunnelConfig) -> Result<Tunnel, TunnelError> {
        Self::open_at(cfg, None).await
    }

    /// Has the SSH connection dropped (network, Mac asleep)? The local
    /// listener would still accept, but no channel would open: whoever uses
    /// it must rebuild the tunnel, not just the database connection.
    pub fn is_closed(&self) -> bool {
        self._handle.is_closed() || self.accept_task.is_finished()
    }

    /// Like `open`; with `local_port` the listener is created on THAT port —
    /// needed to rebuild a dropped tunnel without touching the session's
    /// connection string, which contains it. The old tunnel must be dropped
    /// first: its listener closes with the task abort, which is
    /// asynchronous, so the bind is retried for a while.
    pub async fn open_at(
        cfg: TunnelConfig,
        local_port: Option<u16>,
    ) -> Result<Tunnel, TunnelError> {
        let handle = session(&cfg.ssh).await?;

        // Random port on loopback: no conflicts, no exposure outside the
        // machine.
        let listener = match local_port {
            None => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
            Some(port) => bind_retry(port).await?,
        };
        let local_port = listener.local_addr()?.port();
        let handle = Arc::new(handle);
        let accept_handle = Arc::clone(&handle);
        let (target_host, target_port) = (cfg.target_host.clone(), u32::from(cfg.target_port));
        let accept_task = tokio::spawn(async move {
            // The forwarded connections live here: when the tunnel is dropped
            // (abort of this task) the JoinSet closes them all. On an SSH that
            // died silently they would hang until the keepalive notices
            // (minutes), and with them the running queries and opens.
            let mut conns = tokio::task::JoinSet::new();
            loop {
                let Ok((mut sock, peer)) = listener.accept().await else {
                    // Listener dead: the tunnel is over, no point looping.
                    break;
                };
                while conns.try_join_next().is_some() {}
                let h = Arc::clone(&accept_handle);
                let (th, tp) = (target_host.clone(), target_port);
                conns.spawn(async move {
                    // Channel refused (db down as seen from the bastion,
                    // policy): the socket is closed and the driver sees
                    // "connection reset", its normal connection error.
                    let Ok(channel) = h
                        .channel_open_direct_tcpip(th, tp, "127.0.0.1", u32::from(peer.port()))
                        .await
                    else {
                        return;
                    };
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
                });
            }
        });
        Ok(Tunnel {
            local_port,
            accept_task,
            _handle: handle,
        })
    }
}

async fn bind_retry(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let mut last = None;
    for _ in 0..20 {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    Err(last.expect("at least one attempt"))
}

async fn authenticate(
    handle: &mut Handle<HostCheck>,
    user: &str,
    auth: &TunnelAuth,
) -> Result<(), TunnelError> {
    let result = match auth {
        TunnelAuth::Password(p) => handle.authenticate_password(user, p.clone()).await?,
        TunnelAuth::Key { path, passphrase } => {
            let key = load_secret_key(path, passphrase.as_deref())?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        }
        TunnelAuth::Agent => {
            // macOS/Linux: the SSH_AUTH_SOCK socket. Windows: the OpenSSH
            // agent (ssh-agent service) on its named pipe.
            #[cfg(unix)]
            let agent = AgentClient::connect_env().await;
            #[cfg(windows)]
            let agent = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await;
            let mut agent =
                agent.map_err(|e| TunnelError::Auth(format!("SSH agent not reachable: {e}")))?;
            return authenticate_with_agent(handle, user, &mut agent).await;
        }
    };
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure { .. } => Err(TunnelError::Auth(
            "credentials rejected by the server".into(),
        )),
    }
}

/// Tries the agent's identities one at a time, keys and certificates.
async fn authenticate_with_agent<S>(
    handle: &mut Handle<HostCheck>,
    user: &str,
    agent: &mut AgentClient<S>,
) -> Result<(), TunnelError>
where
    S: AgentStream + Unpin + Send + 'static,
    AgentClient<S>: russh::Signer,
    <AgentClient<S> as russh::Signer>::Error: fmt::Display,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| TunnelError::Auth(format!("SSH agent: {e}")))?;
    if identities.is_empty() {
        return Err(TunnelError::Auth(
            "the SSH agent has no identities loaded (ssh-add)".into(),
        ));
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    for identity in identities {
        let res = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                handle
                    .authenticate_publickey_with(user, key, hash, agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                handle
                    .authenticate_certificate_with(user, certificate, hash, agent)
                    .await
            }
        }
        .map_err(|e| TunnelError::Auth(format!("SSH agent: {e}")))?;
        if matches!(res, AuthResult::Success) {
            return Ok(());
        }
    }
    Err(TunnelError::Auth(
        "no agent identity accepted by the server".into(),
    ))
}

/// `~/.ssh/known_hosts`, if there is a home.
fn default_known_hosts() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".ssh").join("known_hosts"))
}

/// Host key verification (policy in `host_key`).
struct HostCheck {
    host: String,
    port: u16,
    /// Contents of known_hosts, read on open.
    known_hosts: String,
    pinned: Option<ssh_key::PublicKey>,
}

impl client::Handler for HostCheck {
    type Error = TunnelError;

    /// A server certificate counts for its key: the CA signature is not
    /// verified (`@cert-authority` not supported), the key is compared as if
    /// it were bare. It is the key the server has just signed the key
    /// exchange with, so the comparison stays valid.
    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = presented.public_key();
        let server_public_key = &key;
        let verdict = host_key::verify(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts,
            self.pinned.as_ref(),
        )
        .map_err(TunnelError::Config)?;
        match verdict {
            Verdict::Trusted => Ok(true),
            Verdict::Changed { source } => Err(TunnelError::HostKeyChanged { source }),
            Verdict::Revoked { line } => Err(TunnelError::HostKeyRevoked { line }),
            Verdict::Unknown => Err(TunnelError::HostKeyUnknown {
                algorithm: server_public_key.algorithm().as_str().to_owned(),
                fingerprint: host_key::fingerprint(server_public_key),
                key: server_public_key
                    .to_openssh()
                    .map_err(|e| TunnelError::Config(format!("server key: {e}")))?,
            }),
        }
    }
}
