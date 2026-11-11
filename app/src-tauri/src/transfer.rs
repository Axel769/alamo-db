//! Export/import of the saved connections (like DBeaver, our own format): a
//! readable JSON with the database profiles and the SSH bastions they use.
//!
//! Local ids (`id`, `secret_tag`), usage order, default sorts and the
//! confirmed bastion fingerprint do not end up in the file: trust in a host
//! key is not imported from a file, it is reconfirmed at the first
//! connection.
//!
//! Passwords (database and SSH) go in only if asked, in a single blob
//! encrypted with a password chosen at export (`seal`): from the file one
//! cannot even tell which connections have one. The blob is bound to the
//! rest of the document (AES-GCM associated data): changing a host in the
//! file to get the password sent to you makes the blob undecryptable.

use std::collections::{BTreeMap, HashMap, HashSet};

use alamo_session::driver::Drivers;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::storage;

pub const FORMAT: &str = "alamo-connections";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDoc {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub exported_at: Option<String>,
    #[serde(default)]
    pub ssh_profiles: Vec<SshEntry>,
    #[serde(default)]
    pub profiles: Vec<ProfileEntry>,
    /// Encrypted passwords (`seal`); absent = file without secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Sealed>,
}

/// SSH bastion: an entity of its own, as in the DB — several database
/// profiles reference it by its `ref`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshEntry {
    #[serde(rename = "ref")]
    pub ref_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub name: String,
    pub dialect: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub sslmode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssl_root_cert: Option<String>,
    /// Client certificate and key paths (§12.3); the key passphrase is
    /// never in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssl_client_cert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssl_client_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub is_production: bool,
    #[serde(default)]
    pub read_only: bool,
    /// `ref` of the bastion in `ssh_profiles`; absent = direct connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    #[serde(default)]
    pub db_include: String,
    #[serde(default)]
    pub db_exclude: String,
    /// Per-connection setting overrides (§16): empty object omitted.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// Document to export and, aligned by index, the local ids it comes from:
/// needed to read the passwords from the keychain.
pub struct Built {
    pub doc: ExportDoc,
    pub profile_ids: Vec<i64>,
    pub ssh_ids: Vec<i64>,
}

/// Document with the chosen profiles, in list order (last use, then name)
/// and with only the bastions they use. Ids that vanished in the meantime
/// are skipped.
pub fn build_export(db: &Connection, ids: &[i64]) -> rusqlite::Result<Built> {
    let wanted: HashSet<i64> = ids.iter().copied().collect();
    let mut ssh_refs: HashMap<i64, String> = HashMap::new();
    let mut ssh_profiles = Vec::new();
    let mut ssh_ids = Vec::new();
    let mut profiles = Vec::new();
    let mut profile_ids = Vec::new();
    for p in storage::profiles_list(db)? {
        let Some(id) = p.id.filter(|id| wanted.contains(id)) else {
            continue;
        };
        profile_ids.push(id);
        let ssh = match p.ssh_profile_id {
            Some(sid) => match ssh_refs.get(&sid) {
                Some(r) => Some(r.clone()),
                // Orphan reference (should not happen, delete detaches):
                // exported as a direct connection.
                None => storage::ssh_profile_get(db, sid)?.map(|s| {
                    let r = format!("s{}", ssh_profiles.len() + 1);
                    ssh_refs.insert(sid, r.clone());
                    ssh_ids.push(sid);
                    ssh_profiles.push(SshEntry {
                        ref_id: r.clone(),
                        name: s.name,
                        host: s.host,
                        port: s.port,
                        username: s.username,
                        auth: s.auth,
                        key_path: s.key_path,
                    });
                    r
                }),
            },
            None => None,
        };
        profiles.push(ProfileEntry {
            name: p.name,
            dialect: p.dialect,
            host: p.host,
            port: p.port,
            database: p.database,
            username: p.username,
            sslmode: p.sslmode,
            ssl_root_cert: p.ssl_root_cert,
            ssl_client_cert: p.ssl_client_cert,
            ssl_client_key: p.ssl_client_key,
            color: p.color,
            is_production: p.is_production,
            read_only: p.read_only,
            ssh,
            db_include: p.db_include,
            db_exclude: p.db_exclude,
            settings: p.settings,
        });
    }
    Ok(Built {
        doc: ExportDoc {
            format: FORMAT.into(),
            version: VERSION,
            exported_at: Some(utc_now()),
            ssh_profiles,
            profiles,
            secrets: None,
        },
        profile_ids,
        ssh_ids,
    })
}

// --------------------------------------------------------------- passwords

/// Clear-text passwords, before `seal` and after `open`: index in
/// `profiles` → database password, bastion `ref` → password or key
/// passphrase. Never serialized outside the encrypted blob.
#[derive(Default, Serialize, Deserialize)]
pub struct Secrets {
    #[serde(default)]
    pub profiles: BTreeMap<usize, String>,
    #[serde(default)]
    pub ssh: BTreeMap<String, String>,
}

impl Drop for Secrets {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.profiles.values_mut().for_each(Zeroize::zeroize);
        self.ssh.values_mut().for_each(Zeroize::zeroize);
    }
}

/// Only the counts: a password must not end up in a log by mistake.
impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Secrets({} db, {} ssh)",
            self.profiles.len(),
            self.ssh.len()
        )
    }
}

impl Secrets {
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty() && self.ssh.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sealed {
    pub kdf: Kdf,
    pub cipher: String,
    pub nonce: String,
    pub data: String,
}

/// Argon2id parameters written in the file: they can be raised in the
/// future without breaking old files.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kdf {
    pub alg: String,
    #[serde(flatten)]
    pub params: KdfParams,
    pub salt: String,
}

/// 64 MiB, 3 passes: OWASP recommendation for Argon2id.
pub const KDF_DEFAULT: KdfParams = KdfParams {
    m: 64 * 1024,
    t: 3,
    p: 1,
};
/// Caps for the parameters read from a file: a malicious file must not be
/// able to ask for gigabytes of memory or minutes of computation.
const KDF_MAX: KdfParams = KdfParams {
    m: 1024 * 1024,
    t: 10,
    p: 4,
};
pub const MIN_PASSPHRASE: usize = 8;

/// Associated data: the document without secrets, in canonical form (structs
/// serialized by serde, fixed field order). It must be computed on the
/// document already passed through `normalize`, both at export and at
/// import.
fn aad(doc: &ExportDoc) -> Vec<u8> {
    serde_json::to_vec(&(&doc.format, doc.version, &doc.ssh_profiles, &doc.profiles))
        .unwrap_or_default()
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(params.m, params.t, params.p, Some(32))
        .map_err(|e| format!("invalid encryption parameters: {e}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| format!("key derivation: {e}"))?;
    Ok(key)
}

/// Encrypts the passwords into the document (`doc.secrets`). The document
/// must already be normalized: it is the associated data.
pub fn seal(
    doc: &mut ExportDoc,
    secrets: &Secrets,
    passphrase: &str,
    params: KdfParams,
) -> Result<(), String> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(format!(
            "the file password must be at least {MIN_PASSPHRASE} characters long"
        ));
    }
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut salt).map_err(|e| format!("random generator: {e}"))?;
    getrandom::fill(&mut nonce).map_err(|e| format!("random generator: {e}"))?;
    let key = derive_key(passphrase, &salt, params)?;
    let plain = Zeroizing::new(serde_json::to_vec(secrets).map_err(|e| e.to_string())?);
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|e| e.to_string())?;
    let aad = aad(doc);
    let data = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &plain,
                aad: &aad,
            },
        )
        .map_err(|_| "encryption failed".to_string())?;
    doc.secrets = Some(Sealed {
        kdf: Kdf {
            alg: "argon2id".into(),
            params,
            salt: B64.encode(salt),
        },
        cipher: "aes-256-gcm".into(),
        nonce: B64.encode(nonce),
        data: B64.encode(data),
    });
    Ok(())
}

/// Decrypts the document's passwords. Wrong password and document modified
/// after the export are the same error: GCM does not tell them apart.
pub fn open(doc: &ExportDoc, passphrase: &str) -> Result<Secrets, String> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    let Some(s) = &doc.secrets else {
        return Ok(Secrets::default());
    };
    let bad = || "encrypted passwords unreadable: damaged file".to_string();
    let salt = B64.decode(&s.kdf.salt).map_err(|_| bad())?;
    let nonce: [u8; 12] = B64
        .decode(&s.nonce)
        .map_err(|_| bad())?
        .try_into()
        .map_err(|_| bad())?;
    let data = B64.decode(&s.data).map_err(|_| bad())?;
    let key = derive_key(passphrase, &salt, s.kdf.params)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|e| e.to_string())?;
    let aad = aad(doc);
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &data,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                "wrong file password (or the file was modified after the export)".to_string()
            })?,
    );
    let secrets: Secrets = serde_json::from_slice(&plain).map_err(|_| bad())?;
    if secrets.profiles.keys().any(|&i| i >= doc.profiles.len())
        || secrets.ssh.keys().any(|r| ssh_entry(doc, r).is_none())
    {
        return Err(bad());
    }
    Ok(secrets)
}

// ------------------------------------------------------------------ import

/// Cap on the file to import: thousands of connections fit in a few hundred
/// kB, beyond that it is the wrong file.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_ENTRIES: usize = 5000;
const MAX_FIELD: usize = 4096;

/// Reads an export file. Errors in the user's words: the message ends up as
/// is in the dialog.
pub fn parse(bytes: &[u8], drivers: &Drivers) -> Result<ExportDoc, String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "the file is not valid JSON".to_string())?;
    if v.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err("the file is not an Alamo connections export".into());
    }
    // Before the full parse: a future version may have fields this one does
    // not know, and the right error is "update the app".
    if v.get("version")
        .and_then(|x| x.as_u64())
        .is_some_and(|n| n > u64::from(VERSION))
    {
        return Err("the file was created by a newer version of Alamo: \
                    update the app to import it"
            .into());
    }
    let mut doc: ExportDoc =
        serde_json::from_value(v).map_err(|e| format!("invalid connections export: {e}"))?;
    normalize(&mut doc, drivers)?;
    Ok(doc)
}

/// Checks and cleanup of an untrusted document: everything that reaches the
/// DB goes through here — at apply too, because the document makes a round
/// trip through the frontend. Broken but harmless values are corrected
/// (unknown sslmode = `prefer` as in `storage::tls_options`, invalid color =
/// none), those without a sensible default reject the whole file.
pub fn normalize(doc: &mut ExportDoc, drivers: &Drivers) -> Result<(), String> {
    if doc.format != FORMAT || doc.version == 0 || doc.version > VERSION {
        return Err("invalid connections export: unknown format or version".into());
    }
    if doc.profiles.len() > MAX_ENTRIES || doc.ssh_profiles.len() > MAX_ENTRIES {
        return Err("invalid connections export: too many entries".into());
    }
    let mut refs = HashSet::new();
    for s in &mut doc.ssh_profiles {
        trim(&mut s.name);
        trim(&mut s.host);
        let what = format!("SSH tunnel «{}»", s.name);
        too_long(&what, &[&s.ref_id, &s.name, &s.host, &s.username])?;
        too_long(&what, &[s.key_path.as_deref().unwrap_or("")])?;
        if s.name.is_empty() || s.host.is_empty() || s.username.is_empty() || s.port == 0 {
            return Err(format!("{what}: name, host, user or port missing"));
        }
        if !matches!(s.auth.as_str(), "agent" | "password" | "key") {
            return Err(format!("{what}: unknown authentication «{}»", s.auth));
        }
        if !refs.insert(s.ref_id.clone()) {
            return Err(format!("{what}: duplicate reference «{}»", s.ref_id));
        }
        none_if_blank(&mut s.key_path);
    }
    for p in &mut doc.profiles {
        trim(&mut p.name);
        trim(&mut p.host);
        let what = format!("connection «{}»", p.name);
        too_long(
            &what,
            &[
                &p.name,
                &p.host,
                &p.database,
                &p.username,
                &p.sslmode,
                &p.db_include,
                &p.db_exclude,
            ],
        )?;
        too_long(
            &what,
            &[
                p.ssl_root_cert.as_deref().unwrap_or(""),
                p.ssl_client_cert.as_deref().unwrap_or(""),
                p.ssl_client_key.as_deref().unwrap_or(""),
            ],
        )?;
        if p.name.is_empty() || p.host.is_empty() || p.port == 0 {
            return Err(format!("{what}: name, host or port missing"));
        }
        if drivers.get(&p.dialect).is_none() {
            return Err(format!("{what}: unsupported engine «{}»", p.dialect));
        }
        if let Some(r) = &p.ssh {
            if !refs.contains(r) {
                return Err(format!("{what}: SSH tunnel «{r}» missing from the file"));
            }
        }
        if alamo_core::tls::TlsMode::parse(&p.sslmode).is_none() {
            p.sslmode = "prefer".into();
        }
        let hex = |c: &str| {
            c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
        };
        if p.color.as_deref().is_some_and(|c| !hex(c)) {
            p.color = None;
        }
        none_if_blank(&mut p.ssl_root_cert);
        none_if_blank(&mut p.ssl_client_cert);
        none_if_blank(&mut p.ssl_client_key);
    }
    if let Some(s) = &doc.secrets {
        let p = s.kdf.params;
        if s.kdf.alg != "argon2id" || s.cipher != "aes-256-gcm" {
            return Err("passwords encrypted with an unknown method".into());
        }
        if p.m > KDF_MAX.m || p.t > KDF_MAX.t || p.p > KDF_MAX.p || s.data.len() > MAX_FIELD * 1024
        {
            return Err("passwords encrypted with out-of-range parameters".into());
        }
    }
    Ok(())
}

fn trim(s: &mut String) {
    if s.trim().len() != s.len() {
        *s = s.trim().to_string();
    }
}

fn none_if_blank(s: &mut Option<String>) {
    if s.as_deref().is_some_and(|v| v.trim().is_empty()) {
        *s = None;
    }
}

fn too_long(what: &str, fields: &[&str]) -> Result<(), String> {
    if fields.iter().any(|f| f.len() > MAX_FIELD) {
        return Err(format!("{what}: field too long"));
    }
    Ok(())
}

/// One preview row, aligned by index to `doc.profiles`.
#[derive(Debug, Serialize)]
pub struct ImportRow {
    /// Name it will enter with: the file's one, or "name (2)" if already
    /// used.
    pub name: String,
    pub renamed: bool,
    /// Same server, database, user and tunnel as a saved profile: the
    /// preview proposes it unselected.
    pub existing: bool,
    /// Paths (CA, SSH key) that do not exist on this computer.
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ImportOutcome {
    pub imported: usize,
    pub ssh_created: usize,
    pub ssh_reused: usize,
    /// Passwords put in the keychain (the caller writes them, after the
    /// commit).
    pub passwords_saved: usize,
    /// Problems that do not cancel the import (keychain not writable).
    pub warnings: Vec<String>,
    /// Index in `doc.profiles` → id of the created profile.
    #[serde(skip)]
    pub new_profiles: Vec<(usize, i64)>,
    /// Bastion `ref` → id of the created SSH profile (reused ones keep the
    /// credential they have: not touched).
    #[serde(skip)]
    pub new_ssh: Vec<(String, i64)>,
}

/// Import preview. `exists` says whether a path exists on this computer
/// (parameter: tests do not touch the disk).
pub fn preview(
    db: &Connection,
    doc: &ExportDoc,
    exists: &dyn Fn(&str) -> bool,
) -> rusqlite::Result<Vec<ImportRow>> {
    let local = storage::profiles_list(db)?;
    let local_ssh = storage::ssh_profiles_list(db)?;
    let mut taken = names(local.iter().map(|p| p.name.as_str()));
    Ok(doc
        .profiles
        .iter()
        .map(|e| {
            let e_ssh = e.ssh.as_deref().and_then(|r| ssh_entry(doc, r));
            let existing = local.iter().any(|p| {
                let p_ssh = p
                    .ssh_profile_id
                    .and_then(|id| local_ssh.iter().find(|s| s.id == Some(id)));
                same_server(&p.host, p.port, &p.username, &e.host, e.port, &e.username)
                    && p.dialect == e.dialect
                    && p.database == e.database
                    && match (p_ssh, e_ssh) {
                        (None, None) => true,
                        (Some(a), Some(b)) => {
                            same_server(&a.host, a.port, &a.username, &b.host, b.port, &b.username)
                        }
                        _ => false,
                    }
            });
            let name = unique_name(&e.name, &mut taken);
            let mut warnings = Vec::new();
            if let Some(ca) = e.ssl_root_cert.as_deref().filter(|p| !exists(p)) {
                warnings.push(format!("CA certificate not found on this computer: {ca}"));
            }
            for f in [&e.ssl_client_cert, &e.ssl_client_key]
                .into_iter()
                .flatten()
                .filter(|p| !exists(p))
            {
                warnings.push(format!(
                    "client certificate file not found on this computer: {f}"
                ));
            }
            if let Some(key) = e_ssh
                .filter(|s| s.auth == "key")
                .and_then(|s| s.key_path.as_deref())
                .filter(|p| !exists(p))
            {
                warnings.push(format!("SSH key not found on this computer: {key}"));
            }
            ImportRow {
                renamed: name != e.name,
                name,
                existing,
                warnings,
            }
        })
        .collect())
}

/// Imports the chosen profiles (indexes in `doc.profiles`), all or nothing.
/// Never overwrites: every profile enters as new, renamed if the name is
/// already used. Bastions identical to a saved one (host, port, user,
/// authentication, key) are reused, with the credential they already have.
pub fn apply(
    db: &Connection,
    doc: &ExportDoc,
    selected: &[usize],
) -> Result<ImportOutcome, String> {
    let selected: std::collections::BTreeSet<usize> = selected.iter().copied().collect();
    if selected.iter().any(|&i| i >= doc.profiles.len()) {
        return Err("invalid selection".into());
    }
    let dberr = |e: rusqlite::Error| e.to_string();
    let tx = db.unchecked_transaction().map_err(dberr)?;
    let local_ssh = storage::ssh_profiles_list(&tx).map_err(dberr)?;
    let mut ssh_taken = names(local_ssh.iter().map(|s| s.name.as_str()));
    let mut taken = names(
        storage::profiles_list(&tx)
            .map_err(dberr)?
            .iter()
            .map(|p| p.name.as_str()),
    );
    let mut ssh_ids: HashMap<&str, i64> = HashMap::new();
    let mut out = ImportOutcome::default();
    for i in selected {
        let e = &doc.profiles[i];
        let ssh_profile_id = match e.ssh.as_deref() {
            None => None,
            Some(r) => Some(match ssh_ids.get(r) {
                Some(&id) => id,
                None => {
                    // `normalize` guarantees that the ref exists.
                    let s = ssh_entry(doc, r).ok_or("SSH tunnel missing from the file")?;
                    let reuse = local_ssh.iter().find(|l| {
                        same_server(&l.host, l.port, &l.username, &s.host, s.port, &s.username)
                            && l.auth == s.auth
                            && l.key_path.as_deref().unwrap_or("")
                                == s.key_path.as_deref().unwrap_or("")
                    });
                    let id = match reuse.and_then(|l| l.id) {
                        Some(id) => {
                            out.ssh_reused += 1;
                            id
                        }
                        None => {
                            out.ssh_created += 1;
                            let id = storage::ssh_profile_save(
                                &tx,
                                &storage::SshProfile {
                                    id: None,
                                    name: unique_name(&s.name, &mut ssh_taken),
                                    host: s.host.clone(),
                                    port: s.port,
                                    username: s.username.clone(),
                                    auth: s.auth.clone(),
                                    key_path: s.key_path.clone(),
                                },
                            )
                            .map_err(dberr)?;
                            out.new_ssh.push((r.to_string(), id));
                            id
                        }
                    };
                    ssh_ids.insert(r, id);
                    id
                }
            }),
        };
        let id = storage::profile_save(
            &tx,
            &storage::Profile {
                id: None,
                name: unique_name(&e.name, &mut taken),
                dialect: e.dialect.clone(),
                host: e.host.clone(),
                port: e.port,
                database: e.database.clone(),
                username: e.username.clone(),
                sslmode: e.sslmode.clone(),
                ssl_root_cert: e.ssl_root_cert.clone(),
                ssl_client_cert: e.ssl_client_cert.clone(),
                ssl_client_key: e.ssl_client_key.clone(),
                color: e.color.clone(),
                is_production: e.is_production,
                read_only: e.read_only,
                ssh_profile_id,
                db_include: e.db_include.clone(),
                db_exclude: e.db_exclude.clone(),
                settings: e.settings.clone(),
            },
        )
        .map_err(dberr)?;
        out.new_profiles.push((i, id));
        out.imported += 1;
    }
    tx.commit().map_err(dberr)?;
    Ok(out)
}

fn ssh_entry<'a>(doc: &'a ExportDoc, r: &str) -> Option<&'a SshEntry> {
    doc.ssh_profiles.iter().find(|s| s.ref_id == r)
}

/// Case-insensitive host (DNS names are case-insensitive).
fn same_server(h1: &str, p1: u16, u1: &str, h2: &str, p2: u16, u2: &str) -> bool {
    h1.eq_ignore_ascii_case(h2) && p1 == p2 && u1 == u2
}

/// Names already used, case-insensitive: "Prod" and "prod" in the same list
/// get confused.
fn names<'a>(it: impl Iterator<Item = &'a str>) -> HashSet<String> {
    it.map(str::to_lowercase).collect()
}

/// `base`, or `base (2)`, `base (3)`… the first free one. Marks it as used.
fn unique_name(base: &str, taken: &mut HashSet<String>) -> String {
    let mut name = base.to_string();
    let mut n = 2;
    while taken.contains(&name.to_lowercase()) {
        name = format!("{base} ({n})");
        n += 1;
    }
    taken.insert(name.to_lowercase());
    name
}

fn utc_now() -> String {
    rfc3339(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Seconds since the epoch in RFC 3339 UTC. Civil calendar from days since
/// the epoch (H. Hinnant's algorithm): not worth a dependency.
fn rfc3339(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::{Profile, SshProfile};

    /// The example shown in the import dialog ("File format") must be a
    /// file the import accepts as is.
    #[test]
    fn format_example_in_the_import_dialog_is_valid() {
        let doc = parse(
            include_bytes!("../../src/connections-example.json"),
            &Drivers::builtin(),
        )
        .unwrap();
        assert_eq!(doc.profiles.len(), 2);
        assert_eq!(doc.ssh_profiles.len(), 1);
        assert_eq!(doc.profiles[0].ssh.as_deref(), Some("s1"));
        assert_eq!(doc.profiles[0].sslmode, "verify-full");
        assert_eq!(doc.profiles[0].color.as_deref(), Some("#d73a49"));
        assert!(doc.secrets.is_none());
        let rows = preview(&mem(), &doc, &|_| true).unwrap();
        assert!(rows.iter().all(|r| !r.existing && !r.renamed));
    }

    fn mem() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        storage::migrate(&db).unwrap();
        db
    }

    fn profile(name: &str, ssh: Option<i64>) -> Profile {
        Profile {
            id: None,
            name: name.into(),
            host: "db.internal".into(),
            port: 5432,
            database: "app".into(),
            username: "app".into(),
            sslmode: "require".into(),
            ssl_root_cert: None,
            ssl_client_cert: None,
            ssl_client_key: None,
            color: Some("#a02a83".into()),
            is_production: true,
            dialect: "postgres".into(),
            read_only: false,
            ssh_profile_id: ssh,
            db_include: String::new(),
            db_exclude: "*_test*".into(),
            settings: serde_json::Map::new(),
        }
    }

    fn bastion(name: &str) -> SshProfile {
        SshProfile {
            id: None,
            name: name.into(),
            host: "bastion.example".into(),
            port: 22,
            username: "ops".into(),
            auth: "key".into(),
            key_path: Some("~/.ssh/id_ed25519".into()),
        }
    }

    #[test]
    fn export_only_chosen_and_shared_bastion_once() {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        storage::ssh_profile_save(&db, &bastion("unused")).unwrap();
        let a = storage::profile_save(&db, &profile("a", Some(sid))).unwrap();
        let b = storage::profile_save(&db, &profile("b", Some(sid))).unwrap();
        storage::profile_save(&db, &profile("c", None)).unwrap();

        let built = build_export(&db, &[a, b, 9999]).unwrap();
        assert_eq!(built.profile_ids, [a, b]);
        assert_eq!(built.ssh_ids, [sid]);
        let doc = built.doc;
        assert_eq!(doc.format, FORMAT);
        assert_eq!(doc.version, VERSION);
        let names: Vec<_> = doc.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(doc.ssh_profiles.len(), 1);
        assert_eq!(doc.ssh_profiles[0].name, "jump");
        assert_eq!(doc.ssh_profiles[0].ref_id, "s1");
        assert!(doc.profiles.iter().all(|p| p.ssh.as_deref() == Some("s1")));
        assert_eq!(doc.profiles[0].db_exclude, "*_test*");
        assert!(doc.profiles[0].is_production);
    }

    #[test]
    fn json_without_ids_or_secrets() {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        storage::ssh_host_key_set(&db, sid, "ssh-ed25519 AAAA").unwrap();
        let a = storage::profile_save(&db, &profile("a", Some(sid))).unwrap();
        let json = serde_json::to_string(&build_export(&db, &[a]).unwrap().doc).unwrap();
        for forbidden in [
            "\"id\"",
            "secret_tag",
            "host_key",
            "ssh-ed25519",
            "ssh_profile_id",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} in the file: {json}");
        }
        assert!(json.contains("\"ref\":\"s1\""));
    }

    /// Export from `db`, serialized and read back as the import would.
    fn round_trip(db: &Connection, ids: &[i64]) -> ExportDoc {
        let json = serde_json::to_vec_pretty(&build_export(db, ids).unwrap().doc).unwrap();
        parse(&json, &Drivers::builtin()).unwrap()
    }

    /// Minimal parameters: real Argon2 in tests costs seconds.
    const KDF_TEST: KdfParams = KdfParams { m: 64, t: 1, p: 1 };

    fn secrets() -> Secrets {
        let mut s = Secrets::default();
        s.profiles.insert(0, "pw-db-0".into());
        s.ssh.insert("s1".into(), "passphrase".into());
        s
    }

    /// Document with encrypted passwords, passed through JSON like a real
    /// file.
    fn sealed(passphrase: &str) -> ExportDoc {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        let a = storage::profile_save(&db, &profile("a", Some(sid))).unwrap();
        let b = storage::profile_save(&db, &profile("b", None)).unwrap();
        let mut doc = build_export(&db, &[a, b]).unwrap().doc;
        normalize(&mut doc, &Drivers::builtin()).unwrap();
        seal(&mut doc, &secrets(), passphrase, KDF_TEST).unwrap();
        parse(&serde_json::to_vec(&doc).unwrap(), &Drivers::builtin()).unwrap()
    }

    #[test]
    fn encrypted_passwords_round_trip() {
        let doc = sealed("a long password");
        let json = serde_json::to_string(&doc).unwrap();
        for forbidden in ["pw-db-0", "passphrase\""] {
            assert!(
                !json.contains(forbidden),
                "{forbidden} in clear text: {json}"
            );
        }
        let s = open(&doc, "a long password").unwrap();
        assert_eq!(s.profiles.get(&0).map(String::as_str), Some("pw-db-0"));
        assert_eq!(s.ssh.get("s1").map(String::as_str), Some("passphrase"));
        // Without secrets: open asks for nothing.
        let mut bare = doc.clone();
        bare.secrets = None;
        assert!(open(&bare, "anything").unwrap().is_empty());
    }

    #[test]
    fn wrong_file_password() {
        let doc = sealed("a long password");
        assert!(open(&doc, "another password")
            .unwrap_err()
            .contains("wrong"));
    }

    #[test]
    fn host_changed_in_file_blob_undecryptable() {
        // Whoever edits the file to get the password sent to them does not
        // get it.
        let mut doc = sealed("a long password");
        doc.profiles[0].host = "attacker.example".into();
        assert!(open(&doc, "a long password").unwrap_err().contains("wrong"));
        let mut doc = sealed("a long password");
        doc.ssh_profiles[0].host = "attacker.example".into();
        assert!(open(&doc, "a long password").is_err());
    }

    #[test]
    fn tampered_blob_or_out_of_range_parameters() {
        let mut doc = sealed("a long password");
        let s = doc.secrets.as_mut().unwrap();
        s.data.replace_range(0..4, "AAAA");
        assert!(open(&doc, "a long password").is_err());

        let mut doc = sealed("a long password");
        doc.secrets.as_mut().unwrap().kdf.params.m = 64 * 1024 * 1024;
        assert!(normalize(&mut doc, &Drivers::builtin())
            .unwrap_err()
            .contains("out-of-range"));
        let mut doc = sealed("a long password");
        doc.secrets.as_mut().unwrap().cipher = "rot13".into();
        assert!(normalize(&mut doc, &Drivers::builtin())
            .unwrap_err()
            .contains("unknown"));
    }

    /// The real parameters are accepted by Argon2 and, with argon2 optimized
    /// in debug too (workspace Cargo.toml), do not block for seconds.
    #[test]
    fn real_parameters_valid_and_fast() {
        let t = std::time::Instant::now();
        derive_key("a long password", &[7u8; 16], KDF_DEFAULT).unwrap();
        assert!(t.elapsed().as_secs() < 3, "{:?}", t.elapsed());
    }

    #[test]
    fn file_password_too_short() {
        let mut doc = sealed("a long password");
        assert!(seal(&mut doc, &secrets(), "short", KDF_TEST)
            .unwrap_err()
            .contains("at least"));
    }

    #[test]
    fn apply_tells_which_ids_it_created() {
        let doc = sealed("a long password");
        let db = mem();
        let out = apply(&db, &doc, &[1, 0]).unwrap();
        // In index order, with the real ids.
        let names: Vec<_> = out
            .new_profiles
            .iter()
            .map(|&(i, id)| (i, storage::profile_get(&db, id).unwrap().unwrap().name))
            .collect();
        assert_eq!(names, [(0, "a".to_string()), (1, "b".to_string())]);
        assert_eq!(out.new_ssh.len(), 1);
        assert_eq!(out.new_ssh[0].0, "s1");
        // Reused bastion: not among the created ones (its credential is not
        // touched).
        let out = apply(&db, &doc, &[0]).unwrap();
        assert!(out.new_ssh.is_empty());
        assert_eq!(out.ssh_reused, 1);
    }

    fn everything_exists(_: &str) -> bool {
        true
    }

    #[test]
    fn import_into_empty_db_recreates_profiles_and_bastion() {
        let a_db = mem();
        let sid = storage::ssh_profile_save(&a_db, &bastion("jump")).unwrap();
        let a = storage::profile_save(&a_db, &profile("a", Some(sid))).unwrap();
        let b = storage::profile_save(&a_db, &profile("b", Some(sid))).unwrap();
        let mut direct = profile("c", None);
        direct.dialect = "mysql".into();
        direct.port = 3306;
        direct.read_only = true;
        direct.ssl_root_cert = Some("/etc/ca.pem".into());
        let c = storage::profile_save(&a_db, &direct).unwrap();
        let doc = round_trip(&a_db, &[a, b, c]);

        let b_db = mem();
        let rows = preview(&b_db, &doc, &everything_exists).unwrap();
        assert!(rows
            .iter()
            .all(|r| !r.existing && !r.renamed && r.warnings.is_empty()));
        let out = apply(&b_db, &doc, &[0, 1, 2]).unwrap();
        assert_eq!((out.imported, out.ssh_created, out.ssh_reused), (3, 1, 0));

        let ssh = storage::ssh_profiles_list(&b_db).unwrap();
        assert_eq!(ssh.len(), 1);
        assert_eq!(ssh[0].key_path.as_deref(), Some("~/.ssh/id_ed25519"));
        let mut got = storage::profiles_list(&b_db).unwrap();
        got.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(got[0].ssh_profile_id, ssh[0].id);
        assert_eq!(got[1].ssh_profile_id, ssh[0].id);
        assert_eq!(got[0].db_exclude, "*_test*");
        assert!(got[0].is_production);
        assert_eq!(got[2].dialect, "mysql");
        assert!(got[2].read_only && got[2].ssh_profile_id.is_none());
        assert_eq!(got[2].ssl_root_cert.as_deref(), Some("/etc/ca.pem"));
        // The imported bastion carries no trust with it.
        assert!(storage::ssh_host_key_get(&b_db, ssh[0].id.unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn reimport_into_same_db_already_present_and_renamed() {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        let a = storage::profile_save(&db, &profile("a", Some(sid))).unwrap();
        let doc = round_trip(&db, &[a]);

        let rows = preview(&db, &doc, &everything_exists).unwrap();
        assert!(rows[0].existing);
        assert_eq!(rows[0].name, "a (2)");
        assert!(rows[0].renamed);

        // Imported anyway: renamed copy, bastion reused.
        let out = apply(&db, &doc, &[0]).unwrap();
        assert_eq!((out.imported, out.ssh_created, out.ssh_reused), (1, 0, 1));
        let names: Vec<_> = storage::profiles_list(&db)
            .unwrap()
            .into_iter()
            .map(|p| (p.name, p.ssh_profile_id))
            .collect();
        assert!(names.contains(&("a (2)".into(), Some(sid))), "{names:?}");
        assert_eq!(storage::ssh_profiles_list(&db).unwrap().len(), 1);
    }

    #[test]
    fn same_name_different_server_is_not_already_present() {
        let db = mem();
        let a = storage::profile_save(&db, &profile("a", None)).unwrap();
        let mut doc = round_trip(&db, &[a]);
        doc.profiles[0].host = "other.host".into();
        let rows = preview(&db, &doc, &everything_exists).unwrap();
        assert!(!rows[0].existing);
        assert_eq!(rows[0].name, "a (2)");
        // Upper-case host = same server.
        doc.profiles[0].host = "DB.INTERNAL".into();
        assert!(preview(&db, &doc, &everything_exists).unwrap()[0].existing);
    }

    #[test]
    fn bastion_with_different_key_is_not_reused() {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        let a = storage::profile_save(&db, &profile("a", Some(sid))).unwrap();
        let mut doc = round_trip(&db, &[a]);
        doc.ssh_profiles[0].key_path = Some("~/.ssh/other".into());
        let out = apply(&db, &doc, &[0]).unwrap();
        assert_eq!((out.ssh_created, out.ssh_reused), (1, 0));
        let ssh: Vec<_> = storage::ssh_profiles_list(&db)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(ssh, ["jump", "jump (2)"]);
    }

    #[test]
    fn warnings_on_missing_paths() {
        let db = mem();
        let sid = storage::ssh_profile_save(&db, &bastion("jump")).unwrap();
        let mut p = profile("a", Some(sid));
        p.ssl_root_cert = Some("/does/not/exist/ca.pem".into());
        p.ssl_client_cert = Some("/does/not/exist/client.pem".into());
        p.ssl_client_key = Some("/does/not/exist/client-key.pem".into());
        let a = storage::profile_save(&db, &p).unwrap();
        let doc = round_trip(&db, &[a]);
        assert_eq!(
            doc.profiles[0].ssl_client_key.as_deref(),
            Some("/does/not/exist/client-key.pem")
        );
        let rows = preview(&mem(), &doc, &|_| false).unwrap();
        let w = &rows[0].warnings;
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w[0].contains("/does/not/exist/ca.pem"));
        assert!(w[1].contains("/does/not/exist/client.pem"));
        assert!(w[2].contains("/does/not/exist/client-key.pem"));
        assert!(w[3].contains("~/.ssh/id_ed25519"));
    }

    #[test]
    fn out_of_range_selection_writes_nothing() {
        let db = mem();
        let a = storage::profile_save(&db, &profile("a", None)).unwrap();
        let doc = round_trip(&db, &[a]);
        let empty = mem();
        assert!(apply(&empty, &doc, &[0, 5]).is_err());
        assert!(storage::profiles_list(&empty).unwrap().is_empty());
    }

    #[test]
    fn rejected_files() {
        let err = |s: &str| parse(s.as_bytes(), &Drivers::builtin()).unwrap_err();
        assert!(err("not json").contains("JSON"));
        assert!(err(r#"{"format":"dbeaver"}"#).contains("not an Alamo connections export"));
        assert!(err(r#"{"format":"alamo-connections","version":2}"#).contains("newer version"));

        let base = |profile: &str, ssh: &str| {
            format!(
                r#"{{"format":"alamo-connections","version":1,
                    "ssh_profiles":[{ssh}],"profiles":[{profile}]}}"#
            )
        };
        let p = |extra: &str| {
            format!(
                r#"{{"name":"x","dialect":"postgres","host":"h","port":5432,
                    "database":"d","username":"u","sslmode":"prefer"{extra}}}"#
            )
        };
        let s = |auth: &str| {
            format!(
                r#"{{"ref":"s1","name":"b","host":"h","port":22,"username":"u","auth":"{auth}"}}"#
            )
        };
        assert!(parse(base(&p(""), "").as_bytes(), &Drivers::builtin()).is_ok());
        let bad_dialect = p("").replace("postgres", "oracle");
        assert!(err(&base(&bad_dialect, "")).contains("unsupported"));
        let port0 = p("").replace("5432", "0");
        assert!(err(&base(&port0, "")).contains("port"));
        let blank = p("").replace("\"x\"", "\"  \"");
        assert!(err(&base(&blank, "")).contains("name"));
        assert!(err(&base(&p(r#","ssh":"s9""#), &s("key"))).contains("s9"));
        assert!(err(&base(&p(""), &s("kerberos"))).contains("kerberos"));
        let duplicate = format!("{},{}", s("key"), s("agent"));
        assert!(err(&base(&p(""), &duplicate)).contains("duplicate"));
        let long = p("").replace("\"d\"", &format!("\"{}\"", "x".repeat(5000)));
        assert!(err(&base(&long, "")).contains("too long"));
    }

    #[test]
    fn harmless_values_corrected() {
        let json = r##"{"format":"alamo-connections","version":1,"profiles":[
            {"name":" spaces ","dialect":"mysql","host":"h","port":3306,"database":"d",
             "username":"u","sslmode":"meh","color":"red","ssl_root_cert":"  "}]}"##;
        let doc = parse(json.as_bytes(), &Drivers::builtin()).unwrap();
        let p = &doc.profiles[0];
        assert_eq!(p.name, "spaces");
        assert_eq!(p.sslmode, "prefer");
        assert_eq!(p.color, None);
        assert_eq!(p.ssl_root_cert, None);
    }

    #[test]
    fn unique_names() {
        let mut taken = names(["Prod", "prod (2)"].into_iter());
        assert_eq!(unique_name("prod", &mut taken), "prod (3)");
        assert_eq!(unique_name("prod", &mut taken), "prod (4)");
        assert_eq!(unique_name("test", &mut taken), "test");
    }

    #[test]
    fn rfc3339_date() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_792_500_123), "2026-10-20T12:42:03Z");
    }
}
