//! Construction of the TLS connector (§12): from `TlsOptions` to
//! `MakeRustlsConnect`. The semantics of the modes are documented in
//! `alamo_core::tls`; only the translation into a rustls config lives here.
//!
//! The connector also keeps the certificate the server presented (§12.2):
//! tokio-postgres hides the rustls session, but every mode goes through a
//! verifier of ours, which sees the certificate before deciding.
//!
//! Crypto provider: `ring`, passed **explicitly** to every builder — never
//! the process default, which is global state and panics if two
//! dependencies install a different one.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use tokio_postgres_rustls::MakeRustlsConnect;

use alamo_core::error::DbError;
use alamo_core::tls::{ClientIdentity, TlsMode, TlsOptions};

use crate::PgError;

/// The certificate the server presented at the last handshake made with
/// this connector (DER). The cancel/terminate connections reuse the
/// connector and talk to the same server: their overwrite changes nothing.
type PeerCertSlot = Arc<Mutex<Option<Vec<u8>>>>;

/// The session's connector plus what its handshakes revealed (§12.2).
#[derive(Clone)]
pub struct Connector {
    pub make: MakeRustlsConnect,
    mode: TlsMode,
    peer_cert: PeerCertSlot,
}

impl Connector {
    /// `None` = no handshake done yet with this connector.
    pub fn server_cert_der(&self) -> Option<Vec<u8>> {
        self.peer_cert
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The mode verified the chain: the certificate above is trusted, not
    /// just seen.
    pub fn verifies_chain(&self) -> bool {
        self.mode.verifies_chain()
    }
}

/// `None` = mode `disable`: connects with `NoTls`.
///
/// Errors are possible only in the verifying modes: `verify-ca` without a
/// profile CA, unreadable CA or one without certificates, empty system root
/// store. They fail the connect, never a silent degradation to unverified.
pub fn make_connector(opts: &TlsOptions) -> Result<Option<Connector>, PgError> {
    opts.check().map_err(config_err)?;
    if !opts.mode.wants_tls() {
        return Ok(None);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| config_err(format!("TLS configuration: {e}")))?;
    let verifier: Arc<dyn ServerCertVerifier> = if opts.mode.verifies_chain() {
        let roots = Arc::new(root_store(opts.root_cert.as_deref())?);
        let webpki = WebPkiServerVerifier::builder_with_provider(roots, provider)
            .build()
            .map_err(|e| config_err(format!("TLS certificate verification: {e}")))?;
        match opts.mode {
            TlsMode::VerifyFull => webpki,
            _ => Arc::new(NoHostname(webpki)),
        }
    } else {
        // prefer/require: encrypted channel, no server authentication
        // (libpq semantics, see alamo_core::tls).
        Arc::new(NoVerify(provider))
    };
    let peer_cert = PeerCertSlot::default();
    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Capture {
            inner: verifier,
            slot: Arc::clone(&peer_cert),
        }));
    let config = match &opts.client_cert {
        None => builder.with_no_client_auth(),
        Some(id) => {
            let (chain, key) = client_identity(id)?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| config_err(format!("client certificate: {e}")))?
        }
    };
    Ok(Some(Connector {
        make: MakeRustlsConnect::new(config),
        mode: opts.mode,
        peer_cert,
    }))
}

/// The client certificate as rustls wants it. The PEM was already checked
/// when the profile's files were read: an error here means bytes corrupted
/// in between, still said and never skipped.
fn client_identity(
    id: &ClientIdentity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), PgError> {
    let chain = CertificateDer::pem_slice_iter(&id.cert_chain_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| config_err(format!("client certificate: {e}")))?;
    if chain.is_empty() {
        return Err(config_err("client certificate: no certificate".into()));
    }
    let key = PrivateKeyDer::from_pem_slice(&id.key_pem)
        .map_err(|e| config_err(format!("client certificate key: {e}")))?;
    Ok((chain, key))
}

/// The profile CA (PEM) if present, ONLY that one: a server with an internal
/// CA must not pass through a coincidence with the public roots. Otherwise
/// the system roots.
fn root_store(custom_ca: Option<&std::path::Path>) -> Result<RootCertStore, PgError> {
    let mut roots = RootCertStore::empty();
    if let Some(path) = custom_ca {
        let pem = std::fs::read(path)
            .map_err(|e| config_err(format!("root CA {}: {e}", path.display())))?;
        let mut added = 0usize;
        // PEM with rustls-pki-types: rustls-pemfile is no longer maintained
        // (RUSTSEC-2025-0134).
        for cert in CertificateDer::pem_slice_iter(&pem) {
            let cert = cert.map_err(|e| config_err(format!("root CA {}: {e}", path.display())))?;
            roots
                .add(cert)
                .map_err(|e| config_err(format!("root CA {}: {e}", path.display())))?;
            added += 1;
        }
        if added == 0 {
            return Err(config_err(format!(
                "root CA {}: no certificate in the file",
                path.display()
            )));
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        roots.add_parsable_certificates(native.certs);
    }
    if roots.is_empty() {
        return Err(config_err(
            "no trust root: empty system root store and no custom CA".into(),
        ));
    }
    Ok(roots)
}

fn config_err(msg: String) -> PgError {
    PgError::Db(Box::new(DbError::internal(msg)))
}

/// Keeps the server certificate, then lets the mode's verifier decide: the
/// capture is what the UI shows (§12.2), the decision stays the inner one's.
#[derive(Debug)]
struct Capture {
    inner: Arc<dyn ServerCertVerifier>,
    slot: PeerCertSlot,
}

impl ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(end_entity.to_vec());
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Encrypted channel without server authentication (`prefer`/`require`):
/// every certificate passes. The handshake signatures remain verified — it
/// is not optional in the protocol, and it costs nothing.
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `verify-ca`: chain verified by the standard webpki verifier, hostname
/// mismatch tolerated — the only error this wrapper absorbs.
#[derive(Debug)]
struct NoHostname(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for NoHostname {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_no_connector() {
        assert!(make_connector(&TlsOptions::plain()).unwrap().is_none());
    }

    #[test]
    fn require_builds_without_roots() {
        // require does not verify: it must build even without a CA and
        // without touching the system root store.
        let opts = TlsOptions {
            mode: TlsMode::Require,
            root_cert: None,
            tls_server_name: None,
            client_cert: None,
        };
        assert!(make_connector(&opts).unwrap().is_some());
    }

    #[test]
    fn verify_ca_without_ca_fails() {
        let opts = TlsOptions {
            mode: TlsMode::VerifyCa,
            root_cert: None,
            tls_server_name: None,
            client_cert: None,
        };
        assert!(make_connector(&opts).is_err());
    }

    #[test]
    fn verify_full_missing_ca_fails() {
        let opts = TlsOptions {
            mode: TlsMode::VerifyFull,
            root_cert: Some("/nonexistent/path.pem".into()),
            tls_server_name: None,
            client_cert: None,
        };
        assert!(make_connector(&opts).is_err());
    }

    /// The capture happens before the inner verdict and keeps the DER as
    /// presented: what the UI shows is the real certificate, not a copy
    /// re-encoded by us.
    #[test]
    fn capture_keeps_the_presented_certificate() {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let slot = PeerCertSlot::default();
        let capture = Capture {
            inner: Arc::new(NoVerify(provider)),
            slot: Arc::clone(&slot),
        };
        let der = CertificateDer::from(vec![0x30, 0x03, 0x02, 0x01, 0x07]);
        let name = ServerName::try_from("db.example").unwrap();
        capture
            .verify_server_cert(&der, &[], &name, &[], UnixTime::now())
            .expect("NoVerify accepts anything");
        assert_eq!(slot.lock().unwrap().as_deref(), Some(der.as_ref()));
    }
}
