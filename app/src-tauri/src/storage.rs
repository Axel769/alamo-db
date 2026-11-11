//! Local persistence on SQLite (§9.4): connection profiles (§9.1), editor
//! autosave. Lives in the data directory derived from the Tauri identifier
//! (§13.1) — never hand-written paths.
//!
//! **No passwords on disk**: they live in the system keychain (M5, §12,
//! `secrets` module) or only in memory — never in here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use alamo_core::tls::{ClientIdentity, TlsMode, TlsOptions};
use alamo_session::driver::ConnectTarget;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Connection profile (§9.1): host, port, user, sslmode, color/label.
/// `is_production` drives the color that pervades the UI (§9.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: Option<i64>,
    pub name: String,
    /// `postgres` or `mysql` (MariaDB included: recognized at the handshake,
    /// §15.1). In profiles written before M3 the field is missing: default
    /// `postgres` both in serde and in the schema.
    #[serde(default = "default_dialect")]
    pub dialect: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    /// TLS mode (§12): `disable`/`prefer`/`require`/`verify-ca`/
    /// `verify-full`, semantics in `alamo_core::tls`. Unknown values
    /// (hand-corrupted profiles) degrade to `prefer` in `tls_options`.
    pub sslmode: String,
    /// Custom root CA for the verifying modes (path of a PEM, §12).
    #[serde(default)]
    pub ssl_root_cert: Option<String>,
    /// Client certificate and its private key (PEM paths, may be the same
    /// file) for servers that authenticate by certificate (mTLS, §12.3).
    /// The key passphrase lives in the keychain, never here.
    #[serde(default)]
    pub ssl_client_cert: Option<String>,
    #[serde(default)]
    pub ssl_client_key: Option<String>,
    pub color: Option<String>,
    pub is_production: bool,
    /// §9.7 point 4: read-only, complementary to `is_production` — manual
    /// commit protects from distraction, this from the case where nothing
    /// should have been written at all. Serde default: profiles written
    /// before migration 2.
    #[serde(default)]
    pub read_only: bool,
    /// SSH tunnel (§12): reference to a saved SSH profile (migration 5) —
    /// the bastion is an entity of its own, shared by several database
    /// profiles. `None` = direct connection.
    #[serde(default)]
    pub ssh_profile_id: Option<i64>,
    /// Filter on the databases/schemas shown (migration 8, `dbfilter`
    /// module): comma-separated masks. Empty = all.
    #[serde(default)]
    pub db_include: String,
    #[serde(default)]
    pub db_exclude: String,
    /// Per-connection overrides of the overridable settings (§16): a
    /// partial `category.field` → value object, kept as given. Empty = the
    /// global settings. Stored as `settings_json` (migration 12).
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// SSH profile (§12): a bastion saved once and reused by several database
/// profiles. The credential (password or key passphrase) lives in the
/// keychain with an account tied to THIS id — shared by all the connections
/// that go through here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshProfile {
    pub id: Option<i64>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// `agent` | `password` | `key`.
    pub auth: String,
    pub key_path: Option<String>,
}

fn default_dialect() -> String {
    "postgres".into()
}

pub fn open(dir: &Path) -> Result<Connection, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let db = Connection::open(dir.join("alamo.db")).map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA journal_mode = WAL;")
        .map_err(|e| e.to_string())?;
    migrate(&db).map_err(|e| e.to_string())?;
    Ok(db)
}

/// Migration protocol (`PRAGMA user_version`): `user_version` = number of
/// `MIGRATIONS` steps already applied. 0 = DB never seen or created before
/// versioning: all steps run. Each step runs in a transaction with a bump of
/// `user_version` at the end — a crash halfway leaves the DB at the previous
/// step and it resumes at the next start.
///
/// Step 1 is the bootstrap and MUST stay idempotent (IF NOT EXISTS,
/// `pragma_table_info` probes): DBs created before versioning have
/// `user_version` 0 while already being at the current schema. From step 2
/// on, clean steps, no probes: the starting state is guaranteed by the
/// previous step. Never reorder or modify already published steps: only
/// append.
const MIGRATIONS: &[fn(&Connection) -> rusqlite::Result<()>] = &[
    migration_1_bootstrap,
    migration_2_read_only,
    migration_3_ssl_root_cert,
    migration_4_ssh_tunnel,
    migration_5_ssh_profiles,
    migration_6_ssh_host_key,
    migration_7_secret_tag,
    migration_8_db_filter,
    migration_9_table_sorts,
    migration_10_default_sort_only,
    migration_11_tree_favorites,
    migration_12_profile_settings,
    migration_13_client_cert,
];

/// Step 1: initial schema (M1-M3). Idempotent by construction, see the
/// comment on `MIGRATIONS`.
fn migration_1_bootstrap(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS profiles (
             id            INTEGER PRIMARY KEY,
             name          TEXT NOT NULL,
             host          TEXT NOT NULL,
             port          INTEGER NOT NULL,
             database      TEXT NOT NULL,
             username      TEXT NOT NULL,
             sslmode       TEXT NOT NULL DEFAULT 'prefer',
             color         TEXT,
             is_production INTEGER NOT NULL DEFAULT 0,
             last_used_at  INTEGER
         );
         CREATE TABLE IF NOT EXISTS autosave (
             buffer     TEXT PRIMARY KEY,
             sql        TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS kv (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );",
    )?;
    // M3: dialect column on existing profiles. Hand-made idempotent ALTER:
    // SQLite has no IF NOT EXISTS on columns.
    let has_dialect: bool = db
        .prepare("SELECT 1 FROM pragma_table_info('profiles') WHERE name = 'dialect'")?
        .exists([])?;
    if !has_dialect {
        db.execute_batch(
            "ALTER TABLE profiles ADD COLUMN dialect TEXT NOT NULL DEFAULT 'postgres';",
        )?;
    }
    Ok(())
}

/// Step 2 (M4): read-only flag on profiles (§9.7 point 4). Clean step, no
/// probes: the starting state is guaranteed by step 1.
fn migration_2_read_only(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("ALTER TABLE profiles ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0;")
}

/// Step 3 (M5): custom root CA for the verifying TLS modes (§12).
fn migration_3_ssl_root_cert(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("ALTER TABLE profiles ADD COLUMN ssl_root_cert TEXT;")
}

/// Step 4 (M5): inline SSH tunnel configuration on the profile (§12).
/// Superseded by step 5 (shared SSH profiles) but stays as it was: never
/// modify already published steps.
fn migration_4_ssh_tunnel(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "ALTER TABLE profiles ADD COLUMN ssh_enabled INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE profiles ADD COLUMN ssh_host TEXT NOT NULL DEFAULT '';
         ALTER TABLE profiles ADD COLUMN ssh_port INTEGER NOT NULL DEFAULT 22;
         ALTER TABLE profiles ADD COLUMN ssh_user TEXT NOT NULL DEFAULT '';
         ALTER TABLE profiles ADD COLUMN ssh_auth TEXT NOT NULL DEFAULT 'agent';
         ALTER TABLE profiles ADD COLUMN ssh_key_path TEXT;",
    )
}

/// Step 5 (M5): the bastion becomes an entity saved on its own
/// (`ssh_profiles`), shared by several database profiles — the user writes
/// it only once. The inline data of step 4 is carried over (one SSH profile
/// for each db profile that had the tunnel, name `user@host`) and the inline
/// columns are dropped.
fn migration_5_ssh_profiles(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE ssh_profiles (
             id       INTEGER PRIMARY KEY,
             name     TEXT NOT NULL,
             host     TEXT NOT NULL,
             port     INTEGER NOT NULL DEFAULT 22,
             username TEXT NOT NULL,
             auth     TEXT NOT NULL DEFAULT 'agent',
             key_path TEXT
         );
         ALTER TABLE profiles ADD COLUMN ssh_profile_id INTEGER REFERENCES ssh_profiles(id);",
    )?;
    // Carry-over: a loop in Rust because each row must receive the id of the
    // SSH profile just inserted.
    let rows: Vec<(i64, String, u16, String, String, Option<String>)> = db
        .prepare(
            "SELECT id, ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path
             FROM profiles WHERE ssh_enabled = 1",
        )?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    for (profile_id, host, port, user, auth, key_path) in rows {
        db.execute(
            "INSERT INTO ssh_profiles (name, host, port, username, auth, key_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![format!("{user}@{host}"), host, port, user, auth, key_path],
        )?;
        db.execute(
            "UPDATE profiles SET ssh_profile_id = ?2 WHERE id = ?1",
            params![profile_id, db.last_insert_rowid()],
        )?;
    }
    db.execute_batch(
        "ALTER TABLE profiles DROP COLUMN ssh_enabled;
         ALTER TABLE profiles DROP COLUMN ssh_host;
         ALTER TABLE profiles DROP COLUMN ssh_port;
         ALTER TABLE profiles DROP COLUMN ssh_user;
         ALTER TABLE profiles DROP COLUMN ssh_auth;
         ALTER TABLE profiles DROP COLUMN ssh_key_path;",
    )
}

/// Step 6: bastion fingerprint confirmed by the user (§12), public key in
/// OpenSSH format. Lives with the SSH profile: deleting the profile takes it
/// away, no orphans.
fn migration_6_ssh_host_key(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("ALTER TABLE ssh_profiles ADD COLUMN host_key TEXT;")
}

/// Step 7: random tag that names the keychain entry
/// (`secrets::db_account`/`ssh_account`). With the id alone (rowid,
/// reusable) a new profile could find the password of another server: after
/// a recreated app DB (the keychain survives) or a failed `forget`. It is
/// regenerated when the server changes (see `profile_save`). Existing rows
/// stay NULL = legacy name by id, until they change server: they belong to
/// this DB, so their entry is theirs.
fn migration_7_secret_tag(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "ALTER TABLE profiles ADD COLUMN secret_tag TEXT;
         ALTER TABLE ssh_profiles ADD COLUMN secret_tag TEXT;",
    )
}

/// Step 8: per-profile database filter (same server split into prod/test
/// profiles, like in DBeaver).
fn migration_8_db_filter(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "ALTER TABLE profiles ADD COLUMN db_include TEXT NOT NULL DEFAULT '';
         ALTER TABLE profiles ADD COLUMN db_exclude TEXT NOT NULL DEFAULT '';",
    )
}

/// Step 9: saved sorts of the table tab, with a name, per profile + schema +
/// table. At most one per table is the default, applied when the table
/// opens. `order_json` = list of `{column, desc}` (the same shape as
/// `TableQuery.order_by`).
fn migration_9_table_sorts(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE table_sorts (
             profile_id  INTEGER NOT NULL,
             schema_name TEXT NOT NULL,
             table_name  TEXT NOT NULL,
             name        TEXT NOT NULL,
             order_json  TEXT NOT NULL,
             is_default  INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (profile_id, schema_name, table_name, name)
         );",
    )
}

/// Step 10: no named sorts, only the table's default (usage choice: one is
/// enough). The default of step 9, if there was one, is carried over; the
/// others are left behind.
fn migration_10_default_sort_only(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE table_default_sorts (
             profile_id  INTEGER NOT NULL,
             schema_name TEXT NOT NULL,
             table_name  TEXT NOT NULL,
             order_json  TEXT NOT NULL,
             PRIMARY KEY (profile_id, schema_name, table_name)
         );
         INSERT INTO table_default_sorts (profile_id, schema_name, table_name, order_json)
             SELECT profile_id, schema_name, table_name, order_json
             FROM table_sorts WHERE is_default = 1;
         DROP TABLE table_sorts;",
    )
}

/// Step 11: tree favorites (star, like HeidiSQL). Empty `table_name` = the
/// whole database/schema.
fn migration_12_profile_settings(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("ALTER TABLE profiles ADD COLUMN settings_json TEXT NOT NULL DEFAULT '{}';")
}

/// Step 13: client certificate for mTLS (§12.3).
fn migration_13_client_cert(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "ALTER TABLE profiles ADD COLUMN ssl_client_cert TEXT;
         ALTER TABLE profiles ADD COLUMN ssl_client_key TEXT;",
    )
}

fn migration_11_tree_favorites(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE tree_favorites (
             profile_id  INTEGER NOT NULL,
             schema_name TEXT NOT NULL,
             table_name  TEXT NOT NULL,
             PRIMARY KEY (profile_id, schema_name, table_name)
         );",
    )
}

/// New tag: 128 random bits, generated by SQLite.
const NEW_SECRET_TAG: &str = "lower(hex(randomblob(16)))";

pub(crate) fn migrate(db: &Connection) -> rusqlite::Result<()> {
    let applied: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, step) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        // `user_version` sits in the DB header and is transactional: step
        // and bump commit (or fail) together.
        let tx = db.unchecked_transaction()?;
        step(&tx)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

fn row_to_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: Some(row.get(0)?),
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        database: row.get(4)?,
        username: row.get(5)?,
        sslmode: row.get(6)?,
        color: row.get(7)?,
        is_production: row.get(8)?,
        dialect: row.get(9)?,
        read_only: row.get(10)?,
        ssl_root_cert: row.get(11)?,
        ssh_profile_id: row.get(12)?,
        db_include: row.get(13)?,
        db_exclude: row.get(14)?,
        settings: {
            let raw: String = row.get(15)?;
            serde_json::from_str(&raw).unwrap_or_default()
        },
        ssl_client_cert: row.get(16)?,
        ssl_client_key: row.get(17)?,
    })
}

const PROFILE_COLS: &str = "id, name, host, port, database, username, sslmode, color, \
                            is_production, dialect, read_only, ssl_root_cert, ssh_profile_id, \
                            db_include, db_exclude, settings_json, ssl_client_cert, \
                            ssl_client_key";

pub fn profiles_list(db: &Connection) -> rusqlite::Result<Vec<Profile>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {PROFILE_COLS} FROM profiles ORDER BY COALESCE(last_used_at, 0) DESC, name"
    ))?;
    let rows = stmt.query_map([], row_to_profile)?;
    rows.collect()
}

pub fn profile_get(db: &Connection, id: i64) -> rusqlite::Result<Option<Profile>> {
    db.query_row(
        &format!("SELECT {PROFILE_COLS} FROM profiles WHERE id = ?1"),
        params![id],
        row_to_profile,
    )
    .optional()
}

/// Upsert: `id` present updates, absent inserts. Returns the id.
///
/// Server changed (host, port, user or dialect) = new keychain tag: the
/// saved password was for the other server and must not go out to this one.
/// The caller deletes the old entry.
/// Longest profile name, in characters: the dialog stops there, an import
/// is cut there. A label of a 240 px panel, not a description.
pub const PROFILE_NAME_MAX: usize = 120;

pub fn profile_save(db: &Connection, p: &Profile) -> rusqlite::Result<i64> {
    let name: String = p.name.chars().take(PROFILE_NAME_MAX).collect();
    match p.id {
        Some(id) => {
            db.execute(
                &format!(
                    "UPDATE profiles SET name=?2, host=?3, port=?4, database=?5, username=?6,
                                     sslmode=?7, color=?8, is_production=?9, dialect=?10,
                                     read_only=?11, ssl_root_cert=?12, ssh_profile_id=?13,
                                     db_include=?14, db_exclude=?15, settings_json=?16,
                                     ssl_client_cert=?17, ssl_client_key=?18,
                                     secret_tag = CASE WHEN host = ?3 AND port = ?4
                                                        AND username = ?6 AND dialect = ?10
                                                       THEN secret_tag
                                                       ELSE {NEW_SECRET_TAG} END
                 WHERE id=?1"
                ),
                params![
                    id,
                    name,
                    p.host,
                    p.port,
                    p.database,
                    p.username,
                    p.sslmode,
                    p.color,
                    p.is_production,
                    p.dialect,
                    p.read_only,
                    p.ssl_root_cert,
                    p.ssh_profile_id,
                    p.db_include,
                    p.db_exclude,
                    serde_json::Value::Object(p.settings.clone()).to_string(),
                    p.ssl_client_cert,
                    p.ssl_client_key
                ],
            )?;
            Ok(id)
        }
        None => {
            db.execute(
                &format!(
                    "INSERT INTO profiles (name, host, port, database, username, sslmode, color, is_production, dialect, read_only, ssl_root_cert, ssh_profile_id, db_include, db_exclude, settings_json, ssl_client_cert, ssl_client_key, secret_tag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, {NEW_SECRET_TAG})"
                ),
                params![
                    name,
                    p.host,
                    p.port,
                    p.database,
                    p.username,
                    p.sslmode,
                    p.color,
                    p.is_production,
                    p.dialect,
                    p.read_only,
                    p.ssl_root_cert,
                    p.ssh_profile_id,
                    p.db_include,
                    p.db_exclude,
                    serde_json::Value::Object(p.settings.clone()).to_string(),
                    p.ssl_client_cert,
                    p.ssl_client_key
                ],
            )?;
            Ok(db.last_insert_rowid())
        }
    }
}

/// Tag of the database profile's keychain entry; `None` = profile missing or
/// legacy (entry by id).
pub fn profile_secret_tag(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT secret_tag FROM profiles WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// Like `profile_secret_tag`, for the SSH profile.
pub fn ssh_secret_tag(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT secret_tag FROM ssh_profiles WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

pub fn profile_delete(db: &Connection, id: i64) -> rusqlite::Result<()> {
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM table_default_sorts WHERE profile_id = ?1",
        params![id],
    )?;
    tx.execute(
        "DELETE FROM tree_favorites WHERE profile_id = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
    tx.commit()
}

// ---------------------------------------------- default sort (table tab)

/// One column of the sort: the shape of `TableQuery.order_by`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortColumn {
    pub column: String,
    #[serde(default)]
    pub desc: bool,
}

/// A table of a profile: the key of the default sort.
pub struct TableKey<'a> {
    pub profile_id: i64,
    pub schema: &'a str,
    pub table: &'a str,
}

/// Default sort of the table, applied when it opens. `None` = none.
/// Unreadable JSON (hand-written in the file?) counts as none, not as an
/// error.
pub fn table_default_sort_get(
    db: &Connection,
    k: &TableKey,
) -> rusqlite::Result<Option<Vec<SortColumn>>> {
    let json: Option<String> = db
        .query_row(
            "SELECT order_json FROM table_default_sorts
             WHERE profile_id = ?1 AND schema_name = ?2 AND table_name = ?3",
            params![k.profile_id, k.schema, k.table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(json
        .and_then(|j| serde_json::from_str::<Vec<SortColumn>>(&j).ok())
        .filter(|o| !o.is_empty()))
}

/// Sets (or overwrites) the default; empty = removes it.
pub fn table_default_sort_set(
    db: &Connection,
    k: &TableKey,
    order: &[SortColumn],
) -> rusqlite::Result<()> {
    if order.is_empty() {
        db.execute(
            "DELETE FROM table_default_sorts
             WHERE profile_id = ?1 AND schema_name = ?2 AND table_name = ?3",
            params![k.profile_id, k.schema, k.table],
        )?;
        return Ok(());
    }
    let json = serde_json::to_string(order).expect("serializing SortColumn");
    db.execute(
        "INSERT INTO table_default_sorts (profile_id, schema_name, table_name, order_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(profile_id, schema_name, table_name) DO UPDATE SET
             order_json = excluded.order_json",
        params![k.profile_id, k.schema, k.table, json],
    )?;
    Ok(())
}

// ------------------------------------------------------------- tree favorites

/// A favorite of a profile: `table` absent = the whole database/schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Favorite {
    pub schema: String,
    pub table: Option<String>,
}

pub fn favorites_list(db: &Connection, profile_id: i64) -> rusqlite::Result<Vec<Favorite>> {
    let mut st = db.prepare(
        "SELECT schema_name, table_name FROM tree_favorites
         WHERE profile_id = ?1 ORDER BY schema_name, table_name",
    )?;
    let rows = st.query_map(params![profile_id], |r| {
        let table: String = r.get(1)?;
        Ok(Favorite {
            schema: r.get(0)?,
            table: (!table.is_empty()).then_some(table),
        })
    })?;
    rows.collect()
}

/// Turns the star on (`on`) or off; repeating does not duplicate and turning
/// off what is not there is not an error.
pub fn favorite_set(
    db: &Connection,
    profile_id: i64,
    fav: &Favorite,
    on: bool,
) -> rusqlite::Result<()> {
    let table = fav.table.as_deref().unwrap_or("");
    let sql = if on {
        "INSERT INTO tree_favorites (profile_id, schema_name, table_name)
         VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM tree_favorites
         WHERE profile_id = ?1 AND schema_name = ?2 AND table_name = ?3"
    };
    db.execute(sql, params![profile_id, fav.schema, table])?;
    Ok(())
}

// ----------------------------------------------------------- SSH profiles (§12)

fn row_to_ssh_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<SshProfile> {
    Ok(SshProfile {
        id: Some(row.get(0)?),
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        username: row.get(4)?,
        auth: row.get(5)?,
        key_path: row.get(6)?,
    })
}

const SSH_COLS: &str = "id, name, host, port, username, auth, key_path";

pub fn ssh_profiles_list(db: &Connection) -> rusqlite::Result<Vec<SshProfile>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {SSH_COLS} FROM ssh_profiles ORDER BY name"
    ))?;
    let rows = stmt.query_map([], row_to_ssh_profile)?;
    rows.collect()
}

pub fn ssh_profile_get(db: &Connection, id: i64) -> rusqlite::Result<Option<SshProfile>> {
    db.query_row(
        &format!("SELECT {SSH_COLS} FROM ssh_profiles WHERE id = ?1"),
        params![id],
        row_to_ssh_profile,
    )
    .optional()
}

/// Upsert as for database profiles. Returns the id.
pub fn ssh_profile_save(db: &Connection, p: &SshProfile) -> rusqlite::Result<i64> {
    match p.id {
        Some(id) => {
            // Host or port changed = another server: the fingerprint confirmed
            // for the old one no longer holds (in the SET the column names are
            // the values BEFORE the update). With the user too the credential
            // changes: new keychain tag.
            db.execute(
                &format!(
                    "UPDATE ssh_profiles SET name=?2, host=?3, port=?4, username=?5, auth=?6,
                                         key_path=?7,
                                         host_key = CASE WHEN host = ?3 AND port = ?4
                                                         THEN host_key END,
                                         secret_tag = CASE WHEN host = ?3 AND port = ?4
                                                                AND username = ?5
                                                           THEN secret_tag
                                                           ELSE {NEW_SECRET_TAG} END
                 WHERE id=?1"
                ),
                params![id, p.name, p.host, p.port, p.username, p.auth, p.key_path],
            )?;
            Ok(id)
        }
        None => {
            db.execute(
                &format!(
                    "INSERT INTO ssh_profiles (name, host, port, username, auth, key_path, secret_tag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NEW_SECRET_TAG})"
                ),
                params![p.name, p.host, p.port, p.username, p.auth, p.key_path],
            )?;
            Ok(db.last_insert_rowid())
        }
    }
}

/// Confirmed bastion fingerprint (OpenSSH key), if any.
pub fn ssh_host_key_get(db: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT host_key FROM ssh_profiles WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

pub fn ssh_host_key_set(db: &Connection, id: i64, key: &str) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE ssh_profiles SET host_key = ?2 WHERE id = ?1",
        params![id, key],
    )?;
    Ok(())
}

/// The user no longer trusts the saved key: the next connection asks for
/// the fingerprint again (bastion reinstalled, key rotated on purpose).
pub fn ssh_host_key_forget(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE ssh_profiles SET host_key = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Names of the database profiles that go through each SSH profile: the
/// manager says who a change or a deletion touches.
pub fn ssh_profile_usage(db: &Connection) -> rusqlite::Result<HashMap<i64, Vec<String>>> {
    let mut stmt = db.prepare(
        "SELECT ssh_profile_id, name FROM profiles
         WHERE ssh_profile_id IS NOT NULL ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut usage: HashMap<i64, Vec<String>> = HashMap::new();
    for row in rows {
        let (sid, name) = row?;
        usage.entry(sid).or_default().push(name);
    }
    Ok(usage)
}

/// Deletes the SSH profile and detaches the database profiles that used it
/// (they go back to a direct connection: disappearing is better than
/// pointing into the void).
pub fn ssh_profile_delete(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE profiles SET ssh_profile_id = NULL WHERE ssh_profile_id = ?1",
        params![id],
    )?;
    db.execute("DELETE FROM ssh_profiles WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn profile_touch(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE profiles SET last_used_at = strftime('%s','now') WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn autosave_get(db: &Connection, buffer: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT sql FROM autosave WHERE buffer = ?1",
        params![buffer],
        |r| r.get(0),
    )
    .optional()
}

pub fn autosave_set(db: &Connection, buffer: &str, sql: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO autosave (buffer, sql, updated_at) VALUES (?1, ?2, strftime('%s','now'))
         ON CONFLICT(buffer) DO UPDATE SET sql = excluded.sql, updated_at = excluded.updated_at",
        params![buffer, sql],
    )?;
    Ok(())
}

pub fn autosave_delete(db: &Connection, buffer: &str) -> rusqlite::Result<()> {
    db.execute("DELETE FROM autosave WHERE buffer = ?1", params![buffer])?;
    Ok(())
}

pub fn kv_get(db: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    db.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
        r.get(0)
    })
    .optional()
}

pub fn kv_set(db: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn kv_delete(db: &Connection, key: &str) -> rusqlite::Result<()> {
    db.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
    Ok(())
}

/// Where the profile's driver connects (§2.2), with the TCP possibly going
/// through the SSH tunnel listener (§12): `tunnel_port` = port on 127.0.0.1.
pub fn connect_target<'a>(
    p: &'a Profile,
    password: &'a str,
    tunnel_port: Option<u16>,
) -> ConnectTarget<'a> {
    ConnectTarget {
        host: &p.host,
        port: p.port,
        user: &p.username,
        password,
        database: &p.database,
        tls: tls_mode(p),
        tunnel_port,
    }
}

/// An unknown `sslmode` (hand-corrupted profile) degrades to `prefer`, never
/// upwards: faking a verification the user did not ask for would be as wrong
/// as skipping one they asked for.
fn tls_mode(p: &Profile) -> TlsMode {
    TlsMode::parse(&p.sslmode).unwrap_or_default()
}

/// The profile's client certificate files (§12.3), `~/` expanded. `None`
/// = no mTLS, or a mode that does not use TLS at all. One field without
/// the other is an error: the server would refuse with a less clear one.
pub fn client_cert_paths(p: &Profile) -> Result<Option<(PathBuf, PathBuf)>, String> {
    if !tls_mode(p).wants_tls() {
        return Ok(None);
    }
    fn field(v: &Option<String>) -> Option<&str> {
        v.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
    match (field(&p.ssl_client_cert), field(&p.ssl_client_key)) {
        (None, None) => Ok(None),
        (Some(c), Some(k)) => Ok(Some((crate::expand_home(c), crate::expand_home(k)))),
        _ => Err(
            "client certificate: set both the certificate and its private key \
                  (the same file if it holds both)"
                .into(),
        ),
    }
}

/// TLS regime of the profile (§12), for the drivers' connector. `~/` in
/// the CA path as in the SSH key path: the field is typed by hand too.
/// `client_cert`: read by the caller (`alamo_session::client_cert`), which
/// knows the passphrase.
pub fn tls_options(p: &Profile, tunneled: bool, client_cert: Option<ClientIdentity>) -> TlsOptions {
    TlsOptions {
        mode: tls_mode(p),
        root_cert: p
            .ssl_root_cert
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(crate::expand_home),
        // SSH tunnel: the TCP points to 127.0.0.1 but the certificate must
        // be verified against the real name.
        tls_server_name: tunneled.then(|| p.host.clone()),
        client_cert,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        db
    }

    fn profile() -> Profile {
        Profile {
            id: None,
            name: "local".into(),
            host: "localhost".into(),
            port: 5432,
            database: "alamo_bench".into(),
            username: "postgres".into(),
            sslmode: "prefer".into(),
            ssl_root_cert: None,
            ssl_client_cert: None,
            ssl_client_key: None,
            color: Some("#a02a83".into()),
            is_production: false,
            dialect: "postgres".into(),
            read_only: false,
            ssh_profile_id: None,
            db_include: String::new(),
            db_exclude: String::new(),
            settings: serde_json::Map::new(),
        }
    }

    #[test]
    fn upsert_list_delete() {
        let db = mem();
        let id = profile_save(&db, &profile()).unwrap();
        let mut p = profile_get(&db, id).unwrap().unwrap();
        assert_eq!(p.name, "local");
        assert!(!p.read_only);
        assert_eq!(p.db_exclude, "");
        p.name = "local 2".into();
        p.read_only = true;
        p.db_exclude = "*_test*, *_demo*".into();
        assert_eq!(profile_save(&db, &p).unwrap(), id);
        assert_eq!(
            profile_get(&db, id).unwrap().unwrap().db_exclude,
            "*_test*, *_demo*"
        );
        assert_eq!(profiles_list(&db).unwrap().len(), 1);
        assert_eq!(profiles_list(&db).unwrap()[0].name, "local 2");
        assert!(profiles_list(&db).unwrap()[0].read_only);
        profile_delete(&db, id).unwrap();
        assert!(profile_get(&db, id).unwrap().is_none());
    }

    #[test]
    fn autosave_upsert() {
        let db = mem();
        assert_eq!(autosave_get(&db, "editor:0").unwrap(), None);
        autosave_set(&db, "editor:0", "SELECT 1;").unwrap();
        autosave_set(&db, "editor:0", "SELECT 2;").unwrap();
        assert_eq!(
            autosave_get(&db, "editor:0").unwrap().as_deref(),
            Some("SELECT 2;")
        );
    }

    #[test]
    fn migration_5_carries_over_inline_ssh() {
        let db = Connection::open_in_memory().unwrap();
        // Brings the DB to version 4 (inline tunnel) as if it were a real
        // install, then inserts a profile with the tunnel.
        for step in &MIGRATIONS[..4] {
            let tx = db.unchecked_transaction().unwrap();
            step(&tx).unwrap();
            tx.commit().unwrap();
        }
        db.execute_batch("PRAGMA user_version = 4").unwrap();
        db.execute(
            "INSERT INTO profiles (name, host, port, database, username, ssh_enabled,
                                   ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path)
             VALUES ('p', 'db-internal', 5432, 'd', 'u', 1,
                     'bastion', 2222, 'ale', 'key', '~/.ssh/id_ed25519')",
            [],
        )
        .unwrap();
        migrate(&db).unwrap();
        let profs = profiles_list(&db).unwrap();
        let sid = profs[0].ssh_profile_id.expect("profile not attached");
        let sp = ssh_profile_get(&db, sid).unwrap().unwrap();
        assert_eq!(sp.name, "ale@bastion");
        assert_eq!(sp.host, "bastion");
        assert_eq!(sp.port, 2222);
        assert_eq!(sp.username, "ale");
        assert_eq!(sp.auth, "key");
        assert_eq!(sp.key_path.as_deref(), Some("~/.ssh/id_ed25519"));
        // The inline columns are gone.
        let has_inline: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('profiles') WHERE name = 'ssh_host'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_inline);
    }

    #[test]
    fn ssh_profile_crud_and_delete_detaches() {
        let db = mem();
        let sid = ssh_profile_save(
            &db,
            &SshProfile {
                id: None,
                name: "bastion prod".into(),
                host: "b.example".into(),
                port: 22,
                username: "ale".into(),
                auth: "agent".into(),
                key_path: None,
            },
        )
        .unwrap();
        let mut p = profile();
        p.ssh_profile_id = Some(sid);
        let pid = profile_save(&db, &p).unwrap();
        assert_eq!(
            profile_get(&db, pid).unwrap().unwrap().ssh_profile_id,
            Some(sid)
        );
        // Update.
        let mut sp = ssh_profile_get(&db, sid).unwrap().unwrap();
        sp.port = 2222;
        assert_eq!(ssh_profile_save(&db, &sp).unwrap(), sid);
        assert_eq!(ssh_profile_get(&db, sid).unwrap().unwrap().port, 2222);
        // Fingerprint: stays if only the name changes, drops if the server
        // changes.
        ssh_host_key_set(&db, sid, "ssh-ed25519 AAAA").unwrap();
        sp.name = "renamed".into();
        ssh_profile_save(&db, &sp).unwrap();
        assert_eq!(
            ssh_host_key_get(&db, sid).unwrap().as_deref(),
            Some("ssh-ed25519 AAAA")
        );
        sp.host = "other.example".into();
        ssh_profile_save(&db, &sp).unwrap();
        assert_eq!(ssh_host_key_get(&db, sid).unwrap(), None);
        ssh_host_key_set(&db, sid, "ssh-ed25519 AAAA").unwrap();
        sp.port = 2223;
        ssh_profile_save(&db, &sp).unwrap();
        assert_eq!(ssh_host_key_get(&db, sid).unwrap(), None);
        // Keychain tag: new if the server or the user changes.
        let tag = ssh_secret_tag(&db, sid).unwrap().expect("tag");
        sp.name = "again".into();
        ssh_profile_save(&db, &sp).unwrap();
        assert_eq!(ssh_secret_tag(&db, sid).unwrap().as_ref(), Some(&tag));
        sp.username = "other".into();
        ssh_profile_save(&db, &sp).unwrap();
        assert_ne!(ssh_secret_tag(&db, sid).unwrap().as_ref(), Some(&tag));
        // Usage: who goes through the bastion, by name.
        let mut other = profile();
        other.name = "other".into();
        other.ssh_profile_id = Some(sid);
        profile_save(&db, &other).unwrap();
        profile_save(&db, &profile()).unwrap();
        let usage = ssh_profile_usage(&db).unwrap();
        assert_eq!(usage.get(&sid).unwrap(), &["local", "other"]);
        assert_eq!(usage.len(), 1);
        // Forgetting the key: the next connection asks again.
        ssh_host_key_set(&db, sid, "ssh-ed25519 AAAA").unwrap();
        ssh_host_key_forget(&db, sid).unwrap();
        assert_eq!(ssh_host_key_get(&db, sid).unwrap(), None);
        // Delete: the database profile goes back to a direct connection.
        ssh_profile_delete(&db, sid).unwrap();
        assert!(ssh_profile_usage(&db).unwrap().is_empty());
        assert!(ssh_profile_get(&db, sid).unwrap().is_none());
        assert_eq!(profile_get(&db, pid).unwrap().unwrap().ssh_profile_id, None);
    }

    #[test]
    fn connect_target_via_tunnel() {
        let p = profile();
        let t = connect_target(&p, "pw", Some(15432));
        assert_eq!(
            (t.host, t.port, t.tunnel_port),
            ("localhost", 5432, Some(15432))
        );
        // The real name travels in `tls_server_name`, only through a tunnel.
        assert_eq!(
            tls_options(&p, true, None).tls_server_name.as_deref(),
            Some("localhost")
        );
        assert!(tls_options(&p, false, None).tls_server_name.is_none());
    }

    #[test]
    fn tls_options_from_profile() {
        use alamo_core::tls::TlsMode;
        let mut p = profile();
        assert_eq!(tls_options(&p, false, None).mode, TlsMode::Prefer);
        assert!(tls_options(&p, false, None).root_cert.is_none());
        p.sslmode = "verify-ca".into();
        p.ssl_root_cert = Some("/etc/ca.pem".into());
        let t = tls_options(&p, false, None);
        assert_eq!(t.mode, TlsMode::VerifyCa);
        assert_eq!(
            t.root_cert.as_deref(),
            Some(std::path::Path::new("/etc/ca.pem"))
        );
        // Empty string = no CA, not a CA with path "".
        p.ssl_root_cert = Some("  ".into());
        assert!(tls_options(&p, false, None).root_cert.is_none());
        // Unknown: never upwards.
        p.sslmode = "unknown".into();
        assert_eq!(tls_options(&p, false, None).mode, TlsMode::Prefer);
    }

    #[test]
    fn client_cert_paths_from_profile() {
        let db = mem();
        let mut p = profile();
        assert_eq!(client_cert_paths(&p), Ok(None));
        p.ssl_client_cert = Some("/etc/client.pem".into());
        assert!(client_cert_paths(&p).unwrap_err().contains("both"));
        p.ssl_client_key = Some("  ".into());
        assert!(client_cert_paths(&p).is_err(), "blank key = missing key");
        p.ssl_client_key = Some("/etc/client-key.pem".into());
        assert_eq!(
            client_cert_paths(&p),
            Ok(Some((
                "/etc/client.pem".into(),
                "/etc/client-key.pem".into()
            )))
        );
        // No TLS, no certificate: the fields stay but are not used.
        p.sslmode = "disable".into();
        assert_eq!(client_cert_paths(&p), Ok(None));
        // Saved and read back.
        let id = profile_save(&db, &p).unwrap();
        let got = profile_get(&db, id).unwrap().unwrap();
        assert_eq!(got.ssl_client_cert.as_deref(), Some("/etc/client.pem"));
        assert_eq!(got.ssl_client_key.as_deref(), Some("/etc/client-key.pem"));
    }

    #[test]
    fn keychain_tag_per_server() {
        let db = mem();
        let id = profile_save(&db, &profile()).unwrap();
        let tag = profile_secret_tag(&db, id).unwrap().expect("tag");
        assert_eq!(tag.len(), 32);
        // Same server: the tag stays.
        let mut p = profile_get(&db, id).unwrap().unwrap();
        p.name = "renamed".into();
        p.database = "other_db".into();
        p.color = None;
        profile_save(&db, &p).unwrap();
        assert_eq!(profile_secret_tag(&db, id).unwrap().as_ref(), Some(&tag));
        // Different server: new tag, every time.
        let mut seen = vec![tag];
        for change in [
            |p: &mut Profile| p.host = "other.example".into(),
            |p: &mut Profile| p.port = 6543,
            |p: &mut Profile| p.username = "intruder".into(),
            |p: &mut Profile| p.dialect = "mysql".into(),
        ] {
            change(&mut p);
            profile_save(&db, &p).unwrap();
            let t = profile_secret_tag(&db, id).unwrap().expect("tag");
            assert!(!seen.contains(&t), "tag reused");
            seen.push(t);
        }
        // Id reused after deletion: different tag.
        profile_delete(&db, id).unwrap();
        assert_eq!(profile_secret_tag(&db, id).unwrap(), None);
        let id2 = profile_save(&db, &profile()).unwrap();
        assert_eq!(id2, id, "SQLite reuses the rowid");
        assert!(!seen.contains(&profile_secret_tag(&db, id2).unwrap().unwrap()));
        // Legacy row (before migration 7): NULL until the server changes.
        db.execute(
            "UPDATE profiles SET secret_tag = NULL WHERE id = ?1",
            params![id2],
        )
        .unwrap();
        let mut p = profile_get(&db, id2).unwrap().unwrap();
        p.name = "old".into();
        profile_save(&db, &p).unwrap();
        assert_eq!(profile_secret_tag(&db, id2).unwrap(), None);
        p.host = "new.example".into();
        profile_save(&db, &p).unwrap();
        assert!(profile_secret_tag(&db, id2).unwrap().is_some());
    }

    #[test]
    fn default_sort_one_per_table() {
        let db = mem();
        let pid = profile_save(&db, &profile()).unwrap();
        let k = TableKey {
            profile_id: pid,
            schema: "public",
            table: "orders",
        };
        let col = |c: &str, desc| SortColumn {
            column: c.into(),
            desc,
        };
        assert_eq!(table_default_sort_get(&db, &k).unwrap(), None);
        table_default_sort_set(&db, &k, &[col("date", true), col("id", false)]).unwrap();
        assert_eq!(
            table_default_sort_get(&db, &k).unwrap(),
            Some(vec![col("date", true), col("id", false)])
        );
        // Overwrites.
        table_default_sort_set(&db, &k, &[col("customer", false)]).unwrap();
        assert_eq!(
            table_default_sort_get(&db, &k).unwrap(),
            Some(vec![col("customer", false)])
        );
        // Another table: none.
        let other = TableKey { table: "rows", ..k };
        assert_eq!(table_default_sort_get(&db, &other).unwrap(), None);
        // Empty = removed.
        table_default_sort_set(&db, &k, &[]).unwrap();
        assert_eq!(table_default_sort_get(&db, &k).unwrap(), None);
        // Deleting the profile takes its defaults away.
        table_default_sort_set(&db, &k, &[col("id", true)]).unwrap();
        profile_delete(&db, pid).unwrap();
        assert_eq!(table_default_sort_get(&db, &k).unwrap(), None);
    }

    #[test]
    fn favorites_per_profile_databases_and_tables() {
        let db = mem();
        let pid = profile_save(&db, &profile()).unwrap();
        let other = profile_save(
            &db,
            &Profile {
                name: "other".into(),
                ..profile()
            },
        )
        .unwrap();
        let fav = |s: &str, t: Option<&str>| Favorite {
            schema: s.into(),
            table: t.map(Into::into),
        };
        assert_eq!(favorites_list(&db, pid).unwrap(), vec![]);
        favorite_set(&db, pid, &fav("shop", None), true).unwrap();
        favorite_set(&db, pid, &fav("crm", Some("customers")), true).unwrap();
        // Repeating does not duplicate.
        favorite_set(&db, pid, &fav("crm", Some("customers")), true).unwrap();
        // The database and its table are distinct favorites.
        favorite_set(&db, pid, &fav("shop", Some("orders")), true).unwrap();
        assert_eq!(
            favorites_list(&db, pid).unwrap(),
            vec![
                fav("crm", Some("customers")),
                fav("shop", None),
                fav("shop", Some("orders")),
            ]
        );
        // Another profile does not see them.
        assert_eq!(favorites_list(&db, other).unwrap(), vec![]);
        // Turning off the database leaves the table; turning off a missing
        // one is fine.
        favorite_set(&db, pid, &fav("shop", None), false).unwrap();
        favorite_set(&db, pid, &fav("nobody", None), false).unwrap();
        assert_eq!(
            favorites_list(&db, pid).unwrap(),
            vec![fav("crm", Some("customers")), fav("shop", Some("orders"))]
        );
        // Deleting the profile takes its favorites away.
        profile_delete(&db, pid).unwrap();
        assert_eq!(favorites_list(&db, pid).unwrap(), vec![]);
    }

    #[test]
    fn migration_10_carries_over_the_step_9_default() {
        let db = Connection::open_in_memory().unwrap();
        for step in &MIGRATIONS[..9] {
            step(&db).unwrap();
        }
        db.execute_batch(
            "INSERT INTO table_sorts VALUES (1, 's', 't', 'by date', '[{\"column\":\"date\",\"desc\":true}]', 1);
             INSERT INTO table_sorts VALUES (1, 's', 't', 'altro', '[{\"column\":\"id\"}]', 0);",
        )
        .unwrap();
        MIGRATIONS[9](&db).unwrap();
        let k = TableKey {
            profile_id: 1,
            schema: "s",
            table: "t",
        };
        assert_eq!(
            table_default_sort_get(&db, &k).unwrap(),
            Some(vec![SortColumn {
                column: "date".into(),
                desc: true
            }])
        );
    }

    #[test]
    fn migrate_bumps_user_version_and_is_idempotent() {
        let db = Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        let v: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        // Second run: no step to re-run, version unchanged.
        migrate(&db).unwrap();
        let v2: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v2 as usize, MIGRATIONS.len());
    }

    #[test]
    fn migration_adds_dialect_to_old_profiles() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE profiles (
                 id            INTEGER PRIMARY KEY,
                 name          TEXT NOT NULL,
                 host          TEXT NOT NULL,
                 port          INTEGER NOT NULL,
                 database      TEXT NOT NULL,
                 username      TEXT NOT NULL,
                 sslmode       TEXT NOT NULL DEFAULT 'prefer',
                 color         TEXT,
                 is_production INTEGER NOT NULL DEFAULT 0,
                 last_used_at  INTEGER
             );
             INSERT INTO profiles (name, host, port, database, username)
             VALUES ('vecchio', 'h', 5432, 'db', 'u');",
        )
        .unwrap();
        migrate(&db).unwrap();
        let p = profiles_list(&db).unwrap();
        assert_eq!(p[0].dialect, "postgres");
        // Migration 2 adds read_only to pre-versioning DBs too.
        assert!(!p[0].read_only);
    }
}
