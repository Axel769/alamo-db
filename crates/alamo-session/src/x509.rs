//! Human-readable summary of X.509 certificates (§12.2): the server's, whose
//! DER the PG driver captures at the handshake, and the profile's root CA,
//! a PEM file the connection dialog inspects before anyone connects.
//! `x509-cert` is already in the tree through tokio-postgres-rustls; the
//! PEM reader is rustls' own, as in the PG driver.

use std::path::Path;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;
use x509_cert::der::Decode;
use x509_cert::Certificate;

use alamo_core::tls::CertSummary;

/// Subject, issuer and validity of a DER certificate.
pub fn summarize_der(der: &[u8]) -> Result<CertSummary, String> {
    let cert = Certificate::from_der(der).map_err(|e| format!("certificate: {e}"))?;
    let tbs = cert.tbs_certificate;
    Ok(CertSummary {
        subject: Some(tbs.subject.to_string()),
        issuer: Some(tbs.issuer.to_string()),
        not_before: Some(unix_seconds(tbs.validity.not_before)),
        not_after: Some(unix_seconds(tbs.validity.not_after)),
    })
}

fn unix_seconds(t: x509_cert::time::Time) -> i64 {
    i64::try_from(t.to_unix_duration().as_secs()).unwrap_or(i64::MAX)
}

/// The first certificate of a PEM file: what a profile's root CA field
/// points at. Errors name the file and say what is wrong with it, in the
/// words the dialog shows.
pub fn inspect_pem_file(path: &Path) -> Result<CertSummary, String> {
    let pem = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("root CA {}: file not found", path.display()));
        }
        Err(e) => return Err(format!("root CA {}: not readable: {e}", path.display())),
    };
    match CertificateDer::pem_slice_iter(&pem).next() {
        None => Err(format!(
            "root CA {}: no PEM certificate in the file",
            path.display()
        )),
        Some(Err(e)) => Err(format!("root CA {}: {e}", path.display())),
        Some(Ok(der)) => {
            summarize_der(&der).map_err(|e| format!("root CA {}: {e}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CA of the TLS lab (`cargo xtask tls-lab`), kept as a fixture.
    const LAB_CA: &str = include_str!("../tests/fixtures/lab-ca.pem");

    fn temp_file(name: &str, body: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("alamo-x509-{}-{name}", std::process::id()));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn pem_file_summary() {
        let path = temp_file("ca.pem", LAB_CA.as_bytes());
        let s = inspect_pem_file(&path).unwrap();
        assert!(
            s.subject.as_deref().unwrap().contains("CN=Alamo Test CA"),
            "{s:?}"
        );
        assert_eq!(s.issuer, s.subject, "self-signed");
        let (from, to) = (s.not_before.unwrap(), s.not_after.unwrap());
        assert!(to - from > 9 * 365 * 86_400, "ten-year CA: {from}..{to}");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_empty_and_garbage_files() {
        let missing = std::env::temp_dir().join("alamo-x509-missing.pem");
        let e = inspect_pem_file(&missing).unwrap_err();
        assert!(e.ends_with("file not found"), "{e}");

        let empty = temp_file("empty.pem", b"");
        let e = inspect_pem_file(&empty).unwrap_err();
        assert!(e.ends_with("no PEM certificate in the file"), "{e}");
        std::fs::remove_file(empty).unwrap();

        let garbage = temp_file("garbage.pem", b"not a certificate at all\n");
        let e = inspect_pem_file(&garbage).unwrap_err();
        assert!(e.ends_with("no PEM certificate in the file"), "{e}");
        std::fs::remove_file(garbage).unwrap();

        let key_only = temp_file(
            "key.pem",
            b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        );
        let e = inspect_pem_file(&key_only).unwrap_err();
        assert!(e.ends_with("no PEM certificate in the file"), "{e}");
        std::fs::remove_file(key_only).unwrap();
    }

    #[test]
    fn broken_der_is_an_error() {
        assert!(summarize_der(&[0x30, 0x03, 0x02, 0x01, 0x07]).is_err());
    }
}
