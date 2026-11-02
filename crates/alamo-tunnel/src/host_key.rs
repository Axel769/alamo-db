//! Bastion host key verification (§12).
//!
//! Sources of trust: `~/.ssh/known_hosts` (the same one the `ssh` command
//! uses) and the key the user confirmed in Alamo, saved with the SSH profile.
//! Policy, in order:
//!
//! - key on an `@revoked` line for the host → refusal;
//! - key equal to one recorded for the host → ok;
//! - host recorded with different keys → refusal (possible MITM), even when
//!   the presented key is of ANOTHER algorithm: an attacker freely chooses
//!   which algorithm to offer;
//! - never-seen host → refusal with the fingerprint, which the app shows to
//!   the user for confirmation. Never accepted silently.
//!
//! The parsing is ours and not `russh::keys::known_hosts`: that one compares
//! the host exactly (no wildcards, negations, uppercase), ignores `@revoked`
//! and skips keys of a different algorithm.

use std::path::Path;

use hmac::{Hmac, KeyInit, Mac};
use russh::keys::ssh_key::known_hosts::{Entry, HostPatterns, Marker};
use russh::keys::ssh_key::{HashAlg, PublicKey};
use sha1::Sha1;

/// Outcome of the verification.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Trusted,
    /// Known host with different keys; `source` says where (for the message).
    Changed {
        source: String,
    },
    Revoked {
        line: usize,
    },
    Unknown,
}

/// Contents of `known_hosts`: missing file = empty (whoever has never used
/// `ssh` does not have one), any other error = error — never treat an
/// unreadable file as "no known host".
pub(crate) fn read_known_hosts(path: &Path) -> Result<String, String> {
    // A directory in its place: on Windows `read_to_string` does not fail
    // (std opens directories too), so it is refused here, everywhere.
    if path.is_dir() {
        return Err(format!("{} unreadable: it is a directory", path.display()));
    }
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{} unreadable: {e}", path.display())),
    }
}

/// Fingerprint in the `ssh-keygen -l` format (`SHA256:…`).
pub(crate) fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

pub(crate) fn verify(
    host: &str,
    port: u16,
    presented: &PublicKey,
    known_hosts: &str,
    pinned: Option<&PublicKey>,
) -> Result<Verdict, String> {
    // OpenSSH compares names in lowercase and writes non-standard ports as
    // `[host]:port`.
    let host = host.to_ascii_lowercase();
    let candidate = if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    };
    let mut known: Vec<(String, PublicKey)> = Vec::new();
    for (n, raw) in known_hosts.lines().enumerate() {
        let line = n + 1;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let entry: Entry = match text.parse() {
            Ok(e) => e,
            // Unreadable line: if it concerns THIS host we do not know what
            // it says (maybe the revocation of the key) → error. The others
            // are skipped, as ssh does.
            Err(_) if raw_line_mentions(text, &candidate) => {
                return Err(format!(
                    "known_hosts line {line}: entry for {candidate} is unreadable"
                ));
            }
            Err(_) => continue,
        };
        if !host_matches(entry.host_patterns(), &candidate) {
            continue;
        }
        let same = entry.public_key().key_data() == presented.key_data();
        match entry.marker() {
            Some(Marker::Revoked) if same => return Ok(Verdict::Revoked { line }),
            Some(Marker::Revoked) => {}
            // Certificates not supported: the line authenticates nothing here.
            Some(Marker::CertAuthority) => {}
            None => known.push((
                format!("known_hosts line {line}"),
                entry.public_key().clone(),
            )),
        }
    }
    if let Some(p) = pinned {
        known.push(("the fingerprint saved in Alamo".into(), p.clone()));
    }
    if known
        .iter()
        .any(|(_, k)| k.key_data() == presented.key_data())
    {
        return Ok(Verdict::Trusted);
    }
    Ok(match known.into_iter().next() {
        Some((source, _)) => Verdict::Changed { source },
        None => Verdict::Unknown,
    })
}

/// Host patterns of an entry: a list with globs and negations, or a hashed
/// name (`HashKnownHosts`, HMAC-SHA1 with the line's salt).
fn host_matches(patterns: &HostPatterns, candidate: &str) -> bool {
    match patterns {
        HostPatterns::Patterns(list) => {
            let mut positive = false;
            for p in list {
                let (negated, pat) = match p.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, p.as_str()),
                };
                if glob(&pat.to_ascii_lowercase(), candidate) {
                    if negated {
                        return false;
                    }
                    positive = true;
                }
            }
            positive
        }
        HostPatterns::HashedName { salt, hash } => {
            let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(salt) else {
                return false;
            };
            mac.update(candidate.as_bytes());
            mac.verify_slice(hash).is_ok()
        }
    }
}

/// OpenSSH glob: `*` any sequence, `?` one character.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&b| b == b'*')
}

/// Does a line that cannot be read mention this host? Plain names only: a
/// hash inside a broken line cannot be verified.
fn raw_line_mentions(text: &str, candidate: &str) -> bool {
    let mut fields = text.split_whitespace();
    let mut hosts = fields.next().unwrap_or("");
    if hosts.starts_with('@') {
        hosts = fields.next().unwrap_or("");
    }
    hosts.split(',').any(|p| {
        !p.starts_with('!') && !p.starts_with("|1|") && glob(&p.to_ascii_lowercase(), candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};

    fn key(alg: Algorithm) -> PublicKey {
        PrivateKey::random(&mut rand::rng(), alg)
            .unwrap()
            .public_key()
            .clone()
    }

    fn line(host: &str, k: &PublicKey) -> String {
        format!("{host} {}\n", k.to_openssh().unwrap())
    }

    fn ecdsa() -> Algorithm {
        Algorithm::Ecdsa {
            curve: russh::keys::EcdsaCurve::NistP256,
        }
    }

    #[test]
    fn never_seen_host_is_unknown_even_without_file() {
        let k = key(Algorithm::Ed25519);
        assert_eq!(verify("b.example", 22, &k, "", None), Ok(Verdict::Unknown));
        let others = line("other.example", &key(Algorithm::Ed25519));
        assert_eq!(
            verify("b.example", 22, &k, &others, None),
            Ok(Verdict::Unknown)
        );
    }

    #[test]
    fn recorded_key_passes_even_with_uppercase_and_port() {
        let k = key(Algorithm::Ed25519);
        let kh = line("[b.example]:2222", &k);
        assert_eq!(
            verify("B.Example", 2222, &k, &kh, None),
            Ok(Verdict::Trusted)
        );
        // Same host on another port: it is another server.
        assert_eq!(verify("b.example", 22, &k, &kh, None), Ok(Verdict::Unknown));
    }

    #[test]
    fn different_key_rejected_even_with_other_algorithm() {
        let recorded = key(Algorithm::Ed25519);
        let kh = line("b.example", &recorded);
        // The T9 trick: the server offers another algorithm.
        let presented = key(ecdsa());
        assert_eq!(
            verify("b.example", 22, &presented, &kh, None),
            Ok(Verdict::Changed {
                source: "known_hosts line 1".into()
            })
        );
    }

    #[test]
    fn fingerprint_saved_in_alamo() {
        let k = key(Algorithm::Ed25519);
        assert_eq!(
            verify("b.example", 22, &k, "", Some(&k)),
            Ok(Verdict::Trusted)
        );
        let other = key(Algorithm::Ed25519);
        assert_eq!(
            verify("b.example", 22, &k, "", Some(&other)),
            Ok(Verdict::Changed {
                source: "the fingerprint saved in Alamo".into()
            })
        );
    }

    #[test]
    fn wildcards_negations_and_lists() {
        let k = key(Algorithm::Ed25519);
        let kh = line("*.corp,!old.corp", &k);
        assert_eq!(verify("db.corp", 22, &k, &kh, None), Ok(Verdict::Trusted));
        assert_eq!(verify("old.corp", 22, &k, &kh, None), Ok(Verdict::Unknown));
        let kh = line("b.example,10.0.0.5", &k);
        assert_eq!(verify("10.0.0.5", 22, &k, &kh, None), Ok(Verdict::Trusted));
    }

    #[test]
    fn hashed_host() {
        // Like `ssh-keygen -H`: HMAC-SHA1 of the name with the line's salt.
        let k = key(Algorithm::Ed25519);
        let salt = [7u8; 20];
        let mut mac = Hmac::<Sha1>::new_from_slice(&salt).unwrap();
        mac.update(b"b.example");
        let hash = mac.finalize().into_bytes();
        let hosts = HostPatterns::HashedName {
            salt: salt.to_vec(),
            hash: hash.as_slice().try_into().unwrap(),
        }
        .to_string();
        let kh = line(&hosts, &k);
        assert_eq!(verify("b.example", 22, &k, &kh, None), Ok(Verdict::Trusted));
        let other = key(Algorithm::Ed25519);
        assert!(matches!(
            verify("b.example", 22, &other, &kh, None),
            Ok(Verdict::Changed { .. })
        ));
    }

    #[test]
    fn revoked_wins_over_everything() {
        let k = key(Algorithm::Ed25519);
        let kh = format!(
            "{}@revoked * {}\n",
            line("b.example", &k),
            k.to_openssh().unwrap()
        );
        assert_eq!(
            verify("b.example", 22, &k, &kh, Some(&k)),
            Ok(Verdict::Revoked { line: 2 })
        );
    }

    #[test]
    fn broken_line_for_this_host_is_an_error_others_are_skipped() {
        let k = key(Algorithm::Ed25519);
        let broken = "b.example ssh-ed25519 non-base64!!\n";
        assert!(verify("b.example", 22, &k, broken, None).is_err());
        let broken_other = format!("other.example ssh-ed25519 xx\n{}", line("b.example", &k));
        assert_eq!(
            verify("b.example", 22, &k, &broken_other, None),
            Ok(Verdict::Trusted)
        );
    }

    #[test]
    fn comments_and_blank_lines() {
        let k = key(Algorithm::Ed25519);
        let kh = format!("# comment\n\n{}", line("b.example", &k));
        assert_eq!(verify("b.example", 22, &k, &kh, None), Ok(Verdict::Trusted));
    }

    #[test]
    fn glob_basic_cases() {
        assert!(glob("*", "x"));
        assert!(glob("db?.corp", "db1.corp"));
        assert!(!glob("db?.corp", "db12.corp"));
        assert!(glob("*.corp", "a.b.corp"));
        assert!(!glob("*.corp", "corp"));
    }
}
