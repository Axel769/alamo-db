//! The profile's client certificate (mTLS, §12.3): the two PEM files read
//! and checked, the key decrypted with its passphrase if it is encrypted.
//! Done once, before connecting: the drivers receive bytes
//! (`alamo_core::tls::ClientIdentity`), so every lane, cancel and KILL
//! presents the same certificate without reading the disk or the passphrase
//! again.
//!
//! Encrypted keys: PKCS#8 (`ENCRYPTED PRIVATE KEY`, what OpenSSL 3 writes)
//! with PBES2 (PBKDF2-SHA256 or scrypt, AES). The legacy OpenSSL format
//! (`Proc-Type: 4,ENCRYPTED`) and PKCS#12 are refused with the command that
//! converts them: a second decoder for formats OpenSSL itself deprecated is
//! not worth its weight.

use std::fmt;
use std::path::Path;

use pkcs8::der::pem::LineEnding;
use pkcs8::{EncryptedPrivateKeyInfoRef, SecretDocument};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use alamo_core::tls::{CertSummary, ClientIdentity};

use crate::x509::summarize_der;

const ENCRYPTED_BEGIN: &str = "-----BEGIN ENCRYPTED PRIVATE KEY-----";
const ENCRYPTED_END: &str = "-----END ENCRYPTED PRIVATE KEY-----";

/// Why the client certificate cannot be used. The two passphrase cases are
/// apart because the app reacts to them: it asks for the passphrase instead
/// of showing an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCertError {
    /// The key is encrypted and no passphrase was given.
    NeedsPassphrase,
    /// The passphrase does not open the key.
    WrongPassphrase,
    /// Missing or unreadable file, wrong format: the message says which
    /// file and what to do.
    Invalid(String),
}

impl fmt::Display for ClientCertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientCertError::NeedsPassphrase => {
                f.write_str("the client certificate key is encrypted: enter its passphrase")
            }
            ClientCertError::WrongPassphrase => {
                f.write_str("wrong passphrase for the client certificate key")
            }
            ClientCertError::Invalid(m) => f.write_str(m),
        }
    }
}

/// Is the key file encrypted? For the dialog, which shows the passphrase
/// field only when it is. A file that cannot be read says `false`: the
/// error comes from `load`, with its words.
pub fn key_is_encrypted(key: &Path) -> bool {
    std::fs::read(key).is_ok_and(|b| {
        let text = String::from_utf8_lossy(&b);
        text.contains(ENCRYPTED_BEGIN) || text.contains("Proc-Type: 4,ENCRYPTED")
    })
}

/// Reads certificate and key; `passphrase` is used only if the key is
/// encrypted. The two paths may be the same file (certificate and key
/// together).
pub fn load(
    cert: &Path,
    key: &Path,
    passphrase: Option<&str>,
) -> Result<ClientIdentity, ClientCertError> {
    let cert_pem = read("client certificate", cert)?;
    let chain = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(format!("client certificate {}: {e}", cert.display())))?;
    if chain.is_empty() {
        return Err(invalid(format!(
            "client certificate {}: no PEM certificate in the file{}",
            cert.display(),
            pkcs12_hint(&cert_pem)
        )));
    }
    let key_pem = read("client certificate key", key)?;
    let key_pem = decrypt_if_needed(key, key_pem, passphrase)?;
    PrivateKeyDer::from_pem_slice(&key_pem).map_err(|e| {
        invalid(format!(
            "client certificate key {}: no PEM private key in the file ({e}){}",
            key.display(),
            pkcs12_hint(&key_pem)
        ))
    })?;
    Ok(ClientIdentity {
        cert_chain_pem: cert_pem,
        key_pem,
    })
}

/// Subject, issuer and validity of the client certificate, for the dialog:
/// the user sees which identity the profile presents before connecting.
pub fn inspect(cert: &Path) -> Result<CertSummary, String> {
    let pem = read("client certificate", cert).map_err(|e| e.to_string())?;
    match CertificateDer::pem_slice_iter(&pem).next() {
        None => Err(format!(
            "client certificate {}: no PEM certificate in the file{}",
            cert.display(),
            pkcs12_hint(&pem)
        )),
        Some(Err(e)) => Err(format!("client certificate {}: {e}", cert.display())),
        Some(Ok(der)) => {
            summarize_der(&der).map_err(|e| format!("client certificate {}: {e}", cert.display()))
        }
    }
}

fn invalid(msg: String) -> ClientCertError {
    ClientCertError::Invalid(msg)
}

fn read(what: &str, path: &Path) -> Result<Vec<u8>, ClientCertError> {
    std::fs::read(path).map_err(|e| {
        invalid(match e.kind() {
            std::io::ErrorKind::NotFound => format!("{what} {}: file not found", path.display()),
            _ => format!("{what} {}: not readable: {e}", path.display()),
        })
    })
}

/// Binary content where a PEM was expected: most likely a PKCS#12 bundle,
/// which is how Windows and many CAs hand out client certificates.
fn pkcs12_hint(bytes: &[u8]) -> &'static str {
    if std::str::from_utf8(bytes).is_err() {
        ". If it is a .p12/.pfx file, convert it: \
         openssl pkcs12 -in cert.p12 -nokeys -out cert.pem and \
         openssl pkcs12 -in cert.p12 -nocerts -nodes -out key.pem"
    } else {
        ""
    }
}

/// The key as unencrypted PEM: unchanged if it already is, decrypted into
/// PKCS#8 otherwise.
fn decrypt_if_needed(
    path: &Path,
    pem: Vec<u8>,
    passphrase: Option<&str>,
) -> Result<Vec<u8>, ClientCertError> {
    let text = String::from_utf8_lossy(&pem);
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(invalid(format!(
            "client certificate key {}: encrypted in the legacy OpenSSL format, \
             which is not supported. Convert it to PKCS#8: \
             openssl pkcs8 -topk8 -v2 aes-256-cbc -in old.key -out new.key",
            path.display()
        )));
    }
    let Some(block) = encrypted_block(&text) else {
        return Ok(pem);
    };
    let passphrase = passphrase
        .filter(|p| !p.is_empty())
        .ok_or(ClientCertError::NeedsPassphrase)?;
    let bad_file = |e: String| invalid(format!("client certificate key {}: {e}", path.display()));
    let (_, doc) = SecretDocument::from_pem(block).map_err(|e| bad_file(e.to_string()))?;
    let info = EncryptedPrivateKeyInfoRef::try_from(doc.as_bytes())
        .map_err(|e| bad_file(e.to_string()))?;
    let plain = info.decrypt(passphrase).map_err(|e| match e {
        // Decryption failed, or it "succeeded" with the wrong key and the
        // result is not DER: both mean the passphrase is wrong.
        pkcs8::Error::EncryptedPrivateKey(pkcs8::pkcs5::Error::DecryptFailed)
        | pkcs8::Error::Asn1(_) => ClientCertError::WrongPassphrase,
        pkcs8::Error::EncryptedPrivateKey(pkcs8::pkcs5::Error::UnsupportedAlgorithm { .. }) => {
            bad_file(
                "encryption not supported (only PBES2 with AES). Convert it: \
                 openssl pkcs8 -topk8 -v2 aes-256-cbc -in old.key -out new.key"
                    .into(),
            )
        }
        other => bad_file(other.to_string()),
    })?;
    let pem = plain
        .to_pem("PRIVATE KEY", LineEnding::LF)
        .map_err(|e| bad_file(e.to_string()))?;
    Ok(pem.as_bytes().to_vec())
}

/// The `ENCRYPTED PRIVATE KEY` block alone: the file may hold the
/// certificate too, and the PEM decoder wants exactly one block.
fn encrypted_block(text: &str) -> Option<&str> {
    let start = text.find(ENCRYPTED_BEGIN)?;
    let end = text[start..].find(ENCRYPTED_END)? + start + ENCRYPTED_END.len();
    Some(&text[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated once with OpenSSL 3 (fixtures, not secrets):
    //   openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    //     -keyout client-key.pem -out client.pem -subj "/CN=alamo_mtls" -days 3650
    //   openssl pkcs8 -topk8 -v2 aes-256-cbc -passout pass:alamo-pass \
    //     -in client-key.pem -out client-key-enc.pem
    //   openssl pkcs8 -topk8 -scrypt -passout pass:alamo-pass \
    //     -in client-key.pem -out client-key-scrypt.pem
    //   openssl ec -aes256 -passout pass:alamo-pass \
    //     -in client-key.pem -out client-key-legacy.pem
    const CERT: &str = include_str!("../tests/fixtures/client.pem");
    const KEY: &str = include_str!("../tests/fixtures/client-key.pem");
    const KEY_ENC: &str = include_str!("../tests/fixtures/client-key-enc.pem");
    const KEY_SCRYPT: &str = include_str!("../tests/fixtures/client-key-scrypt.pem");
    const KEY_LEGACY: &str = include_str!("../tests/fixtures/client-key-legacy.pem");

    fn temp_file(name: &str, body: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("alamo-client-cert-{}-{name}", std::process::id()));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn der_of(pem: &[u8]) -> Vec<u8> {
        PrivateKeyDer::from_pem_slice(pem)
            .unwrap()
            .secret_der()
            .to_vec()
    }

    #[test]
    fn plain_key_is_passed_through() {
        let (c, k) = (
            temp_file("c1", CERT.as_bytes()),
            temp_file("k1", KEY.as_bytes()),
        );
        let id = load(&c, &k, None).unwrap();
        assert_eq!(id.cert_chain_pem, CERT.as_bytes());
        assert_eq!(id.key_pem, KEY.as_bytes());
        // A passphrase given for a plain key is not an error.
        assert!(load(&c, &k, Some("whatever")).is_ok());
        assert!(!key_is_encrypted(&k));
    }

    #[test]
    fn encrypted_key_is_decrypted_to_the_same_key() {
        let c = temp_file("c2", CERT.as_bytes());
        for (name, body) in [("k2", KEY_ENC), ("k3", KEY_SCRYPT)] {
            let k = temp_file(name, body.as_bytes());
            assert!(key_is_encrypted(&k));
            assert_eq!(load(&c, &k, None), Err(ClientCertError::NeedsPassphrase));
            assert_eq!(
                load(&c, &k, Some("")),
                Err(ClientCertError::NeedsPassphrase)
            );
            assert_eq!(
                load(&c, &k, Some("wrong")),
                Err(ClientCertError::WrongPassphrase)
            );
            let id = load(&c, &k, Some("alamo-pass")).unwrap();
            assert_eq!(der_of(&id.key_pem), der_of(KEY.as_bytes()), "{name}");
        }
    }

    #[test]
    fn key_and_certificate_in_one_file() {
        let both = format!("{CERT}\n{KEY_ENC}");
        let f = temp_file("both", both.as_bytes());
        let id = load(&f, &f, Some("alamo-pass")).unwrap();
        assert_eq!(der_of(&id.key_pem), der_of(KEY.as_bytes()));
    }

    #[test]
    fn unsupported_formats_say_how_to_convert() {
        let c = temp_file("c4", CERT.as_bytes());
        let legacy = temp_file("k4", KEY_LEGACY.as_bytes());
        assert!(key_is_encrypted(&legacy));
        let ClientCertError::Invalid(m) = load(&c, &legacy, Some("alamo-pass")).unwrap_err() else {
            panic!("legacy format must be refused")
        };
        assert!(m.contains("openssl pkcs8 -topk8"), "{m}");

        let p12 = temp_file("p12", &[0x30, 0x82, 0xff, 0x00, 0x9f]);
        let ClientCertError::Invalid(m) = load(&p12, &p12, None).unwrap_err() else {
            panic!("binary file must be refused")
        };
        assert!(m.contains("openssl pkcs12"), "{m}");
    }

    #[test]
    fn missing_files_are_named() {
        let c = temp_file("c5", CERT.as_bytes());
        let none = Path::new("/does/not/exist/key.pem");
        let ClientCertError::Invalid(m) = load(&c, none, None).unwrap_err() else {
            panic!()
        };
        assert!(
            m.contains("key /does/not/exist/key.pem: file not found"),
            "{m}"
        );
        let k = temp_file("k5", KEY.as_bytes());
        let ClientCertError::Invalid(m) = load(&k, &k, None).unwrap_err() else {
            panic!()
        };
        assert!(m.contains("no PEM certificate"), "{m}");
    }

    #[test]
    fn inspect_shows_the_identity() {
        let c = temp_file("c6", CERT.as_bytes());
        let s = inspect(&c).unwrap();
        assert!(
            s.subject.as_deref().unwrap().contains("CN=alamo_mtls"),
            "{s:?}"
        );
    }
}
