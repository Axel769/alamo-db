//! Tunnel tests against an **in-process** SSH server (russh server side):
//! no external services, yet the round trip is still a real SSH handshake,
//! auth included, and a real direct-tcpip channel to a local TCP echo.

use std::sync::Arc;

use alamo_tunnel::{probe, SshEndpoint, Tunnel, TunnelAuth, TunnelConfig, TunnelError};
use russh::server::{self, Auth, Msg, Session};
use russh::Channel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const USER: &str = "user";
const PASSWORD: &str = "secret";

/// TCP echo on a random port: the "database" on the other side of the tunnel.
async fn spawn_echo() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

struct SrvHandler;

impl server::Handler for SrvHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    /// Any key of the user: russh has already verified the signature; the
    /// tests care that the client can load the key and sign.
    async fn auth_publickey(
        &mut self,
        user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(if user == USER {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let host = host_to_connect.to_owned();
        let port = u16::try_from(port_to_connect).unwrap();
        reply.accept().await;
        tokio::spawn(async move {
            if let Ok(mut sock) = tokio::net::TcpStream::connect((host.as_str(), port)).await {
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
            }
        });
        Ok(())
    }
}

struct Srv;

impl server::Server for Srv {
    type Handler = SrvHandler;
    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> SrvHandler {
        SrvHandler
    }
}

/// In-process SSH server on a random port.
/// In-process SSH server; also returns its public host key, for
/// writing test known_hosts files.
async fn spawn_sshd() -> (u16, russh::keys::PublicKey) {
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let public = key.public_key().clone();
    let config = Arc::new(server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut srv = Srv;
        let _ = server::Server::run_on_socket(&mut srv, config, &listener).await;
    });
    (port, public)
}

/// Temporary known_hosts, unique per test: never the user's file.
/// `None` as content = missing file (host never seen).
fn known_hosts_file(name: &str, content: Option<String>) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "alamo-tunnel-{}-{name}-known_hosts",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    if let Some(c) = content {
        std::fs::write(&path, c).unwrap();
    }
    path
}

fn known_hosts_line(port: u16, key: &russh::keys::PublicKey) -> String {
    format!("[127.0.0.1]:{port} {}\n", key.to_openssh().unwrap())
}

#[tokio::test]
async fn tunnel_carries_bytes_back_and_forth() {
    let echo_port = spawn_echo().await;
    let (ssh_port, key) = spawn_sshd().await;
    let tunnel = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(known_hosts_file("echo", None)),
            trusted_key: Some(key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    })
    .await
    .expect("opening tunnel");

    // Two concurrent connections on the listener: each accept = one channel.
    for _ in 0..2 {
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port))
            .await
            .expect("connection to the tunnel listener");
        sock.write_all(b"through the tunnel").await.unwrap();
        let mut buf = [0u8; 64];
        let n = sock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"through the tunnel");
    }
}

/// RSA key in PEM PKCS#1 (`BEGIN RSA PRIVATE KEY`), the format of AWS
/// `.pem` files: with russh 0.64 it needs its `rsa` feature, without which
/// opening failed with "Unsupported key type RSA".
#[tokio::test]
async fn rsa_pem_pkcs1_key_authenticates() {
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};

    let echo_port = spawn_echo().await;
    let (ssh_port, host_key) = spawn_sshd().await;
    let client_key = russh::keys::PrivateKey::random(
        &mut rand::rng(),
        russh::keys::Algorithm::Rsa { hash: None },
    )
    .unwrap();
    let rsa_key = rsa::RsaPrivateKey::try_from(client_key.key_data().rsa().unwrap()).unwrap();
    let pem = rsa_key.to_pkcs1_pem(LineEnding::LF).unwrap();
    assert!(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    let key_path =
        std::env::temp_dir().join(format!("alamo-tunnel-{}-rsa.pem", std::process::id()));
    std::fs::write(&key_path, pem.as_bytes()).unwrap();

    let tunnel = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Key {
                path: key_path.clone(),
                passphrase: None,
            },
            known_hosts: Some(known_hosts_file("rsa", None)),
            trusted_key: Some(host_key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    })
    .await;
    let _ = std::fs::remove_file(&key_path);
    let tunnel = tunnel.expect("opening tunnel with RSA PEM key");

    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port))
        .await
        .unwrap();
    sock.write_all(b"via rsa key").await.unwrap();
    let mut buf = [0u8; 64];
    let n = sock.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"via rsa key");
}

#[tokio::test]
async fn wrong_credentials_auth_error() {
    let (ssh_port, key) = spawn_sshd().await;
    let err = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password("wrong".into()),
            known_hosts: Some(known_hosts_file("auth", None)),
            trusted_key: Some(key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: 1,
    })
    .await
    .err()
    .expect("auth should have failed");
    assert!(matches!(err, TunnelError::Auth(_)), "{err:?}");
}

/// Dropping the tunnel closes the listener: new connections refused.
#[tokio::test]
async fn drop_closes_the_listener() {
    let echo_port = spawn_echo().await;
    let (ssh_port, key) = spawn_sshd().await;
    let tunnel = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(known_hosts_file("echo", None)),
            trusted_key: Some(key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    })
    .await
    .expect("opening tunnel");
    let port = tunnel.local_port;
    drop(tunnel);
    // The accept task dies with the abort: a few runtime ticks at most.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "listener still alive after the drop"
    );
}

/// The drop also closes connections already forwarded: on an SSH that died
/// silently they would hang (queries, opens) until the keepalive notices.
/// The client sees the close, not an endless wait.
#[tokio::test]
async fn drop_closes_forwarded_connections() {
    let echo_port = spawn_echo().await;
    let (ssh_port, key) = spawn_sshd().await;
    let tunnel = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(known_hosts_file("forwarded", None)),
            trusted_key: Some(key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    })
    .await
    .expect("opening tunnel");
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port))
        .await
        .expect("connection to the tunnel");
    sock.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 16];
    let n = sock.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello");
    drop(tunnel);
    let read = tokio::time::timeout(std::time::Duration::from_secs(2), sock.read(&mut buf))
        .await
        .expect("forwarded connection still open after the tunnel drop");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
}

/// Tunnel dropped and rebuilt on the SAME port (`open_at`): the session's
/// connection string does not change and bytes flow again. The bind
/// succeeds even right after the drop, while the old listener is still
/// closing.
#[tokio::test]
async fn reopened_on_the_same_port_after_drop() {
    let echo_port = spawn_echo().await;
    let (ssh_port, key) = spawn_sshd().await;
    let cfg = TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(known_hosts_file("echo", None)),
            trusted_key: Some(key.to_openssh().unwrap()),
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    };
    let tunnel = Tunnel::open(cfg.clone()).await.expect("opening tunnel");
    assert!(!tunnel.is_closed());
    let port = tunnel.local_port;
    drop(tunnel);
    let again = Tunnel::open_at(cfg, Some(port))
        .await
        .expect("reopening on the same port");
    assert_eq!(again.local_port, port);
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connection to the rebuilt tunnel");
    sock.write_all(b"again").await.unwrap();
    let mut buf = [0u8; 16];
    let n = sock.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"again");
}

/// Host key registered and equal: the tunnel opens.
#[tokio::test]
async fn registered_matching_host_key_passes() {
    let echo_port = spawn_echo().await;
    let (ssh_port, key) = spawn_sshd().await;
    let kh = known_hosts_file("same", Some(known_hosts_line(ssh_port, &key)));
    let tunnel = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(kh.clone()),
            trusted_key: None,
        },
        target_host: "127.0.0.1".into(),
        target_port: echo_port,
    })
    .await;
    let _ = std::fs::remove_file(&kh);
    tunnel.expect("host key equal to known_hosts: should have opened");
}

/// Host key different from the registered one: possible MITM, hard refusal
/// before authentication (also validated by hand against a real bastion).
#[tokio::test]
async fn changed_host_key_refused() {
    let (ssh_port, _) = spawn_sshd().await;
    let other =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let kh = known_hosts_file(
        "changed",
        Some(known_hosts_line(ssh_port, other.public_key())),
    );
    let err = Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(kh.clone()),
            trusted_key: None,
        },
        target_host: "127.0.0.1".into(),
        target_port: 1,
    })
    .await
    .err();
    let _ = std::fs::remove_file(&kh);
    let err = err.expect("changed key: should have refused");
    assert!(
        matches!(&err, TunnelError::HostKeyChanged { source } if source == "known_hosts line 1"),
        "{err:?}"
    );
}

/// Open with a given known_hosts and saved fingerprint; no echo
/// behind: these tests stop at the host key check.
async fn open_with(
    ssh_port: u16,
    known_hosts: std::path::PathBuf,
    trusted_key: Option<String>,
) -> Result<Tunnel, TunnelError> {
    Tunnel::open(TunnelConfig {
        ssh: SshEndpoint {
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: USER.into(),
            auth: TunnelAuth::Password(PASSWORD.into()),
            known_hosts: Some(known_hosts),
            trusted_key,
        },
        target_host: "127.0.0.1".into(),
        target_port: 1,
    })
    .await
}

/// Server never seen: no connection, but the fingerprint to ask the user
/// for confirmation — the same one `ssh-keygen -l` would print.
#[tokio::test]
async fn never_seen_host_refused_with_fingerprint() {
    let (ssh_port, key) = spawn_sshd().await;
    let err = open_with(ssh_port, known_hosts_file("never-seen", None), None)
        .await
        .err()
        .expect("host never seen: should have refused");
    let TunnelError::HostKeyUnknown {
        algorithm,
        fingerprint,
        key: offered,
    } = err
    else {
        panic!("expected HostKeyUnknown: {err:?}");
    };
    assert_eq!(algorithm, "ssh-ed25519");
    assert_eq!(
        fingerprint,
        key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
    );
    assert_eq!(offered, key.to_openssh().unwrap());
    // Confirmed: the same key opens the tunnel.
    open_with(
        ssh_port,
        known_hosts_file("never-seen", None),
        Some(offered),
    )
    .await
    .expect("fingerprint confirmed: should have opened");
}

/// The review bypass (T9): known_hosts has a key of ANOTHER algorithm
/// for this host — it used to pass as "never seen".
#[tokio::test]
async fn key_of_other_algorithm_refused() {
    let (ssh_port, _) = spawn_sshd().await;
    let ecdsa = russh::keys::PrivateKey::random(
        &mut rand::rng(),
        russh::keys::Algorithm::Ecdsa {
            curve: russh::keys::EcdsaCurve::NistP256,
        },
    )
    .unwrap();
    let kh = known_hosts_file(
        "other-algo",
        Some(known_hosts_line(ssh_port, ecdsa.public_key())),
    );
    let err = open_with(ssh_port, kh.clone(), None).await.err();
    let _ = std::fs::remove_file(&kh);
    let err = err.expect("other algorithm: should have refused");
    assert!(matches!(err, TunnelError::HostKeyChanged { .. }), "{err:?}");
}

/// Unreadable known_hosts: refusal, never "no known hosts".
#[tokio::test]
async fn unreadable_known_hosts_refuses() {
    let (ssh_port, key) = spawn_sshd().await;
    // A directory instead of the file: read fails with a real error.
    let err = open_with(
        ssh_port,
        std::env::temp_dir(),
        Some(key.to_openssh().unwrap()),
    )
    .await
    .err()
    .expect("unreadable known_hosts: should have refused");
    assert!(matches!(err, TunnelError::Config(_)), "{err:?}");
}

/// Fingerprint saved in Alamo different from the server's: refusal.
#[tokio::test]
async fn different_saved_fingerprint_refused() {
    let (ssh_port, _) = spawn_sshd().await;
    let other =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let err = open_with(
        ssh_port,
        known_hosts_file("pin-different", None),
        Some(other.public_key().to_openssh().unwrap()),
    )
    .await
    .err()
    .expect("different fingerprint: should have refused");
    assert!(
        matches!(&err, TunnelError::HostKeyChanged { source } if source.contains("Alamo")),
        "{err:?}"
    );
}

/// "Test tunnel": the bastion accepts us, nothing is forwarded, and the
/// same answers as the tunnel when it does not (credentials, host key).
#[tokio::test]
async fn probe_answers_like_the_tunnel() {
    let (ssh_port, key) = spawn_sshd().await;
    let endpoint = |auth: TunnelAuth, trusted_key: Option<String>| SshEndpoint {
        host: "127.0.0.1".into(),
        port: ssh_port,
        user: USER.into(),
        auth,
        known_hosts: Some(known_hosts_file("probe", None)),
        trusted_key,
    };
    let trusted = Some(key.to_openssh().unwrap());
    probe(&endpoint(
        TunnelAuth::Password(PASSWORD.into()),
        trusted.clone(),
    ))
    .await
    .expect("probe");
    let err = probe(&endpoint(TunnelAuth::Password("wrong".into()), trusted))
        .await
        .expect_err("auth should have failed");
    assert!(matches!(err, TunnelError::Auth(_)), "{err:?}");
    let err = probe(&endpoint(TunnelAuth::Password(PASSWORD.into()), None))
        .await
        .expect_err("never-seen host should be refused");
    assert!(matches!(err, TunnelError::HostKeyUnknown { .. }), "{err:?}");
}

#[test]
fn fingerprint_of_saved_key() {
    let key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let public = key.public_key();
    let openssh = public.to_openssh().unwrap();
    assert_eq!(
        alamo_tunnel::fingerprint_of(&openssh).as_deref(),
        Some(
            public
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string()
                .as_str()
        )
    );
    assert_eq!(alamo_tunnel::fingerprint_of("not a key"), None);
}
