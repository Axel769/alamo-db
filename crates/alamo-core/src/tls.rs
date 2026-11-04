//! TLS options of the connection (§12): the type lives here because both
//! drivers consume it; the concrete construction (rustls config, `SslOpts`)
//! lives in the drivers — this crate stays dependency-free.
//!
//! Semantics of the modes (aligned with libpq, explicit by contract):
//!
//! | mode | channel | chain verification | hostname verification |
//! |---|---|---|---|
//! | `disable` | plaintext | — | — |
//! | `prefer` | TLS if the server offers it, otherwise plaintext | no | no |
//! | `require` | TLS mandatory | no | no |
//! | `verify-ca` | TLS mandatory | yes, only with the profile's CA | no |
//! | `verify-full` | TLS mandatory | yes | yes |
//!
//! `verify-ca` does not check the name, so it only makes sense against a
//! specific CA: with the public roots the certificate of any website would
//! pass. Without a profile CA the connection is refused (like libpq with
//! `sslrootcert=system`).
//!
//! ⚠️ `prefer` and `require` **do not authenticate the server**: they protect
//! against passive sniffing, not against an active MITM. The UI says so in
//! the dialog; here it is only worth remembering that it is not a bug, it is
//! libpq's historical semantics.

use std::path::PathBuf;

/// TLS mode, from the profile's `sslmode` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    Disable,
    /// Historical default of the profiles (M1): tries TLS, falls back to plaintext.
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl TlsMode {
    /// From the value persisted in the profile. `None` = unknown value: the
    /// caller decides whether to degrade or refuse — never silently degrade a
    /// mode the user believed to be verifying.
    pub fn parse(s: &str) -> Option<TlsMode> {
        match s {
            "disable" => Some(TlsMode::Disable),
            "prefer" => Some(TlsMode::Prefer),
            "require" => Some(TlsMode::Require),
            "verify-ca" => Some(TlsMode::VerifyCa),
            "verify-full" => Some(TlsMode::VerifyFull),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TlsMode::Disable => "disable",
            TlsMode::Prefer => "prefer",
            TlsMode::Require => "require",
            TlsMode::VerifyCa => "verify-ca",
            TlsMode::VerifyFull => "verify-full",
        }
    }

    /// The TLS channel must be attempted (everything except `disable`).
    pub fn wants_tls(&self) -> bool {
        !matches!(self, TlsMode::Disable)
    }

    /// The server certificate chain must be verified.
    pub fn verifies_chain(&self) -> bool {
        matches!(self, TlsMode::VerifyCa | TlsMode::VerifyFull)
    }
}

/// Complete TLS options: they travel alongside the connection string to every
/// point that opens a connection — session lanes, out-of-band cancel/KILL,
/// escalation. A connection opened with a regime different from the profile's
/// is a silent downgrade.
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    pub mode: TlsMode,
    /// Root CA of the profile (PEM file): for corporate servers with an
    /// internal CA. With `verify-ca`/`verify-full` it **replaces** the system
    /// roots (like libpq's `sslrootcert`); `verify-ca` requires it.
    /// Ignored in the non-verifying modes.
    pub root_cert: Option<PathBuf>,
    /// Name to verify the server certificate against when the TCP goes
    /// elsewhere — SSH tunnel: the connection points to 127.0.0.1 but the
    /// certificate speaks of the real database. `None` = the connection host.
    /// The PG driver ignores it: there `host` + `hostaddr` in the
    /// connection string play the same role.
    pub tls_server_name: Option<String>,
}

impl TlsOptions {
    pub fn plain() -> TlsOptions {
        TlsOptions {
            mode: TlsMode::Disable,
            root_cert: None,
            tls_server_name: None,
        }
    }

    /// Combinations to refuse before opening the socket: today only
    /// `verify-ca` without a profile CA, which would verify nothing.
    pub fn check(&self) -> Result<(), String> {
        if self.mode == TlsMode::VerifyCa && self.root_cert.is_none() {
            return Err(
                "verify-ca requires the server's root CA (PEM file): without it, \
                        the public certificate of any website would pass. Provide the CA \
                        or use verify-full."
                    .into(),
            );
        }
        Ok(())
    }
}

/// What was negotiated on an open channel, measured on the session itself
/// (§12.2), never inferred from the profile. A `None` field is a datum the
/// engine cannot give: the UI says "not available", it does not hide it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TlsInfo {
    /// Protocol version as the server names it (`TLSv1.3`).
    pub protocol: Option<String>,
    /// Cipher suite (`TLS_AES_256_GCM_SHA384`).
    pub cipher: Option<String>,
    /// The certificate the server presented.
    pub server_cert: Option<CertSummary>,
    /// The handshake succeeded under a chain-verifying mode: the server is
    /// who the CA says. `false` on an encrypted channel = `prefer`/`require`,
    /// which never authenticate the server.
    pub verified: bool,
}

/// The human-readable part of an X.509 certificate: the server's, or the
/// profile's root CA inspected by the connection dialog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CertSummary {
    /// RFC 4514 names (`CN=db.example.com,O=Example`).
    pub subject: Option<String>,
    pub issuer: Option<String>,
    /// Validity bounds, unix seconds.
    pub not_before: Option<i64>,
    pub not_after: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for m in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyCa,
            TlsMode::VerifyFull,
        ] {
            assert_eq!(TlsMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(TlsMode::parse("allow"), None);
        assert_eq!(TlsMode::parse(""), None);
    }

    #[test]
    fn verify_ca_without_ca_is_rejected() {
        let mut t = TlsOptions {
            mode: TlsMode::VerifyCa,
            root_cert: None,
            tls_server_name: None,
        };
        assert!(t.check().is_err());
        t.root_cert = Some("/etc/ca.pem".into());
        assert!(t.check().is_ok());
        t.root_cert = None;
        for m in [
            TlsMode::Disable,
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull,
        ] {
            t.mode = m;
            assert!(t.check().is_ok(), "{m:?}");
        }
    }

    #[test]
    fn mode_semantics() {
        assert!(!TlsMode::Disable.wants_tls());
        assert!(TlsMode::Prefer.wants_tls());
        assert!(!TlsMode::Require.verifies_chain());
        assert!(TlsMode::VerifyCa.verifies_chain());
        assert!(TlsMode::VerifyFull.verifies_chain());
    }
}
