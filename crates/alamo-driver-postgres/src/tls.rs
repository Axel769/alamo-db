//! Construction of the TLS connector (§12): from `TlsOptions` to
//! `MakeRustlsConnect`. The semantics of the modes are documented in
//! `alamo_core::tls`; only the translation into a rustls config lives here.
//!
//! Crypto provider: `ring`, passed **explicitly** to every builder — never
//! the process default, which is global state and panics if two
//! dependencies install a different one.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use tokio_postgres_rustls::MakeRustlsConnect;

use alamo_core::error::DbError;
use alamo_core::tls::{TlsMode, TlsOptions};

use crate::PgError;

/// `None` = mode `disable`: connects with `NoTls`.
///
/// Errors are possible only in the verifying modes: `verify-ca` without a
/// profile CA, unreadable CA or one without certificates, empty system root
/// store. They fail the connect, never a silent degradation to unverified.
pub fn make_connector(opts: &TlsOptions) -> Result<Option<MakeRustlsConnect>, PgError> {
    opts.check().map_err(config_err)?;
    if !opts.mode.wants_tls() {
        return Ok(None);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| config_err(format!("TLS configuration: {e}")))?;
    let config = if opts.mode.verifies_chain() {
        let roots = Arc::new(root_store(opts.root_cert.as_deref())?);
        let webpki = WebPkiServerVerifier::builder_with_provider(roots, provider)
            .build()
            .map_err(|e| config_err(format!("TLS certificate verification: {e}")))?;
        match opts.mode {
            TlsMode::VerifyFull => builder
                .dangerous()
                .with_custom_certificate_verifier(webpki)
                .with_no_client_auth(),
            _ => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoHostname(webpki)))
                .with_no_client_auth(),
        }
    } else {
        // prefer/require: encrypted channel, no server authentication
        // (libpq semantics, see alamo_core::tls).
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth()
    };
    Ok(Some(MakeRustlsConnect::new(config)))
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
        };
        assert!(make_connector(&opts).unwrap().is_some());
    }

    #[test]
    fn verify_ca_without_ca_fails() {
        let opts = TlsOptions {
            mode: TlsMode::VerifyCa,
            root_cert: None,
            tls_server_name: None,
        };
        assert!(make_connector(&opts).is_err());
    }

    #[test]
    fn verify_full_missing_ca_fails() {
        let opts = TlsOptions {
            mode: TlsMode::VerifyFull,
            root_cert: Some("/nonexistent/path.pem".into()),
            tls_server_name: None,
        };
        assert!(make_connector(&opts).is_err());
    }
}
