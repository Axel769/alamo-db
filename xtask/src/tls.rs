//! TLS lab (§12, §15.1): a CA and a server certificate generated at every
//! run, so that `verify-ca`/`verify-full` have a real server to succeed
//! against — the plain matrix images speak TLS only with self-signed
//! certificates (MySQL) or not at all (PG). Pure Rust (rcgen): no `openssl`
//! on the host, same result on every OS.

use std::path::{Path, PathBuf};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use time::{Duration, OffsetDateTime};

/// Subject of the generated CA: the tests and the UI checks look for it.
pub const CA_NAME: &str = "Alamo Test CA";

/// The lab's files, written under `dir` and kept in memory for the
/// containers (copied in, never bind-mounted: a bind mount keeps the
/// host's uid and PG refuses a key it does not own).
pub struct TlsFiles {
    pub dir: PathBuf,
    pub ca_pem: String,
    pub server_pem: String,
    pub key_pem: String,
}

impl TlsFiles {
    pub fn ca_path(&self) -> PathBuf {
        self.dir.join("ca.pem")
    }
}

/// CA + server certificate for `127.0.0.1`/`localhost` (the matrix and the
/// lab connect over IPv4 loopback, like the development containers), valid
/// from yesterday for about a year: real dates, so the expiry shown by the
/// app is meaningful. ECDSA P-256: `ring` cannot generate RSA keys, and
/// PG, MySQL 8 and MariaDB all accept EC server certificates.
pub fn generate(dir: &Path) -> Result<TlsFiles, String> {
    let now = OffsetDateTime::now_utc();
    let ca_key = KeyPair::generate().map_err(|e| format!("CA key: {e}"))?;
    let mut ca = CertificateParams::default();
    ca.distinguished_name.push(DnType::CommonName, CA_NAME);
    ca.distinguished_name
        .push(DnType::OrganizationName, "Alamo DB");
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca.not_before = now - Duration::days(1);
    ca.not_after = now + Duration::days(3650);
    let ca_cert = ca
        .self_signed(&ca_key)
        .map_err(|e| format!("CA certificate: {e}"))?;

    let server_key = KeyPair::generate().map_err(|e| format!("server key: {e}"))?;
    let mut server = CertificateParams::new(["localhost".to_string(), "127.0.0.1".to_string()])
        .map_err(|e| format!("server names: {e}"))?;
    server
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    server
        .distinguished_name
        .push(DnType::OrganizationName, "Alamo DB");
    server.not_before = now - Duration::days(1);
    server.not_after = now + Duration::days(398);
    let issuer = Issuer::from_params(&ca, &ca_key);
    let server_cert = server
        .signed_by(&server_key, &issuer)
        .map_err(|e| format!("server certificate: {e}"))?;

    let files = TlsFiles {
        dir: dir.to_path_buf(),
        ca_pem: ca_cert.pem(),
        server_pem: server_cert.pem(),
        key_pem: server_key.serialize_pem(),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, body) in [
        ("ca.pem", &files.ca_pem),
        ("server.pem", &files.server_pem),
        ("server-key.pem", &files.key_pem),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(files)
}
