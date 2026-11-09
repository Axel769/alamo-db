//! Tauri commands: only serialization and wiring (§2), the logic lives in
//! `alamo-session`. IPC surface of §8: the full result set stays in Rust
//! indexed by an id, the frontend asks only for the visible window.
//!
//! The `bench_*` and `report_*` commands serve the M0 harness
//! (`ALAMO_M0_AUTO=1`) and use `execute_collect` to get the precise
//! statistics of §15.

// Commands return `ErrorOut`, the error the frontend receives serialized: it
// is big (code, detail, position…) but it crosses the IPC once per failed
// command. Boxing it in every signature saves nothing measurable.
#![allow(clippy::result_large_err)]

mod abandoned;
mod dbfilter;
#[cfg(feature = "e2e")]
mod e2e;
mod secrets;
mod settings;
mod storage;
mod transfer;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use alamo_session::driver::{Driver, Drivers};
use alamo_session::events::{ErrorOut, ExecEvent};
use alamo_session::Session;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use zeroize::Zeroizing;

use alamo_driver_postgres::execute::{execute_collect, FetchOptions};

/// What an extension build adds to the app (§13): the Community binary
/// passes none. The extension's own binary calls `run` with its plugin; a
/// plugin reaches the open connections through `AppState::session`.
#[derive(Default)]
pub struct Extensions {
    /// Engines on top of the built-in ones (§2.2).
    pub drivers: Vec<Arc<dyn Driver>>,
    /// Tauri plugins: their commands, in their own namespace.
    pub plugins: Vec<Box<dyn tauri::plugin::Plugin<tauri::Wry>>>,
}

/// M3.5: several connections open at once (DBeaver model). Each `Session`
/// is self-contained (result registry, introspection connection); only the
/// id → session map lives here. Ids are never reused.
pub struct AppState {
    started: Instant,
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// Database engines of this build (§2.2), by the profile's `dialect`.
    drivers: Drivers,
    /// Profile of each profile connection, by conn_id: when it closes, its
    /// server connections are kept for the next connect (`abandoned`).
    profiles: Mutex<HashMap<u64, i64>>,
    /// Active SSH tunnels, by conn_id (§12): the tunnel lives as long as its
    /// session — removed (and therefore closed) at disconnect/terminate.
    tunnels: Mutex<HashMap<u64, TunnelSlot>>,
    /// Database filter of the profile, by conn_id: fixed at connect like the
    /// rest of the profile (prod, read-only). Absent = no filter.
    db_filters: Mutex<HashMap<u64, dbfilter::DbFilter>>,
    /// PG (one database per connection, §3.3): by conn_id of the profile's
    /// connection, what opening another database of the same server needs.
    /// Password in memory only, like the session's URL (§12).
    reopen: Mutex<HashMap<u64, Reopen>>,
    /// Connections to the other databases of a profile (§3.3), by their
    /// conn_id: the profile's connection and the database. They share its
    /// tunnel and close with it.
    siblings: Mutex<HashMap<u64, (u64, String)>>,
    next_conn_id: AtomicU64,
    /// Close confirmed by the "pending writes" dialog (§9.7.2): the
    /// CloseRequested/ExitRequested gates let it through.
    quit_confirmed: AtomicBool,
    /// Unsaved grid edits in some tab (only the frontend knows them, and it
    /// reports them with `set_unsaved_edits`): they stop the close too.
    unsaved_edits: AtomicBool,
    /// Host keys of never-seen bastions, by SSH profile id (§12): the tunnel
    /// rejected them and the frontend asks the user whether to trust them.
    /// `ssh_host_key_trust` saves ONLY the key the backend saw here, and
    /// only if the confirmed fingerprint is the same: the frontend never
    /// chooses which key to save.
    pending_host_keys: Mutex<HashMap<i64, PendingHostKey>>,
}

/// The profile as connected and the password actually used (§3.3).
struct Reopen {
    profile: storage::Profile,
    password: String,
}

struct PendingHostKey {
    fingerprint: String,
    key: String,
}

/// Uncommitted writes on some connection (§9.7.2): closing the app now would
/// be a silent ROLLBACK — the close must be confirmed. Same for grid edits
/// not yet saved.
fn writes_pending(state: &AppState) -> bool {
    state.unsaved_edits.load(Ordering::Relaxed)
        || locked(&state.sessions)
            .values()
            .any(|s| s.txn_state().has_writes)
}

/// A signal to the frontend as a DOM event (`alamo:*`) via `eval`, not
/// `emit`: the app has no capability ACL and without
/// `core:event:allow-listen` the JS `listen` is refused — the signal would
/// never arrive.
fn dom_event(app: &AppHandle, name: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval(format!("window.dispatchEvent(new Event('{name}'))"));
    }
}

/// Close stopped by the gate: tells the frontend, which shows the dialog
/// (§9.7.2) — otherwise the red button would stay mute.
fn signal_quit_blocked(app: &AppHandle) {
    dom_event(app, "alamo:quit-blocked");
}

/// §9.7.2: every 15 s, ROLLBACK of the transactions idle for the session's
/// `idle_rollback` (a setting, §16), on every connection — MySQL has no
/// server-side net. The UI learns it from a DOM event (same reason as
/// `signal_quit_blocked`) and says whether writes were lost.
fn watch_idle_transactions(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tick.tick().await;
            let sessions: Vec<(u64, Arc<Session>)> = locked(&app.state::<AppState>().sessions)
                .iter()
                .map(|(id, s)| (*id, Arc::clone(s)))
                .collect();
            for (conn_id, session) in sessions {
                let Ok(Some(before)) = session.rollback_if_idle(session.idle_rollback()).await
                else {
                    continue;
                };
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.eval(format!(
                        "window.dispatchEvent(new CustomEvent('alamo:txn-idle-rollback', \
                         {{ detail: {{ connId: {conn_id}, writes: {} }} }}))",
                        before.has_writes
                    ));
                }
            }
        }
    });
}

/// The frontend reports whether there are unsaved grid edits: the close gate
/// treats them like uncommitted writes.
#[tauri::command]
fn set_unsaved_edits(state: State<'_, AppState>, on: bool) {
    state.unsaved_edits.store(on, Ordering::Relaxed);
}

/// Local SQLite (§9.4): profiles and autosave. Mutex and not a pool: accesses
/// are short and rare, all from the command thread.
struct Db(Mutex<rusqlite::Connection>);

fn internal(msg: impl Into<String>) -> ErrorOut {
    ErrorOut::from(&alamo_core::error::DbError::internal(msg))
}

/// `~/` in the SSH key path: the field is free text and the shortcut is the
/// one everybody types. Only the prefix, as the shell does.
fn expand_home(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        if let Some(home) = home {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// Lock that ignores poisoning: a panic while the lock is held must not
/// poison the mutex and make all later commands panic in cascade. Safe here:
/// the critical sections on `sessions` and `Db` are short and without
/// multi-step invariants, so the protected data stays consistent even if the
/// thread holding the lock died halfway.
fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AppState {
    pub fn session(&self, conn_id: u64) -> Result<Arc<Session>, String> {
        locked(&self.sessions)
            .get(&conn_id)
            .cloned()
            .ok_or_else(|| format!("connection {conn_id} missing or closed"))
    }

    /// Session on a profile: its engine's driver (§2.2) writes the
    /// connection string. §9.7: the prod flag enters the session —
    /// auto-commit off on the sticky, implicit BEGIN, explicit
    /// commit/rollback; the read-only flag sets server-side read-only
    /// transactions on every lane. §12: the TLS regime travels next to the
    /// string, on every lane.
    /// Opens a session with the effective Execution settings (§16): the
    /// global ones with the profile's overrides applied. Returns the
    /// session, those settings (the frontend shows them, never merges) and
    /// the override problems (unknown or not overridable), which the
    /// caller shows once at connect.
    async fn open_session(
        &self,
        profile: &storage::Profile,
        password: &str,
        tunnel_port: Option<u16>,
        global: &settings::Settings,
    ) -> Result<(Session, settings::Execution, Vec<settings::Issue>), ErrorOut> {
        let driver = self
            .drivers
            .get(&profile.dialect)
            .ok_or_else(|| internal(format!("unsupported engine «{}»", profile.dialect)))?;
        let url = driver.conn_string(&storage::connect_target(profile, password, tunnel_port));
        let session = Session::connect_with(
            driver,
            &url,
            profile.is_production,
            profile.read_only,
            storage::tls_options(profile, tunnel_port.is_some()),
            global.connections.net(),
        )
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
        let (eff, warnings) = settings::effective(global, &profile.settings);
        let exec = eff.execution;
        session.set_limits(exec.limits());
        // The statement timeout (§16) is a server SET, not just an atomic:
        // apply it to the sticky now (the lazy lanes pick it up at open).
        session
            .apply_query_timeout()
            .await
            .map_err(|e| ErrorOut::from(&*e))?;
        if exec.manual_commit_default && !profile.is_production && !profile.read_only {
            session
                .set_manual_commit(true)
                .map_err(|e| ErrorOut::from(&*e))?;
        }
        Ok((session, exec, warnings))
    }

    fn insert(&self, session: Session) -> u64 {
        let id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        locked(&self.sessions).insert(id, Arc::new(session));
        id
    }

    /// The profile's connection a connection belongs to: itself, or the one
    /// it was opened from on another database (§3.3). Owner of the tunnel.
    fn root(&self, conn_id: u64) -> u64 {
        locked(&self.siblings)
            .get(&conn_id)
            .map_or(conn_id, |(root, _)| *root)
    }

    /// Forgets a closed connection; the profile's connection takes its
    /// siblings with it. Dropping the sessions closes them, and the tunnel;
    /// what the server may not have noticed is kept (`remember_abandoned`).
    fn forget(&self, db: &Db, conn_id: u64) {
        self.remember_abandoned(db, conn_id);
        locked(&self.profiles).remove(&conn_id);
        let mut gone = vec![conn_id];
        locked(&self.siblings).retain(|id, (root, _)| {
            let child = *root == conn_id;
            if child {
                gone.push(*id);
            }
            !child && *id != conn_id
        });
        locked(&self.reopen).remove(&conn_id);
        for id in gone {
            locked(&self.sessions).remove(&id);
            locked(&self.tunnels).remove(&id);
            locked(&self.db_filters).remove(&id);
        }
    }

    /// The server connections of a profile connection that is going away,
    /// kept for the next connect of the profile (`abandoned`). Best-effort:
    /// at worst they expire on the server.
    fn remember_abandoned(&self, db: &Db, conn_id: u64) {
        let Some(profile_id) = locked(&self.profiles).get(&conn_id).copied() else {
            return;
        };
        let Ok(session) = self.session(conn_id) else {
            return;
        };
        if let Err(e) = abandoned::remember(&locked(&db.0), profile_id, session.server_conn_ids()) {
            eprintln!("abandoned connections (profile {profile_id}): {e}");
        }
    }

    /// The profile's database filter as a predicate on the name. It is
    /// kept on the profile's connection: on PG the other databases' ones
    /// read it from there (§3.3).
    fn db_filter(&self, conn_id: u64) -> impl Fn(&str) -> bool {
        let f = locked(&self.db_filters)
            .get(&self.root(conn_id))
            .cloned()
            .unwrap_or_default();
        move |name| f.shows(name)
    }

    /// The filter on a connection's schemas: on MySQL/MariaDB they are the
    /// databases. On PG it filters the server's databases instead
    /// (`server_databases`) and the schemas are all shown.
    fn schema_filter(&self, conn_id: u64, session: &Session) -> impl Fn(&str) -> bool {
        let on_schemas = session.capabilities().can_change_database_in_session;
        let shows = self.db_filter(conn_id);
        move |name| !on_schemas || shows(name)
    }

    /// M0 bench/harness path: any session (in practice the only one).
    fn any_session(&self) -> Option<(u64, Arc<Session>)> {
        locked(&self.sessions)
            .iter()
            .next()
            .map(|(id, s)| (*id, s.clone()))
    }
}

// ---------------------------------------------------------------- connection

/// Connection from URL for smoke and bench (ALAMO_M1_SMOKE / ALAMO_M0_AUTO):
/// only from ALAMO_BENCH_URL, never a URL from the frontend, and in clear
/// text. Outside those paths the command does nothing: in the normal build
/// only a profile connects, with TLS, keychain and confirmations.
#[tauri::command]
async fn connect(state: State<'_, AppState>, url: Option<String>) -> Result<u64, ErrorOut> {
    let refuse = |m: &str| ErrorOut::from(&alamo_core::error::DbError::internal(m));
    if !(m1_smoke() || auto_mode_flag()) {
        return Err(refuse(
            "connection from URL available only for smoke and bench (ALAMO_M1_SMOKE, ALAMO_M0_AUTO)",
        ));
    }
    if url.is_some() {
        return Err(refuse("the URL comes only from ALAMO_BENCH_URL"));
    }
    let url =
        std::env::var("ALAMO_BENCH_URL").map_err(|_| refuse("no URL: set ALAMO_BENCH_URL"))?;
    let session = Session::connect(&url)
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(state.insert(session))
}

#[derive(Serialize)]
struct ConnectOut {
    conn_id: u64,
    profile: storage::Profile,
    security: Security,
    /// PG (§3.3): one database per connection, the others are opened with
    /// `open_database`.
    one_db_per_conn: bool,
    /// The Execution settings in force on this connection (§16): the global
    /// ones with the profile's overrides. The frontend reads, never merges.
    execution: settings::Execution,
    /// Per-connection overrides (§16) the profile asked for that this build
    /// could not apply (unknown or not overridable): told once at connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<settings::Issue>,
}

/// What really protects the open connection, for the UI (§12.2): `prefer`
/// may have fallen back to clear text, `require` encrypts without knowing
/// who it is talking to. Every field beyond `encrypted` is as measured on
/// the session; `None` = the engine cannot tell.
#[derive(Serialize)]
struct Security {
    encrypted: bool,
    /// Server certificate verified (verify-ca/verify-full).
    verified: bool,
    protocol: Option<String>,
    cipher: Option<String>,
    cert: Option<CertOut>,
}

impl From<Option<alamo_core::tls::TlsInfo>> for Security {
    fn from(info: Option<alamo_core::tls::TlsInfo>) -> Security {
        match info {
            None => Security {
                encrypted: false,
                verified: false,
                protocol: None,
                cipher: None,
                cert: None,
            },
            Some(i) => Security {
                encrypted: true,
                verified: i.verified,
                protocol: i.protocol,
                cipher: i.cipher,
                cert: i.server_cert.map(CertOut::from),
            },
        }
    }
}

/// A certificate as the UI shows it: the server's, or the profile's root
/// CA inspected in the dialog. Dates in unix seconds.
#[derive(Serialize)]
struct CertOut {
    subject: Option<String>,
    issuer: Option<String>,
    not_before: Option<i64>,
    not_after: Option<i64>,
}

impl From<alamo_core::tls::CertSummary> for CertOut {
    fn from(c: alamo_core::tls::CertSummary) -> CertOut {
        CertOut {
            subject: c.subject,
            issuer: c.issuer,
            not_before: c.not_before,
            not_after: c.not_after,
        }
    }
}

/// SSH tunnel of a connection, with what is needed to rebuild it if it drops
/// (network, Mac asleep): the session points to `127.0.0.1:port` and that
/// port stays. `tunnel` None = the last reopen failed, retried at the next
/// `ensure_connected`.
struct TunnelSlot {
    tunnel: Option<alamo_tunnel::Tunnel>,
    cfg: alamo_tunnel::TunnelConfig,
    /// Timeout and keepalive of §16 as they were at connect: a rebuild is
    /// the same connection, not a new one.
    net: alamo_tunnel::TunnelNet,
    port: u16,
}

impl TunnelSlot {
    fn is_closed(&self) -> bool {
        self.tunnel.as_ref().map_or(true, |t| t.is_closed())
    }
}

/// Profile opened but not yet registered: session (and tunnel) up, with the
/// credentials actually used. Consumed by `connect_profile` (which registers
/// it) and `test_profile` (which throws it away).
struct OpenedProfile {
    profile: storage::Profile,
    session: Session,
    /// Execution settings in force (§16) and the override problems found at
    /// connect, shown once.
    execution: settings::Execution,
    warnings: Vec<settings::Issue>,
    tunnel: Option<TunnelSlot>,
    service: String,
    password: String,
    ssh_secret: String,
    /// Credentials typed in the dialog (not read from the keychain): only
    /// these get saved, rewriting an identical entry asks for Keychain
    /// permission for nothing.
    password_typed: bool,
    ssh_typed: bool,
    /// Keychain entries (see `db_account`/`ssh_account`).
    db_account: String,
    ssh_account: Option<String>,
}

/// Keychain account of the database profile's password: profile tag (or id
/// for legacy profiles, `secrets::db_account`).
fn db_account(db: &rusqlite::Connection, id: i64) -> Result<String, String> {
    let tag = storage::profile_secret_tag(db, id).map_err(|e| e.to_string())?;
    Ok(secrets::db_account(id, tag.as_deref()))
}

/// Like `db_account`, for the SSH profile's credential.
fn ssh_account(db: &rusqlite::Connection, sid: i64) -> Result<String, String> {
    let tag = storage::ssh_secret_tag(db, sid).map_err(|e| e.to_string())?;
    Ok(secrets::ssh_account(sid, tag.as_deref()))
}

/// Opens the connection of a saved profile: credentials from the dialog or,
/// with an empty field, from the keychain; SSH tunnel first if the profile
/// uses one.
async fn open_profile(
    app: &AppHandle,
    db: &State<'_, Db>,
    id: i64,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> Result<OpenedProfile, ErrorOut> {
    let (profile, db_acc) = {
        let db = locked(&db.0);
        (
            storage::profile_get(&db, id).map_err(|e| internal(e.to_string()))?,
            db_account(&db, id).map_err(internal)?,
        )
    };
    let profile = profile.ok_or_else(|| internal(format!("profile {id} does not exist")))?;
    // An unknown sslmode (hand-written profile, future version) does not
    // silently become `prefer`: the user may have expected a verification.
    if alamo_core::tls::TlsMode::parse(&profile.sslmode).is_none() {
        return Err(internal(format!(
            "unknown SSL \"{}\": choose a mode in the profile",
            profile.sslmode
        )));
    }
    // §13.1: service name derived from the identifier, never typed by hand.
    let service = keychain_service(app.config());
    let typed = password.unwrap_or_default();
    let password_typed = !typed.is_empty();
    let password = if typed.is_empty() {
        secrets::get(&service, &db_acc)
            .map_err(internal)?
            .unwrap_or_default()
    } else {
        typed
    };
    // The SSH profile is a shared entity (migration 5): the credential in
    // the keychain is tied to ITS id, so all the database profiles that go
    // through the same bastion reuse it.
    let (ssh_profile, ssh_acc) = match profile.ssh_profile_id {
        Some(sid) => {
            let db = locked(&db.0);
            let sp = storage::ssh_profile_get(&db, sid)
                .map_err(|e| internal(e.to_string()))?
                .ok_or_else(|| internal(format!("SSH profile {sid} does not exist")))?;
            (Some(sp), Some(ssh_account(&db, sid).map_err(internal)?))
        }
        None => (None, None),
    };
    let typed_ssh = ssh_secret.unwrap_or_default();
    let ssh_typed = !typed_ssh.is_empty();
    let ssh_secret = match (&ssh_acc, typed_ssh.is_empty()) {
        (Some(acc), true) => secrets::get(&service, acc)
            .map_err(internal)?
            .unwrap_or_default(),
        _ => typed_ssh,
    };
    // The tunnel first (§12): the connection string points to its listener.
    // Tunnel failed, no session.
    let global = app.state::<settings::SettingsStore>().snapshot();
    let mut tunnel = None;
    let tunnel_port = if let Some(sp) = &ssh_profile {
        let sid = sp.id.unwrap_or(0);
        let cfg = alamo_tunnel::TunnelConfig {
            ssh: ssh_endpoint(&locked(&db.0), sp, &ssh_secret)?,
            target_host: profile.host.clone(),
            target_port: profile.port,
        };
        let net = global.connections.tunnel_net();
        let t = alamo_tunnel::Tunnel::open_at(cfg.clone(), None, net)
            .await
            .map_err(|e| tunnel_error(app, sid, sp, e))?;
        let port = t.local_port;
        tunnel = Some(TunnelSlot {
            tunnel: Some(t),
            cfg,
            net,
            port,
        });
        Some(port)
    } else {
        None
    };
    let (session, execution, warnings) = app
        .state::<AppState>()
        .open_session(&profile, &password, tunnel_port, &global)
        .await?;
    Ok(OpenedProfile {
        profile,
        execution,
        warnings,
        session,
        tunnel,
        service,
        password,
        ssh_secret,
        password_typed,
        ssh_typed,
        db_account: db_acc,
        ssh_account: ssh_acc,
    })
}

/// The bastion of an SSH profile as `alamo-tunnel` wants it: credential
/// resolved by the caller (dialog or keychain), trusted key from the profile.
fn ssh_endpoint(
    db: &rusqlite::Connection,
    sp: &storage::SshProfile,
    secret: &str,
) -> Result<alamo_tunnel::SshEndpoint, ErrorOut> {
    let auth = match sp.auth.as_str() {
        "password" => alamo_tunnel::TunnelAuth::Password(secret.to_owned()),
        "key" => alamo_tunnel::TunnelAuth::Key {
            path: expand_home(
                &sp.key_path
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| internal("SSH tunnel: private key path missing"))?,
            ),
            passphrase: (!secret.is_empty()).then(|| secret.to_owned()),
        },
        _ => alamo_tunnel::TunnelAuth::Agent,
    };
    let trusted_key =
        storage::ssh_host_key_get(db, sp.id.unwrap_or(0)).map_err(|e| internal(e.to_string()))?;
    Ok(alamo_tunnel::SshEndpoint {
        host: sp.host.clone(),
        port: sp.port,
        user: sp.username.clone(),
        auth,
        known_hosts: None,
        trusted_key,
    })
}

/// Tunnel error towards the frontend. The never-seen bastion has its own
/// code (`ssh_host_key_unknown`): the dialog shows fingerprint (`detail`)
/// and server (`context`) and asks for confirmation; the key stays here,
/// waiting.
fn tunnel_error(
    app: &AppHandle,
    ssh_id: i64,
    sp: &storage::SshProfile,
    e: alamo_tunnel::TunnelError,
) -> ErrorOut {
    let mut out = internal(e.to_string());
    if let alamo_tunnel::TunnelError::HostKeyUnknown {
        algorithm,
        fingerprint,
        key,
    } = e
    {
        out.code = Some("ssh_host_key_unknown".into());
        out.detail = Some(fingerprint.clone());
        out.context = Some(format!("{}:{} · {algorithm}", sp.host, sp.port));
        locked(&app.state::<AppState>().pending_host_keys)
            .insert(ssh_id, PendingHostKey { fingerprint, key });
    }
    out
}

/// The user trusts the bastion of profile `id` (§12): saves the pending key
/// with the SSH profile, if the fingerprint is the one they saw. Afterwards
/// the frontend retries the connection.
#[tauri::command]
fn ssh_host_key_trust(
    state: State<'_, AppState>,
    db: State<'_, Db>,
    id: i64,
    fingerprint: String,
) -> Result<(), String> {
    let db = locked(&db.0);
    let sid = storage::profile_get(&db, id)
        .map_err(|e| e.to_string())?
        .and_then(|p| p.ssh_profile_id)
        .ok_or("the profile does not use an SSH tunnel")?;
    trust_pending(&state, &db, sid, &fingerprint)
}

/// Same, from the SSH profiles manager ("Test tunnel"): `id` is the SSH
/// profile itself.
#[tauri::command]
fn ssh_profile_host_key_trust(
    state: State<'_, AppState>,
    db: State<'_, Db>,
    id: i64,
    fingerprint: String,
) -> Result<(), String> {
    trust_pending(&state, &locked(&db.0), id, &fingerprint)
}

fn trust_pending(
    state: &AppState,
    db: &rusqlite::Connection,
    sid: i64,
    fingerprint: &str,
) -> Result<(), String> {
    let pending = locked(&state.pending_host_keys).remove(&sid);
    match pending {
        Some(p) if p.fingerprint == fingerprint => {
            storage::ssh_host_key_set(db, sid, &p.key).map_err(|e| e.to_string())
        }
        _ => Err("fingerprint no longer valid: retry the connection".into()),
    }
}

#[derive(Serialize)]
struct TestOut {
    /// Current database as seen by the server once connected.
    database: Option<String>,
    elapsed_ms: u64,
    /// What the channel negotiated (§12.2): the dialog shows it next to
    /// the outcome, so a profile is judged before it is used.
    security: Security,
}

/// "Test connection": opens (tunnel, TLS, authentication), runs a real
/// query, closes. Does not register the session and does not touch the
/// keychain: a test must neither save nor forget credentials.
#[tauri::command]
async fn test_profile(
    app: AppHandle,
    db: State<'_, Db>,
    id: i64,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> Result<TestOut, ErrorOut> {
    let started = std::time::Instant::now();
    let opened = open_profile(&app, &db, id, password, ssh_secret).await?;
    let database = opened
        .session
        .current_database()
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    let security = security(&opened.session).await;
    // Explicit drop: session first, then the tunnel that carries it.
    drop(opened.session);
    drop(opened.tunnel);
    Ok(TestOut {
        database,
        elapsed_ms: started.elapsed().as_millis() as u64,
        security,
    })
}

/// Connection from a saved profile (§9.1). Database password and SSH
/// credential come from the dialog or, with an empty field, from the system
/// keychain (§12) — never from SQLite. `save_password` is the desired state
/// of the keychain entries: true = save on successful connection, false =
/// remove. With an SSH profile attached the TCP goes through a local
/// port-forward (alamo-tunnel): ALL the session's connections — lanes,
/// cancel/KILL, escalation — share the rewritten string and therefore the
/// tunnel. Does not touch the connections already open: they coexist (M3.5).
#[tauri::command]
async fn connect_profile(
    app: AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
    id: i64,
    password: Option<String>,
    save_password: bool,
    ssh_secret: Option<String>,
) -> Result<ConnectOut, ErrorOut> {
    let OpenedProfile {
        profile,
        session,
        tunnel,
        service,
        password,
        ssh_secret,
        password_typed,
        ssh_typed,
        db_account,
        ssh_account,
        execution,
        warnings,
    } = open_profile(&app, &db, id, password, ssh_secret).await?;
    let one_db_per_conn = !session.capabilities().can_change_database_in_session;
    // Connections of the profile's previous session that the server may
    // still hold (closed after a sleep): closed now, before the tree and the
    // table tabs ask for theirs.
    let left = abandoned::take(&locked(&db.0), id).map_err(|e| internal(e.to_string()))?;
    session.kill_abandoned(&left).await;
    let conn_id = state.insert(session);
    locked(&state.profiles).insert(conn_id, id);
    if let Some(t) = tunnel {
        locked(&state.tunnels).insert(conn_id, t);
    }
    if one_db_per_conn {
        let mut profile = profile.clone();
        // No database in the profile: PG connects to the user's namesake.
        if profile.database.trim().is_empty() {
            profile.database = profile.username.clone();
        }
        locked(&state.reopen).insert(
            conn_id,
            Reopen {
                profile,
                password: password.clone(),
            },
        );
    }
    locked(&state.db_filters).insert(
        conn_id,
        dbfilter::DbFilter::new(&profile.db_include, &profile.db_exclude),
    );
    // Only on successful connection: a wrong credential in the keychain
    // would come back on every connect. Best-effort: a keychain that
    // refuses must not throw away a connection already up.
    let outcome = if save_password {
        let mut r = Ok(());
        if password_typed {
            r = r.and(secrets::save(&service, &db_account, &password));
        }
        if let Some(acc) = &ssh_account {
            if ssh_typed {
                r = r.and(secrets::save(&service, acc, &ssh_secret));
            }
        }
        r
    } else {
        // Checkbox cleared: the db password drops and so does the credential
        // of the bastion used HERE — shared, so it disappears for the other
        // profiles that go through it too: that is the semantics of "do not
        // save it". Only if it is there: deletion too reads the entry and
        // asks for Keychain permission.
        let forget_present = |acc: &str| -> Result<(), String> {
            if secrets::exists(&service, acc)? {
                secrets::forget(&service, acc)?;
            }
            Ok(())
        };
        let mut r = forget_present(&db_account);
        if let Some(acc) = &ssh_account {
            r = r.and(forget_present(acc));
        }
        r
    };
    if let Err(e) = outcome {
        eprintln!("keychain (profile {id}): {e}");
    }
    let session = state.session(conn_id).map_err(internal)?;
    let security = security(&session).await;
    let db = locked(&db.0);
    let _ = storage::profile_touch(&db, id);
    Ok(ConnectOut {
        conn_id,
        profile,
        security,
        one_db_per_conn,
        execution,
        warnings,
    })
}

/// What protects the channel, measured on the open session. If the server
/// does not answer the question, better "not encrypted" than a false padlock.
async fn security(session: &Session) -> Security {
    Security::from(session.tls_info().await.unwrap_or(None))
}

/// Databases of the server (connection page, tree), through the profile's
/// filter. On PG each one is opened with `open_database`; on MySQL/MariaDB
/// they are the schemas.
#[tauri::command]
async fn server_databases(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<Vec<alamo_session::introspect::SchemaOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session.databases().await.map_err(|e| ErrorOut::from(&*e))?;
    let shows = state.db_filter(conn_id);
    Ok(v.into_iter()
        .filter(|d| d.is_system || shows(&d.name))
        .map(Into::into)
        .collect())
}

/// Another database of the same PG server (§3.3): a connection of its own,
/// with the profile's credentials, TLS regime, prod/read-only flags and SSH
/// tunnel. `conn_id` can be the profile's connection or one of its
/// siblings; the profile's database gives back the profile's connection,
/// an already open one gives back its connection.
#[tauri::command]
async fn open_database(
    state: State<'_, AppState>,
    settings: State<'_, settings::SettingsStore>,
    conn_id: u64,
    db: String,
) -> Result<ConnectOut, ErrorOut> {
    let root = state.root(conn_id);
    let existing = |state: &AppState| {
        locked(&state.siblings)
            .iter()
            .find(|(_, (r, d))| *r == root && *d == db)
            .map(|(id, _)| *id)
    };
    let (mut profile, password) = {
        let reopen = locked(&state.reopen);
        let r = reopen.get(&root).ok_or_else(|| {
            internal(format!(
                "connection {root} cannot open other databases: closed, or not PostgreSQL"
            ))
        })?;
        (r.profile.clone(), r.password.clone())
    };
    let target = if profile.database == db {
        Some(root)
    } else {
        existing(&state)
    };
    // The same overrides as the profile's connection (§16): the problems
    // were already told at its connect.
    let global = settings.snapshot();
    let execution = settings::effective(&global, &profile.settings).0.execution;
    let id = match target {
        Some(id) => id,
        None => {
            let port = locked(&state.tunnels).get(&root).map(|t| t.port);
            profile.database = db.clone();
            let (session, _, _) = state
                .open_session(&profile, &password, port, &global)
                .await?;
            // Two opens of the same database at once (tree and page): the
            // second one gives up its session for the first one's.
            match existing(&state) {
                Some(id) => id,
                None if !locked(&state.sessions).contains_key(&root) => {
                    return Err(internal(format!("connection {root} closed")));
                }
                None => {
                    let id = state.insert(session);
                    locked(&state.siblings).insert(id, (root, db.clone()));
                    id
                }
            }
        }
    };
    profile.database = db;
    let session = state.session(id).map_err(internal)?;
    let security = security(&session).await;
    Ok(ConnectOut {
        conn_id: id,
        profile,
        security,
        one_db_per_conn: true,
        execution,
        warnings: Vec::new(),
    })
}

#[derive(Serialize)]
struct SavedSecrets {
    db: bool,
    ssh: bool,
}

/// Are there credentials saved in the keychain for this profile? The dialog
/// derives placeholder and checkbox state from it — the credentials
/// themselves never cross the IPC towards the frontend. The ssh flag looks
/// at the referenced SSH profile (credential shared among database
/// profiles).
#[tauri::command]
fn profile_password_present(
    app: AppHandle,
    db: State<'_, Db>,
    id: i64,
) -> Result<SavedSecrets, String> {
    let service = keychain_service(app.config());
    let (db_acc, ssh_acc) = {
        let db = locked(&db.0);
        let sid = storage::profile_get(&db, id)
            .map_err(|e| e.to_string())?
            .and_then(|p| p.ssh_profile_id);
        (
            db_account(&db, id)?,
            sid.map(|sid| ssh_account(&db, sid)).transpose()?,
        )
    };
    Ok(SavedSecrets {
        db: secrets::exists(&service, &db_acc)?,
        ssh: match ssh_acc {
            Some(acc) => secrets::exists(&service, &acc)?,
            None => false,
        },
    })
}

/// §7.2 escalation: cancel had no effect within ~10s, the user chose
/// termination. Destructive: afterwards the session is thrown away and must
/// be reopened from the dialog.
#[tauri::command]
async fn terminate_connection(
    state: State<'_, AppState>,
    db: State<'_, Db>,
    conn_id: u64,
) -> Result<bool, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let terminated = session.terminate().await.map_err(|e| ErrorOut::from(&*e))?;
    state.forget(&db, conn_id);
    Ok(terminated)
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>, db: State<'_, Db>, conn_id: u64) {
    // Dropping the session closes the connection; the indexed result sets
    // die with the registry. Any SSH tunnel dies with it, and so do the
    // connections to the profile's other databases (§3.3).
    state.forget(&db, conn_id);
}

#[tauri::command]
fn connection_status(state: State<'_, AppState>) -> bool {
    !locked(&state.sessions).is_empty()
}

// ---------------------------------------------------------------- profiles and autosave

#[tauri::command]
fn profiles_list(db: State<'_, Db>) -> Result<Vec<storage::Profile>, String> {
    storage::profiles_list(&locked(&db.0)).map_err(|e| e.to_string())
}

/// Server changed = new keychain entry (`storage::profile_save`): the old
/// one is deleted here, it was the password of another server.
#[derive(Serialize)]
struct ProfileSaveOut {
    id: i64,
    /// The profile's open connections with their Execution settings after
    /// the save (§16): its overrides may have changed.
    effective: Vec<EffectiveOut>,
}

#[tauri::command]
async fn profile_save(
    app: AppHandle,
    db: State<'_, Db>,
    settings: State<'_, settings::SettingsStore>,
    profile: storage::Profile,
) -> Result<ProfileSaveOut, String> {
    let id = {
        let db = locked(&db.0);
        let before = profile.id.map(|id| db_account(&db, id)).transpose()?;
        let id = storage::profile_save(&db, &profile).map_err(|e| e.to_string())?;
        if let Some(old) = before.filter(|old| db_account(&db, id).ok().as_ref() != Some(old)) {
            if let Err(e) = secrets::forget(&keychain_service(app.config()), &old) {
                eprintln!("keychain (profile {id}): {e}");
            }
        }
        id
    };
    // Its overrides may have changed (§16): reapply to this profile's open
    // connections so the limits take effect without a reconnect.
    let mut effective = Vec::new();
    for (conn_id, session, execution) in effective_executions(&app, &settings.snapshot(), Some(id))
    {
        push_execution(&session, &execution).await;
        effective.push(EffectiveOut { conn_id, execution });
    }
    Ok(ProfileSaveOut { id, effective })
}

/// The Execution settings in force (§16) on every open connection: the
/// global ones with the overrides of its profile, read from the profile
/// store (the connections to the other databases of a PG server take their
/// profile's). `only`: the connections of that profile alone. A connection
/// without a profile (bench) takes the global ones.
fn effective_executions(
    app: &AppHandle,
    global: &settings::Settings,
    only: Option<i64>,
) -> Vec<(u64, Arc<Session>, settings::Execution)> {
    let state = app.state::<AppState>();
    let conns: Vec<(u64, Arc<Session>, Option<i64>)> = {
        let sessions = locked(&state.sessions);
        let profiles = locked(&state.profiles);
        let siblings = locked(&state.siblings);
        sessions
            .iter()
            .map(|(id, s)| {
                let root = siblings.get(id).map(|(root, _)| *root).unwrap_or(*id);
                (*id, s.clone(), profiles.get(&root).copied())
            })
            .filter(|(_, _, pid)| only.map_or(true, |only| *pid == Some(only)))
            .collect()
    };
    let db = app.state::<Db>();
    let db = locked(&db.0);
    let mut by_profile: HashMap<i64, settings::Execution> = HashMap::new();
    conns
        .into_iter()
        .map(|(id, session, pid)| {
            let execution = match pid {
                None => global.execution.clone(),
                Some(pid) => by_profile
                    .entry(pid)
                    .or_insert_with(|| {
                        let overrides = storage::profile_get(&db, pid)
                            .ok()
                            .flatten()
                            .map(|p| p.settings)
                            .unwrap_or_default();
                        settings::effective(global, &overrides).0.execution
                    })
                    .clone(),
            };
            (id, session, execution)
        })
        .collect()
}

/// Pushes Execution settings (§16) into an open session: the limits are
/// atomics, so a fetch already running sees them; the statement timeout is
/// a server SET (best-effort: a dead connection gets it at reconnect).
async fn push_execution(session: &Session, execution: &settings::Execution) {
    session.set_limits(execution.limits());
    let _ = session.apply_query_timeout().await;
}

#[tauri::command]
fn profile_delete(app: AppHandle, db: State<'_, Db>, id: i64) -> Result<(), String> {
    let db = locked(&db.0);
    let account = db_account(&db, id)?;
    storage::profile_delete(&db, id).map_err(|e| e.to_string())?;
    abandoned::forget(&db, id).map_err(|e| e.to_string())?;
    // The db password dies with the profile. The SSH credential instead
    // belongs to the (shared) SSH profile and dies with IT, in
    // `ssh_profile_delete`.
    secrets::forget(&keychain_service(app.config()), &account)
}

// ------------------------------------------------------------- SSH profiles

/// SSH profile as the manager shows it: who uses it, what is saved for it.
#[derive(Serialize)]
struct SshProfileOut {
    #[serde(flatten)]
    profile: storage::SshProfile,
    /// Database profiles that go through this bastion, by name.
    used_by: Vec<String>,
    /// Bastion key confirmed by the user, as a fingerprint (`SHA256:…`).
    host_fingerprint: Option<String>,
    /// Its credential is in the keychain (the credential itself never
    /// crosses the IPC).
    secret_saved: bool,
}

#[tauri::command]
fn ssh_profiles_list(app: AppHandle, db: State<'_, Db>) -> Result<Vec<SshProfileOut>, String> {
    let service = keychain_service(app.config());
    let db = locked(&db.0);
    let mut usage = storage::ssh_profile_usage(&db).map_err(|e| e.to_string())?;
    storage::ssh_profiles_list(&db)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|profile| {
            let id = profile.id.unwrap_or(0);
            let host_fingerprint = storage::ssh_host_key_get(&db, id)
                .map_err(|e| e.to_string())?
                .as_deref()
                .and_then(alamo_tunnel::fingerprint_of);
            Ok(SshProfileOut {
                used_by: usage.remove(&id).unwrap_or_default(),
                host_fingerprint,
                secret_saved: secrets::exists(&service, &ssh_account(&db, id)?)?,
                profile,
            })
        })
        .collect()
}

/// Like `profile_save`: bastion or user changed = old credential gone.
#[tauri::command]
fn ssh_profile_save(
    app: AppHandle,
    db: State<'_, Db>,
    profile: storage::SshProfile,
) -> Result<i64, String> {
    let db = locked(&db.0);
    let before = profile.id.map(|id| ssh_account(&db, id)).transpose()?;
    let id = storage::ssh_profile_save(&db, &profile).map_err(|e| e.to_string())?;
    if let Some(old) = before.filter(|old| ssh_account(&db, id).ok().as_ref() != Some(old)) {
        if let Err(e) = secrets::forget(&keychain_service(app.config()), &old) {
            eprintln!("keychain (SSH profile {id}): {e}");
        }
    }
    Ok(id)
}

#[tauri::command]
fn ssh_profile_delete(app: AppHandle, db: State<'_, Db>, id: i64) -> Result<(), String> {
    let db = locked(&db.0);
    let account = ssh_account(&db, id)?;
    storage::ssh_profile_delete(&db, id).map_err(|e| e.to_string())?;
    // The bastion's credential dies with its profile.
    secrets::forget(&keychain_service(app.config()), &account)
}

/// "Do not save it" in the manager: the bastion's credential leaves the
/// keychain for every connection that goes through it.
#[tauri::command]
fn ssh_secret_forget(app: AppHandle, db: State<'_, Db>, id: i64) -> Result<(), String> {
    let account = ssh_account(&locked(&db.0), id)?;
    secrets::forget(&keychain_service(app.config()), &account)
}

#[tauri::command]
fn ssh_host_key_forget(db: State<'_, Db>, id: i64) -> Result<(), String> {
    storage::ssh_host_key_forget(&locked(&db.0), id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct ProbeOut {
    elapsed_ms: u64,
}

/// "Test tunnel" of the SSH profiles manager: bastion reachable, key
/// trusted, credential accepted — nothing forwarded. The credential comes
/// from the dialog or, empty, from the keychain; with `save_secret` the
/// typed one is saved once the bastion has accepted it (as on connect:
/// never a wrong credential in the keychain).
#[tauri::command]
async fn ssh_profile_test(
    app: AppHandle,
    db: State<'_, Db>,
    settings: State<'_, settings::SettingsStore>,
    id: i64,
    secret: Option<String>,
    save_secret: bool,
) -> Result<ProbeOut, ErrorOut> {
    let started = Instant::now();
    let service = keychain_service(app.config());
    let (sp, account) = {
        let db = locked(&db.0);
        let sp = storage::ssh_profile_get(&db, id)
            .map_err(|e| internal(e.to_string()))?
            .ok_or_else(|| internal(format!("SSH profile {id} does not exist")))?;
        (sp, ssh_account(&db, id).map_err(internal)?)
    };
    let typed = Zeroizing::new(secret.unwrap_or_default());
    let secret = if typed.is_empty() {
        Zeroizing::new(
            secrets::get(&service, &account)
                .map_err(internal)?
                .unwrap_or_default(),
        )
    } else {
        typed.clone()
    };
    // The lock must not live across the await: built first.
    let endpoint = ssh_endpoint(&locked(&db.0), &sp, &secret)?;
    alamo_tunnel::probe_with(&endpoint, settings.snapshot().connections.tunnel_net())
        .await
        .map_err(|e| tunnel_error(&app, id, &sp, e))?;
    if save_secret && !typed.is_empty() {
        if let Err(e) = secrets::save(&service, &account, &typed) {
            eprintln!("keychain (SSH profile {id}): {e}");
        }
    }
    Ok(ProbeOut {
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

// ------------------------------------------------- connections export/import

#[derive(Serialize)]
struct ConnectionsExportOut {
    path: String,
    count: usize,
    /// With "include passwords": connections with the database password in
    /// the file (the others had none in the keychain).
    passwords: usize,
}

/// Export of the chosen connections (`transfer` module): native save dialog
/// like `export_result`. With `passphrase` the keychain passwords go into
/// the file, encrypted. `None` = cancelled by the user.
#[tauri::command]
async fn connections_export(
    app: AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
    profile_ids: Vec<i64>,
    passphrase: Option<String>,
    suggested_name: String,
) -> Result<Option<ConnectionsExportOut>, String> {
    use tauri_plugin_dialog::DialogExt;
    let passphrase = passphrase.filter(|p| !p.is_empty()).map(Zeroizing::new);
    if passphrase
        .as_ref()
        .is_some_and(|p| p.chars().count() < transfer::MIN_PASSPHRASE)
    {
        return Err(format!(
            "the file password must be at least {} characters long",
            transfer::MIN_PASSPHRASE
        ));
    }
    let (built, db_accounts, ssh_accounts) = {
        let db = locked(&db.0);
        let built = transfer::build_export(&db, &profile_ids).map_err(|e| e.to_string())?;
        let db_accounts = built
            .profile_ids
            .iter()
            .map(|&id| db_account(&db, id))
            .collect::<Result<Vec<_>, _>>()?;
        let ssh_accounts = built
            .ssh_ids
            .iter()
            .map(|&sid| ssh_account(&db, sid))
            .collect::<Result<Vec<_>, _>>()?;
        (built, db_accounts, ssh_accounts)
    };
    let mut doc = built.doc;
    if doc.profiles.is_empty() {
        return Err("no connections to export".into());
    }
    // Canonical form: it is the passwords' associated data, and the file
    // must be able to come back through the import as is.
    transfer::normalize(&mut doc, &state.drivers)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("JSON", &["json"])
        .set_file_name(&suggested_name)
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Ok(Some(picked)) = rx.await else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    // Keychain and Argon2 after the file choice (cancelling must not bring
    // up Keychain prompts) and outside the command runtime.
    let service = keychain_service(app.config());
    let (doc, passwords) = tokio::task::spawn_blocking(move || {
        let Some(passphrase) = passphrase else {
            return Ok((doc, 0));
        };
        let mut s = transfer::Secrets::default();
        for (i, acc) in db_accounts.iter().enumerate() {
            if let Some(p) = secrets::get(&service, acc)? {
                s.profiles.insert(i, p);
            }
        }
        for (k, acc) in ssh_accounts.iter().enumerate() {
            if let Some(p) = secrets::get(&service, acc)? {
                s.ssh.insert(doc.ssh_profiles[k].ref_id.clone(), p);
            }
        }
        let n = s.profiles.len();
        // Nothing to encrypt: file without blob, the import will ask for no
        // password.
        if !s.is_empty() {
            transfer::seal(&mut doc, &s, &passphrase, transfer::KDF_DEFAULT)?;
        }
        Ok::<_, String>((doc, n))
    })
    .await
    .map_err(|e| e.to_string())??;
    let json = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(Some(ConnectionsExportOut {
        path: path.display().to_string(),
        count: doc.profiles.len(),
        passwords,
    }))
}

#[derive(Serialize)]
struct ImportPreviewOut {
    file_name: String,
    /// Comes back with `connections_import_apply`: no state in the backend
    /// between preview and import (the apply re-checks it from scratch).
    doc: transfer::ExportDoc,
    rows: Vec<transfer::ImportRow>,
}

/// Native "open file" dialog, Rust side only (no JS binding, see the plugin
/// setup). `None` = cancelled. The e2e build takes the path queued by
/// `e2e_next_pick` instead of opening anything.
async fn pick_file(
    app: &AppHandle,
    filter_name: &str,
    extensions: &[&str],
) -> Result<Option<std::path::PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt;
    #[cfg(feature = "e2e")]
    if let Some(p) = e2e::take_pick() {
        return Ok(Some(p));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter(filter_name, extensions)
        .pick_file(move |p| {
            let _ = tx.send(p);
        });
    let Ok(Some(picked)) = rx.await else {
        return Ok(None);
    };
    picked.into_path().map(Some).map_err(|e| e.to_string())
}

/// "Browse…" of the profile's root CA (§12.2). `None` = cancelled.
#[tauri::command]
async fn pick_root_cert(app: AppHandle) -> Result<Option<String>, String> {
    Ok(pick_file(&app, "PEM certificate", &["pem", "crt", "cer"])
        .await?
        .map(|p| p.to_string_lossy().into_owned()))
}

/// Subject, issuer and validity of the profile's root CA, shown under the
/// field as soon as it is filled: a wrong file is found before connecting.
#[tauri::command]
fn inspect_root_cert(path: String) -> Result<CertOut, String> {
    alamo_session::x509::inspect_pem_file(&expand_home(&path)).map(CertOut::from)
}

/// First half of the import: native open dialog, read, checks and preview.
/// Writes nothing. `None` = cancelled by the user.
#[tauri::command]
async fn connections_import_open(
    app: AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<Option<ImportPreviewOut>, String> {
    let Some(path) = pick_file(&app, "JSON", &["json"]).await? else {
        return Ok(None);
    };
    let len = std::fs::metadata(&path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?
        .len();
    if len > transfer::MAX_FILE_BYTES {
        return Err("file too large for a connections export".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let doc = transfer::parse(&bytes, &state.drivers)?;
    let rows = transfer::preview(&locked(&db.0), &doc, &|p| expand_home(p).exists())
        .map_err(|e| e.to_string())?;
    Ok(Some(ImportPreviewOut {
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        doc,
        rows,
    }))
}

/// Second half: imports the chosen profiles (indexes in `doc.profiles`), in
/// a single transaction. With `passphrase` the file's passwords go into the
/// keychain: decrypted BEFORE writing (wrong password = nothing imported),
/// saved AFTER the commit under the tags of the new profiles. Empty = import
/// without passwords.
#[tauri::command]
async fn connections_import_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    db: State<'_, Db>,
    mut doc: transfer::ExportDoc,
    selected: Vec<usize>,
    passphrase: Option<String>,
) -> Result<transfer::ImportOutcome, String> {
    transfer::normalize(&mut doc, &state.drivers)?;
    let passphrase = passphrase.filter(|p| !p.is_empty()).map(Zeroizing::new);
    // Argon2 (~tenths of a second) outside the command runtime.
    let (doc, secrets) = tokio::task::spawn_blocking(move || {
        let s = match &passphrase {
            Some(p) => transfer::open(&doc, p)?,
            None => transfer::Secrets::default(),
        };
        Ok::<_, String>((doc, s))
    })
    .await
    .map_err(|e| e.to_string())??;
    let (mut out, to_save) = {
        let db = locked(&db.0);
        let out = transfer::apply(&db, &doc, &selected)?;
        let mut to_save = Vec::new();
        for &(i, id) in &out.new_profiles {
            if let Some(p) = secrets.profiles.get(&i) {
                to_save.push((db_account(&db, id)?, Zeroizing::new(p.clone())));
            }
        }
        for (r, sid) in &out.new_ssh {
            if let Some(p) = secrets.ssh.get(r) {
                to_save.push((ssh_account(&db, *sid)?, Zeroizing::new(p.clone())));
            }
        }
        (out, to_save)
    };
    let service = keychain_service(app.config());
    for (account, password) in to_save {
        match secrets::save(&service, &account, &password) {
            Ok(()) => out.passwords_saved += 1,
            Err(e) => out.warnings.push(e),
        }
    }
    Ok(out)
}

// M2: one autosave entry per tab, key `editor:<tab_id>` (§9.4). Tab 0 is
// also the implicit migration of the single M1 buffer.
fn buffer_key(tab_id: &str) -> String {
    format!("editor:{tab_id}")
}

#[tauri::command]
fn autosave_get(db: State<'_, Db>, tab_id: String) -> Result<Option<String>, String> {
    storage::autosave_get(&locked(&db.0), &buffer_key(&tab_id)).map_err(|e| e.to_string())
}

#[tauri::command]
fn autosave_set(db: State<'_, Db>, tab_id: String, sql: String) -> Result<(), String> {
    storage::autosave_set(&locked(&db.0), &buffer_key(&tab_id), &sql).map_err(|e| e.to_string())
}

/// Tab closed: the buffer dies with it, or the autosave accumulates orphans.
#[tauri::command]
fn autosave_delete(db: State<'_, Db>, tab_id: String) -> Result<(), String> {
    storage::autosave_delete(&locked(&db.0), &buffer_key(&tab_id)).map_err(|e| e.to_string())
}

// Workspace (§9.1): set of tabs + layout, opaque JSON for the backend.
// Decision 4 (global or per profile) resolved minimally: global — a single
// workspace, like the single M1 buffer it replaces.

#[tauri::command]
fn workspace_get(db: State<'_, Db>) -> Result<Option<String>, String> {
    storage::kv_get(&locked(&db.0), "workspace").map_err(|e| e.to_string())
}

#[tauri::command]
fn workspace_set(db: State<'_, Db>, json: String) -> Result<(), String> {
    storage::kv_set(&locked(&db.0), "workspace", &json).map_err(|e| e.to_string())
}

// Default sort of the table tab: per profile (not per session: the connId
// changes at every connection), schema and table.

#[tauri::command]
fn table_default_sort_get(
    db: State<'_, Db>,
    profile_id: i64,
    schema: String,
    table: String,
) -> Result<Option<Vec<storage::SortColumn>>, String> {
    let k = storage::TableKey {
        profile_id,
        schema: &schema,
        table: &table,
    };
    storage::table_default_sort_get(&locked(&db.0), &k).map_err(|e| e.to_string())
}

/// Empty `order` = removes the default.
#[tauri::command]
fn table_default_sort_set(
    db: State<'_, Db>,
    profile_id: i64,
    schema: String,
    table: String,
    order: Vec<storage::SortColumn>,
) -> Result<(), String> {
    let k = storage::TableKey {
        profile_id,
        schema: &schema,
        table: &table,
    };
    storage::table_default_sort_set(&locked(&db.0), &k, &order).map_err(|e| e.to_string())
}

// Tree favorites: per profile, like the default sort.

#[tauri::command]
fn favorites_list(db: State<'_, Db>, profile_id: i64) -> Result<Vec<storage::Favorite>, String> {
    storage::favorites_list(&locked(&db.0), profile_id).map_err(|e| e.to_string())
}

/// `table` absent = the whole database/schema.
#[tauri::command]
fn favorite_set(
    db: State<'_, Db>,
    profile_id: i64,
    schema: String,
    table: Option<String>,
    on: bool,
) -> Result<(), String> {
    let fav = storage::Favorite { schema, table };
    storage::favorite_set(&locked(&db.0), profile_id, &fav, on).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- introspection

// §3: they run on the session's dedicated connection, never on the sticky —
// the tree stays explorable even during an execution.

#[tauri::command]
async fn schema_list(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<Vec<alamo_session::introspect::SchemaOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session.schemas().await.map_err(|e| ErrorOut::from(&*e))?;
    let shows = state.schema_filter(conn_id, &session);
    Ok(v.into_iter()
        .filter(|s| s.is_system || shows(&s.name))
        .map(Into::into)
        .collect())
}

/// Numbers per schema/database (connection page): separate call from the
/// list, it can be costly on servers with many databases.
#[tauri::command]
async fn schema_stats(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<Vec<alamo_session::introspect::SchemaStatsOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .schema_stats()
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    let shows = state.schema_filter(conn_id, &session);
    Ok(v.into_iter()
        .filter(|s| s.is_system || shows(&s.name))
        .map(Into::into)
        .collect())
}

#[tauri::command]
async fn schema_relations(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
) -> Result<Vec<alamo_session::introspect::RelationOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .relations(&schema)
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(v.into_iter().map(Into::into).collect())
}

/// Names of every relation of the server in one call (no estimates, no
/// comments): the tree filter searches the table names of all schemas, and
/// one query per schema was too slow over a slow link with many schemas.
#[tauri::command]
async fn schema_relations_all(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<Vec<alamo_session::introspect::RelationOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .relations_all()
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(v.into_iter().map(Into::into).collect())
}

#[tauri::command]
async fn schema_columns(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
) -> Result<Vec<alamo_session::introspect::ColumnInfoOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .columns(&schema, &table)
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(v.into_iter().map(Into::into).collect())
}

/// Foreign keys that touch the table (outgoing and incoming): navigation
/// between linked records starts from here.
#[tauri::command]
async fn schema_foreign_keys(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
) -> Result<Vec<alamo_session::introspect::ForeignKeyOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .foreign_keys(&schema, &table)
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(v.into_iter().map(Into::into).collect())
}

#[tauri::command]
async fn autocomplete_catalog(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: Option<String>,
) -> Result<Vec<alamo_session::introspect::AutocompleteOut>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    let v = session
        .autocomplete_catalog(schema.as_deref())
        .await
        .map_err(|e| ErrorOut::from(&*e))?;
    Ok(v.into_iter()
        .map(
            |(schema, table, column)| alamo_session::introspect::AutocompleteOut {
                schema,
                table,
                column,
            },
        )
        .collect())
}

// ---------------------------------------------------------------- execution

// Note: these commands are `async` even when they await nothing — `Session`
// spawns tokio tasks and needs the Tauri runtime context.

#[tauri::command]
async fn execute(
    state: State<'_, AppState>,
    conn_id: u64,
    sql: String,
    on_event: Channel<ExecEvent>,
) -> Result<u64, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .execute(sql, session.editor_fetch_options(), move |ev| {
            // The channel may already be closed (tab closed, §9.3): ignored.
            let _ = on_event.send(ev);
        })
        .map_err(|e| ErrorOut::from(&*e))
}

/// Data for the table tab (§9.5): SQL generated by the app, shared
/// connection (§3.1). Same events as `execute`: the UI reuses everything.
/// `query` carries server-side sort and filters (decision 10, second half).
#[tauri::command]
async fn table_data(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    query: alamo_session::TableQuery,
    on_event: Channel<ExecEvent>,
) -> Result<u64, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .fetch_table(
            &schema,
            &table,
            &query,
            session.fetch_options(),
            move |ev| {
                let _ = on_event.send(ev);
            },
        )
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// Next page of the table tab, appended to the open result set: the grid
/// asks for it when reaching the bottom. Replies once the page has arrived.
#[tauri::command]
async fn table_data_more(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    query: alamo_session::TableQuery,
    result_id: u64,
) -> Result<alamo_session::TableMoreOut, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .fetch_table_more(&schema, &table, &query, result_id)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// The SELECT generated for the table tab (server-side sort and filters), to
/// show and to open as a query: it is exactly the one of `table_data`.
#[tauri::command]
async fn table_data_sql(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    query: alamo_session::TableQuery,
) -> Result<String, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    Ok(session.table_sql(&schema, &table, &query))
}

// ---------------------------------------------------------------- export (dec. 11)

#[derive(Serialize)]
struct ExportOut {
    path: String,
    rows: u64,
}

/// Export to file (decision 11): native save dialog, streaming write from
/// the registry — full values (`cell_full`), never the window truncation.
/// `None` = cancelled by the user.
#[tauri::command]
async fn export_result(
    app: AppHandle,
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
    opts: alamo_session::export::ExportOptions,
    suggested_name: String,
) -> Result<Option<ExportOut>, ErrorOut> {
    use tauri_plugin_dialog::DialogExt;
    let session = state.session(conn_id).map_err(internal)?;
    #[cfg(feature = "e2e")]
    let chosen = e2e::take_pick();
    #[cfg(not(feature = "e2e"))]
    let chosen: Option<std::path::PathBuf> = None;
    let path = match chosen {
        Some(p) => p,
        None => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.dialog()
                .file()
                .set_file_name(&suggested_name)
                .save_file(move |p| {
                    let _ = tx.send(p);
                });
            let Ok(Some(picked)) = rx.await else {
                return Ok(None);
            };
            picked.into_path().map_err(|e| internal(e.to_string()))?
        }
    };
    // Write on a blocking thread: a large result set must not occupy the
    // command runtime.
    let path2 = path.clone();
    let rows = tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let file = std::fs::File::create(&path2)
            .map_err(|e| Box::new(internal(format!("opening file: {e}"))))?;
        let mut out = std::io::BufWriter::new(file);
        out.write_all(opts.bom_bytes())
            .map_err(|e| Box::new(internal(format!("writing: {e}"))))?;
        let rows = session
            .export_result(result_id, &opts, None, None, &mut out)
            .map_err(|e| Box::new(ErrorOut::from(&*e)))?;
        out.flush()
            .map_err(|e| Box::new(internal(format!("writing: {e}"))))?;
        Ok::<u64, Box<ErrorOut>>(rows)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(|e| *e)?;
    Ok(Some(ExportOut {
        path: path.display().to_string(),
        rows,
    }))
}

/// Where the results of a script go: one file with a section per statement,
/// or a new folder with one file per statement.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScriptExportTarget {
    File,
    Folder,
}

/// Export of all the results of a script (like `export_result`, native
/// dialog and streaming write). `path` is the file or the folder created.
/// `None` = cancelled by the user.
#[tauri::command]
async fn export_script(
    app: AppHandle,
    state: State<'_, AppState>,
    conn_id: u64,
    items: Vec<alamo_session::export::ScriptItem>,
    opts: alamo_session::export::ExportOptions,
    target: ScriptExportTarget,
    suggested_name: String,
) -> Result<Option<ExportOut>, ErrorOut> {
    use tauri_plugin_dialog::DialogExt;
    let session = state.session(conn_id).map_err(internal)?;
    #[cfg(feature = "e2e")]
    let chosen = e2e::take_pick();
    #[cfg(not(feature = "e2e"))]
    let chosen: Option<std::path::PathBuf> = None;
    let picked = match chosen {
        Some(p) => p,
        None => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let dialog = app.dialog().file();
            match target {
                ScriptExportTarget::File => dialog
                    .set_file_name(format!("{suggested_name}.{}", opts.format.extension()))
                    .save_file(move |p| {
                        let _ = tx.send(p);
                    }),
                ScriptExportTarget::Folder => dialog.pick_folder(move |p| {
                    let _ = tx.send(p);
                }),
            }
            let Ok(Some(picked)) = rx.await else {
                return Ok(None);
            };
            picked.into_path().map_err(|e| internal(e.to_string()))?
        }
    };
    let (path, rows) = tokio::task::spawn_blocking(move || match target {
        ScriptExportTarget::File => {
            use std::io::Write as _;
            let file = std::fs::File::create(&picked)
                .map_err(|e| Box::new(internal(format!("opening file: {e}"))))?;
            let mut out = std::io::BufWriter::new(file);
            out.write_all(opts.bom_bytes())
                .map_err(|e| Box::new(internal(format!("writing: {e}"))))?;
            let rows = session
                .export_script(&items, &opts, &mut out)
                .map_err(|e| Box::new(ErrorOut::from(&*e)))?;
            out.flush()
                .map_err(|e| Box::new(internal(format!("writing: {e}"))))?;
            Ok::<_, Box<ErrorOut>>((picked, rows))
        }
        ScriptExportTarget::Folder => session
            .export_script_to_dir(&items, &opts, &picked, &suggested_name)
            .map_err(|e| Box::new(ErrorOut::from(&*e))),
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(|e| *e)?;
    Ok(Some(ExportOut {
        path: path.display().to_string(),
        rows,
    }))
}

/// Grid selection rectangle, in view coordinates.
#[derive(Deserialize)]
struct CellRange {
    row_start: u64,
    row_end: u64,
    col_start: usize,
    col_end: usize,
}

/// Formatted copy of the selection (decision 11). The clipboard is written
/// here in Rust: `navigator.clipboard` in the WKWebView dies silently.
#[tauri::command]
async fn copy_result_cells(
    app: AppHandle,
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
    range: CellRange,
    opts: alamo_session::export::ExportOptions,
) -> Result<u64, ErrorOut> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let session = state.session(conn_id).map_err(internal)?;
    // Materialization on a blocking thread: a large selection must not
    // occupy the command runtime (same pattern as export_result).
    let (rows, text) = tokio::task::spawn_blocking(move || {
        let mut buf = Vec::new();
        let rows = session
            .export_result(
                result_id,
                &opts,
                Some(range.row_start..range.row_end),
                Some(range.col_start..range.col_end),
                &mut buf,
            )
            .map_err(|e| Box::new(ErrorOut::from(&*e)))?;
        let text = String::from_utf8(buf).map_err(|e| Box::new(internal(e.to_string())))?;
        Ok::<(u64, String), Box<ErrorOut>>((rows, text))
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(|e| *e)?;
    app.clipboard()
        .write_text(text)
        .map_err(|e| internal(e.to_string()))?;
    Ok(rows)
}

// ---------------------------------------------------------------- transactions

/// Transaction state of the sticky (§9.7): the UI asks for it at the end of
/// an execution and after commit/rollback, never by polling.
#[tauri::command]
async fn txn_state(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<alamo_session::TxnStateOut, String> {
    Ok(state.session(conn_id)?.txn_state())
}

/// Writes of the open transaction (§9.7.2), listed before Commit.
#[tauri::command]
async fn txn_log(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<alamo_session::txn::TxnLogOut, String> {
    Ok(state.session(conn_id)?.txn_log())
}

/// Manual commit on a non-prod connection (§9.7 toolbar switch).
#[tauri::command]
async fn set_manual_commit(
    state: State<'_, AppState>,
    conn_id: u64,
    on: bool,
) -> Result<(), ErrorOut> {
    state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?
        .set_manual_commit(on)
        .map_err(|e| ErrorOut::from(&*e))
}

/// Explicit COMMIT (§9.7 button). Returns the updated state.
#[tauri::command]
async fn txn_commit(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<alamo_session::TxnStateOut, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session.commit().await.map_err(|e| ErrorOut::from(&*e))
}

/// Explicit ROLLBACK (§9.7 button). Returns the updated state.
#[tauri::command]
async fn txn_rollback(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<alamo_session::TxnStateOut, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session.rollback().await.map_err(|e| ErrorOut::from(&*e))
}

#[derive(Serialize)]
struct StatementClassOut {
    index: u32,
    char_offset: u32,
    /// "read" | "write" | "ddl" | "control" | "unknown"
    class: &'static str,
    /// For the prod gates (§9.7.3): unknown counts as a write.
    is_write: bool,
    /// Confirmation always required (§9.7 point 3).
    irreversible: bool,
    /// `UPDATE`/`DELETE` without WHERE: confirmed on every profile.
    unfiltered: bool,
}

/// Boundaries of a statement in the editor text, in characters (code
/// points): the frontend needs them for "run the query under the cursor" and
/// to highlight it. The splitter is the same as the execution's (§6.5): what
/// is highlighted is exactly what runs.
#[derive(Serialize)]
struct StatementRangeOut {
    index: u32,
    char_offset: u32,
    char_len: u32,
}

#[tauri::command]
async fn split_sql(
    state: State<'_, AppState>,
    conn_id: u64,
    sql: String,
) -> Result<Vec<StatementRangeOut>, String> {
    let session = state.session(conn_id)?;
    let caps = session.capabilities();
    Ok(
        alamo_core::splitter::split_statements(&sql, caps.backslash_escapes)
            .iter()
            .map(|stmt| StatementRangeOut {
                index: stmt.origin.index,
                char_offset: stmt.origin.char_offset,
                char_len: stmt.sql.chars().count() as u32,
            })
            .collect(),
    )
}

/// Conservative classification of the batch (§9.7.3), for the UI gates
/// before execution: prod profiles, and `UPDATE`/`DELETE` without WHERE on
/// every profile.
#[tauri::command]
async fn classify_sql(
    state: State<'_, AppState>,
    conn_id: u64,
    sql: String,
) -> Result<Vec<StatementClassOut>, String> {
    use alamo_core::classify::{classify_statement, StatementClass};
    let session = state.session(conn_id)?;
    let caps = session.capabilities();
    let transactional_ddl = caps.transactional_ddl;
    Ok(
        alamo_core::splitter::split_statements(&sql, caps.backslash_escapes)
            .iter()
            .map(|stmt| {
                let info = classify_statement(&stmt.sql, transactional_ddl);
                StatementClassOut {
                    index: stmt.origin.index,
                    char_offset: stmt.origin.char_offset,
                    class: match info.class {
                        StatementClass::Read => "read",
                        StatementClass::Write => "write",
                        StatementClass::Ddl => "ddl",
                        StatementClass::Control => "control",
                        StatementClass::Unknown => "unknown",
                    },
                    is_write: info.class.counts_as_write(),
                    irreversible: info.irreversible,
                    unfiltered: info.unfiltered,
                }
            })
            .collect(),
    )
}

/// Estimate of the affected rows via `EXPLAIN` (§9.7), for the confirmation
/// dialogs. Best-effort by contract: any error degrades to `None` — the
/// confirmation must never stay blocked by the estimate.
#[tauri::command]
async fn explain_estimate(
    state: State<'_, AppState>,
    conn_id: u64,
    sql: String,
) -> Result<Option<u64>, String> {
    let session = state.session(conn_id)?;
    Ok(session.explain_estimate(&sql).await.unwrap_or(None))
}

#[derive(Serialize)]
struct EngineOut {
    id: &'static str,
    name: &'static str,
    short_name: &'static str,
    default_port: u16,
    default_user: &'static str,
    database_required: bool,
    tls_modes: Vec<&'static str>,
    sql_syntax: &'static str,
    /// The database belongs to the tab and is switched with `USE` (MySQL);
    /// otherwise it is fixed at connect (PG).
    can_change_database_in_session: bool,
    /// §9.7.1: false on MySQL — rollback does not save you from DDL.
    transactional_ddl: bool,
}

/// The engines of this build (§2.2): the connection dialog lists them.
#[tauri::command]
fn engines(state: State<'_, AppState>) -> Vec<EngineOut> {
    state
        .drivers
        .engines()
        .into_iter()
        .map(|e| EngineOut {
            id: e.id,
            name: e.name,
            short_name: e.short_name,
            default_port: e.default_port,
            default_user: e.default_user,
            database_required: e.database_required,
            tls_modes: e.tls_modes.iter().map(|m| m.as_str()).collect(),
            sql_syntax: e.sql_syntax,
            can_change_database_in_session: e.capabilities.can_change_database_in_session,
            transactional_ddl: e.capabilities.transactional_ddl,
        })
        .collect()
}

/// Preview of the grid editing UPDATE (M4): the confirmation shows the SQL
/// generated from the same parameters that `table_update` then executes —
/// deterministic generation, the preview cannot lie.
#[tauri::command]
async fn table_update_sql(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    set: Vec<alamo_session::CellAssign>,
    key: Vec<alamo_session::CellAssign>,
) -> Result<String, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .update_sql(&schema, &table, &set, &key)
        .map_err(|e| ErrorOut::from(&*e))
}

/// UPDATE of one row from grid editing (M4): shared lane in auto-commit,
/// same events as `execute` — the outcome arrives as `Affected { rows }`.
#[tauri::command]
async fn table_update(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    set: Vec<alamo_session::CellAssign>,
    key: Vec<alamo_session::CellAssign>,
    on_event: Channel<ExecEvent>,
) -> Result<u64, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .update_table(
            &schema,
            &table,
            &set,
            &key,
            FetchOptions::default(),
            move |ev| {
                let _ = on_event.send(ev);
            },
        )
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// Preview of the multi-cell batch (decision 12): same deterministic
/// generation that `table_update_batch` then executes.
#[tauri::command]
async fn table_update_sql_batch(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    updates: Vec<alamo_session::RowUpdate>,
) -> Result<String, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .update_sql_batch(&schema, &table, &updates)
        .map_err(|e| ErrorOut::from(&*e))
}

/// Multi-cell/paste batch (decision 12): one UPDATE per row. Blocking,
/// outcome in the return value — normal profile in an atomic transaction on
/// the shared lane, prod inside the sticky's transaction (§9.7).
#[tauri::command]
async fn table_update_batch(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    updates: Vec<alamo_session::RowUpdate>,
) -> Result<alamo_session::BatchUpdateOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .update_table_batch(&schema, &table, &updates)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// Preview of the mixed batch (UPDATE cells + INSERT new rows), same
/// generation as `table_write_batch`.
#[tauri::command]
async fn table_write_sql_batch(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    writes: Vec<alamo_session::TableWrite>,
) -> Result<String, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .write_sql_batch(&schema, &table, &writes)
        .map_err(|e| ErrorOut::from(&*e))
}

/// Mixed batch of the table tab: UPDATE of the cells and INSERT of the new
/// rows ("+ Row" / "Duplicate"), same semantics as `table_update_batch`.
#[tauri::command]
async fn table_write_batch(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    writes: Vec<alamo_session::TableWrite>,
    append: Option<alamo_session::AppendTo>,
) -> Result<alamo_session::BatchUpdateOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .write_table_batch(&schema, &table, &writes, append.as_ref())
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// After a save from the grid: re-reads the saved rows and replaces them in
/// the result set, without reloading the table.
#[tauri::command]
async fn table_refresh_rows(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
    result_id: u64,
    updates: Vec<alamo_session::RowUpdate>,
) -> Result<alamo_session::RefreshRowsOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .refresh_table_rows(&schema, &table, result_id, &updates)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// §4.3.1: the table a result set can be written to, with its key, or the
/// reason it is read-only (SQL tab editing).
#[tauri::command]
async fn result_source(
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
) -> Result<alamo_session::ResultSourceOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .result_source(result_id)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// DDL rebuilt from the catalog (DDL sub-tab, §9.5).
#[tauri::command]
async fn table_ddl(
    state: State<'_, AppState>,
    conn_id: u64,
    schema: String,
    table: String,
) -> Result<String, ErrorOut> {
    let session = state
        .session(conn_id)
        .map_err(|m| ErrorOut::from(&alamo_core::error::DbError::internal(m)))?;
    session
        .table_ddl(&schema, &table)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// Current database of the sticky, for the toolbar context: on MySQL it
/// reflects the last `USE`. The UI asks for it once connected and at the end
/// of an execution, never during.
#[tauri::command]
async fn current_database(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<Option<String>, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .current_database()
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

/// `USE` on the sticky (MySQL only): the database select in the toolbar.
#[tauri::command]
async fn use_database(
    state: State<'_, AppState>,
    conn_id: u64,
    db: String,
) -> Result<(), ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    session
        .use_database(&db)
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

#[tauri::command]
async fn cancel(state: State<'_, AppState>, conn_id: u64, exec_id: u64) -> Result<(), String> {
    if let Ok(s) = state.session(conn_id) {
        s.cancel(exec_id);
    }
    Ok(())
}

#[tauri::command]
async fn budget_resume(
    state: State<'_, AppState>,
    conn_id: u64,
    exec_id: u64,
) -> Result<(), String> {
    if let Ok(s) = state.session(conn_id) {
        s.budget_resume(exec_id);
    }
    Ok(())
}

/// Rebuilds the connection's SSH tunnel on the same local port. Without a
/// tunnel it does nothing. If reopening fails the slot stays (without a
/// tunnel): the next round retries, instead of leaving the session
/// knocking on a listener that no longer exists.
async fn rebuild_tunnel(state: &AppState, conn_id: u64) -> Result<(), ErrorOut> {
    let Some(slot) = locked(&state.tunnels).remove(&conn_id) else {
        return Ok(());
    };
    let TunnelSlot {
        tunnel,
        cfg,
        net,
        port,
    } = slot;
    // The old one closes listener and SSH: the port is freed for the new one.
    drop(tunnel);
    let opened = alamo_tunnel::Tunnel::open_at(cfg.clone(), Some(port), net).await;
    let (tunnel, result) = match opened {
        Ok(t) => (Some(t), Ok(())),
        Err(e) => (None, Err(internal(e.to_string()))),
    };
    locked(&state.tunnels).insert(
        conn_id,
        TunnelSlot {
            tunnel,
            cfg,
            net,
            port,
        },
    );
    result
}

/// Before using the editor's connection: if it dropped it reopens by
/// itself (the session does not stay dead until the profile is closed and reopened).
///
/// With the SSH tunnel dropped together with the network, reopening only the
/// session would fail forever: the local listener answers but no channel
/// opens ("connection closed"). First the tunnel, then the session; and if the
/// session does not restart with a tunnel that looks alive (the keepalive has
/// not noticed yet), a second round with the tunnel rebuilt.
#[tauri::command]
async fn ensure_connected(
    state: State<'_, AppState>,
    settings: State<'_, settings::SettingsStore>,
    conn_id: u64,
) -> Result<alamo_session::ReconnectOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    // §16: with automatic reconnection off nothing is rebuilt, the user
    // decides from the note.
    let auto = settings.snapshot().connections.auto_reconnect;
    // The tunnel belongs to the profile's connection (§3.3).
    let owner = state.root(conn_id);
    let tunnel_closed = locked(&state.tunnels)
        .get(&owner)
        .map(TunnelSlot::is_closed);
    if tunnel_closed == Some(true) {
        if !auto {
            return Ok(alamo_session::ReconnectOut {
                reconnected: false,
                dead: true,
                lost_txn: false,
                lost_writes: false,
            });
        }
        rebuild_tunnel(&state, owner).await?;
    }
    match session.ensure_connected_opts(auto).await {
        Ok(rc) => Ok(rc),
        Err(_) if auto && tunnel_closed == Some(false) => {
            rebuild_tunnel(&state, owner).await?;
            session
                .ensure_connected()
                .await
                .map_err(|e| ErrorOut::from(&*e))
        }
        Err(e) => Err(ErrorOut::from(&*e)),
    }
}

/// "Reconnect" (connection menu, connection page): tunnel and session
/// rebuilt right away, without the check of `ensure_connected`, which
/// with the network just back can wait for minutes. Rebuilding the tunnel
/// also closes the calls left hanging on the old one.
#[tauri::command]
async fn reconnect(
    state: State<'_, AppState>,
    conn_id: u64,
) -> Result<alamo_session::ReconnectOut, ErrorOut> {
    let session = state.session(conn_id).map_err(internal)?;
    if session.registry.has_active_execution() {
        return Err(internal("running on this connection: cancel it first"));
    }
    rebuild_tunnel(&state, state.root(conn_id)).await?;
    session
        .force_reconnect()
        .await
        .map_err(|e| ErrorOut::from(&*e))
}

#[tauri::command]
async fn budget_more(state: State<'_, AppState>, conn_id: u64, exec_id: u64) -> Result<(), String> {
    if let Ok(s) = state.session(conn_id) {
        s.budget_more(exec_id);
    }
    Ok(())
}

#[tauri::command]
async fn budget_stop(state: State<'_, AppState>, conn_id: u64, exec_id: u64) -> Result<(), String> {
    if let Ok(s) = state.session(conn_id) {
        s.budget_stop(exec_id);
    }
    Ok(())
}

// ---------------------------------------------------------------- results

#[derive(Serialize)]
struct WindowColOut {
    index: usize,
    values: Vec<String>,
    nulls: Vec<bool>,
    truncated: Vec<bool>,
}

#[derive(Serialize)]
struct WindowOut {
    row_start: u64,
    row_end: u64,
    columns: Vec<WindowColOut>,
}

#[tauri::command]
fn fetch_window(
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
    row_start: u64,
    row_end: u64,
    col_start: usize,
    col_end: usize,
) -> Result<WindowOut, String> {
    let session = state.session(conn_id)?;
    let live = session
        .result(result_id)
        .ok_or_else(|| format!("unknown result_id: {result_id}"))?;
    let w = live.window(row_start..row_end, col_start..col_end);
    Ok(WindowOut {
        row_start: w.rows.start,
        row_end: w.rows.end,
        columns: w
            .columns
            .into_iter()
            .map(|c| WindowColOut {
                index: c.index,
                values: c.values,
                nulls: c.nulls,
                truncated: c.truncated,
            })
            .collect(),
    })
}

#[derive(Serialize)]
struct SortOut {
    rows: u64,
    incomplete: bool,
}

/// Local sort on a permutation (§4.8, decision 10). `col: None` removes the
/// view. `incomplete` feeds the badge "only the N downloaded rows sorted"
/// (§7.3).
#[tauri::command]
fn sort_result(
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
    col: Option<usize>,
    desc: bool,
) -> Result<SortOut, String> {
    let session = state.session(conn_id)?;
    let live = session
        .result(result_id)
        .ok_or_else(|| format!("unknown result_id: {result_id}"))?;
    let info = match col {
        Some(c) => live.sort(c, desc),
        None => live.clear_sort(),
    };
    Ok(SortOut {
        rows: info.rows,
        incomplete: info.incomplete,
    })
}

/// Full value for the inspection panel (LOBs resolved, §4.7).
#[tauri::command]
fn get_cell_full(
    state: State<'_, AppState>,
    conn_id: u64,
    result_id: u64,
    row: u64,
    col: usize,
) -> Result<Option<String>, String> {
    let session = state.session(conn_id)?;
    let live = session
        .result(result_id)
        .ok_or_else(|| format!("unknown result_id: {result_id}"))?;
    Ok(live.cell_full(row, col))
}

#[tauri::command]
fn release_result(state: State<'_, AppState>, conn_id: u64, result_id: u64) {
    if let Ok(s) = state.session(conn_id) {
        s.release_result(result_id);
    }
}

// ---------------------------------------------------------------- harness M0

#[derive(Serialize)]
struct ColOut {
    name: String,
    type_display: String,
}

#[derive(Serialize)]
struct BenchQueryOut {
    conn_id: u64,
    result_id: u64,
    rows: u64,
    cols: Vec<ColOut>,
    time_to_first_row_ms: f64,
    total_ms: f64,
}

/// Bench path: full collection with `execute_collect` for the statistics
/// of §15, result set handed to the registry so `fetch_window` is the only one.
/// Only with the harness (ALAMO_M0_AUTO): runs SQL on any session
/// without the gates of `execute` (read-only included).
#[tauri::command]
async fn bench_run_query(state: State<'_, AppState>, sql: String) -> Result<BenchQueryOut, String> {
    if !auto_mode_flag() {
        return Err("bench_run_query available only with ALAMO_M0_AUTO=1".into());
    }
    let (conn_id, session) = match state.any_session() {
        Some(p) => p,
        None => {
            let url = std::env::var("ALAMO_BENCH_URL")
                .map_err(|_| "ALAMO_BENCH_URL not set".to_string())?;
            let session = Session::connect(&url).await.map_err(|e| e.to_string())?;
            let id = state.insert(session);
            (id, state.session(id)?)
        }
    };
    let (rs, stats) = execute_collect(
        &session
            .pg()
            .ok_or_else(|| "the M0 bench runs only on PostgreSQL".to_string())?
            .client,
        &sql,
        &FetchOptions {
            soft_budget_bytes: None,
            ..FetchOptions::default()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let cols = rs
        .schema()
        .iter()
        .map(|m| ColOut {
            name: m.name.clone(),
            type_display: m.type_display.clone(),
        })
        .collect();
    let out = BenchQueryOut {
        conn_id,
        result_id: 0,
        rows: rs.rows(),
        cols,
        time_to_first_row_ms: stats
            .time_to_first_row
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0),
        total_ms: stats.total.as_secs_f64() * 1000.0,
    };
    let result_id = session.registry.new_result_done(rs);
    Ok(BenchQueryOut { result_id, ..out })
}

#[tauri::command]
fn report_ready(state: State<'_, AppState>) -> f64 {
    let ms = state.started.elapsed().as_secs_f64() * 1000.0;
    let verdict = if ms < 1200.0 {
        "GREEN"
    } else if ms <= 2500.0 {
        "YELLOW"
    } else {
        "RED"
    };
    println!("cold start (main -> first webview frame): {ms:.0} ms  {verdict}   [<1200 / 1200-2500 / >2500]");
    flush();
    ms
}

#[derive(Debug, Serialize, Deserialize)]
struct SlowFrame {
    at_ms: f64,
    delta_ms: f64,
    scroll_top: f64,
    phase: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct FrameReport {
    frames: u64,
    duration_ms: f64,
    avg_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    over_16_6: u64,
    over_50: u64,
    #[serde(default)]
    slow_frames: Vec<SlowFrame>,
    #[serde(default)]
    phase_p95_ms: Vec<f64>,
    #[serde(default)]
    suspended_frames: u64,
    #[serde(default)]
    suspended_ms: f64,
}

#[tauri::command]
fn report_frames(app: AppHandle, report: FrameReport) {
    // §15: p95 <= 16.6 and ZERO frames > 50 = green; p95 <= 25 and max 3 = yellow.
    let verdict = if report.p95_ms <= 16.6 && report.over_50 == 0 {
        "GREEN"
    } else if report.p95_ms <= 25.0 && report.over_50 <= 3 {
        "YELLOW"
    } else {
        "RED"
    };
    println!("=== M0 phase 2 — frame time, continuous scroll 20k rows ===");
    if let Ok(p) = std::env::var("ALAMO_M0_PROFILE") {
        println!("  (profile: {p})");
    }
    if preload_flag() {
        println!("  (preload mode: warm cache, scroll without IPC)");
    }
    println!(
        "  frame: {} in {:.1} s",
        report.frames,
        report.duration_ms / 1000.0
    );
    println!(
        "  avg {:.2} ms · p50 {:.2} · p95 {:.2} · p99 {:.2} · max {:.1}",
        report.avg_ms, report.p50_ms, report.p95_ms, report.p99_ms, report.max_ms
    );
    println!(
        "  frame > 16.6 ms: {} · frame > 50 ms: {}",
        report.over_16_6, report.over_50
    );
    if !report.phase_p95_ms.is_empty() {
        println!(
            "  p95 per phase (slow/medium/fling/return): {}",
            report
                .phase_p95_ms
                .iter()
                .map(|v| format!("{v:.1}"))
                .collect::<Vec<_>>()
                .join(" / ")
        );
    }
    if report.suspended_frames > 0 {
        println!(
            "  ⚠️ {} rAF suspensions (window occluded) for {:.1} s in total — excluded from the statistics",
            report.suspended_frames,
            report.suspended_ms / 1000.0
        );
    }
    for s in &report.slow_frames {
        println!(
            "  slow frame: {:.0} ms at t={:.1} s, scrollTop={:.0}, phase {}",
            s.delta_ms,
            s.at_ms / 1000.0,
            s.scroll_top,
            s.phase
        );
    }
    println!(
        "  verdict: {verdict}   [green: p95<=16.6 and zero >50 / yellow: p95<=25 and max 3 >50]"
    );
    if let Ok(path) = std::env::var("ALAMO_M0_OUT") {
        let json = serde_json::json!({ "report": report, "esito": verdict });
        if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()) {
            eprintln!("writing {path} failed: {e}");
        }
    }
    flush();
    if auto_mode_flag() {
        app.exit(0);
    }
}

#[tauri::command]
fn report_error(app: AppHandle, message: String) {
    eprintln!("harness error: {message}");
    flush();
    if auto_mode_flag() {
        app.exit(1);
    }
}

/// M1 smoke test: the UI runs a batch by itself and reports the outcomes.
#[tauri::command]
fn m1_smoke() -> bool {
    std::env::var("ALAMO_M1_SMOKE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[tauri::command]
fn smoke_done(app: AppHandle, summary: String, ok: bool) {
    println!("=== M1 smoke ===");
    println!("{summary}");
    println!("outcome: {}", if ok { "OK" } else { "FAILED" });
    flush();
    if m1_smoke() {
        app.exit(if ok { 0 } else { 1 });
    }
}

#[derive(Serialize)]
struct HarnessMode {
    auto: bool,
    preload: bool,
    fling: f64,
    profile: String,
}

#[tauri::command]
fn harness_mode() -> HarnessMode {
    HarnessMode {
        auto: auto_mode_flag(),
        preload: preload_flag(),
        fling: std::env::var("ALAMO_M0_FLING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(260.0),
        profile: std::env::var("ALAMO_M0_PROFILE").unwrap_or_else(|_| "continuo".into()),
    }
}

/// §13.1: Keychain service derived from the identifier, never rewritten by
/// hand. The e2e build uses a separate one, so as not to touch the real passwords.
fn keychain_service(config: &tauri::Config) -> String {
    let service = config.identifier.clone();
    #[cfg(feature = "e2e")]
    let service = e2e::keychain_service(&service);
    service
}

/// No saved window state yet: the window opens maximized on the screen it
/// lands on, whatever its size. From then on the window-state plugin
/// restores the last size, position and maximized state.
fn maximize_on_first_launch(app: &tauri::App) -> tauri::Result<()> {
    let saved = app
        .path()
        .app_config_dir()?
        .join(tauri_plugin_window_state::DEFAULT_FILENAME);
    if !saved.exists() {
        if let Some(w) = app.get_webview_window("main") {
            w.maximize()?;
        }
    }
    Ok(())
}

fn auto_mode_flag() -> bool {
    std::env::var("ALAMO_M0_AUTO")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn preload_flag() -> bool {
    std::env::var("ALAMO_M0_PRELOAD")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn flush() {
    let _ = std::io::stdout().flush();
}

/// Close confirmed by the "pending writes" dialog: raises the flag and exits.
/// The connections drop with the process and the server runs the ROLLBACK — it is the
/// user's explicit choice, not an accident.
#[tauri::command]
fn app_quit(app: AppHandle, state: State<'_, AppState>) {
    state.quit_confirmed.store(true, Ordering::Relaxed);
    app.exit(0);
}

/// Integrated title bar (macOS, `titleBarStyle: Overlay`): the app's toolbar
/// acts as the title bar, so it must be able to drag the
/// window. Our own command instead of `data-tauri-drag-region`: that one
/// goes through the window plugin and would require an ACL capability, which we
/// do not use here.
#[tauri::command]
fn window_start_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

/// New published version (Tauri updater, endpoint in tauri.conf.json).
#[derive(Serialize)]
struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
}

/// Check at startup and on "Check now". Never in debug: `cargo xtask run`
/// must not offer to replace itself with the published build — an error,
/// so "Check now" says why instead of a false "up to date".
#[tauri::command]
async fn update_check(app: AppHandle) -> Result<Option<UpdateInfo>, ErrorOut> {
    if cfg!(debug_assertions) {
        return Err(internal("Update checks are off in development builds"));
    }
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| internal(e.to_string()))?;
    let update = updater.check().await.map_err(|e| internal(e.to_string()))?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        current_version: u.current_version,
        notes: u.body,
    }))
}

/// Downloads, installs and restarts. On Windows the installer closes the process
/// with `exit` and bypasses the close gates (§9.7.2): with pending
/// writes it refuses before downloading, never a silent ROLLBACK.
#[tauri::command]
async fn update_install(app: AppHandle) -> Result<(), ErrorOut> {
    if writes_pending(&app.state::<AppState>()) {
        return Err(internal(
            "There are uncommitted writes or unsaved changes: close them before updating.",
        ));
    }
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| internal(e.to_string()))?;
    let Some(update) = updater.check().await.map_err(|e| internal(e.to_string()))? else {
        return Err(internal("No update available."));
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| internal(e.to_string()))?;
    app.restart();
}

/// Double click on the bar: maximize/restore, like a real title
/// bar.
#[tauri::command]
fn window_toggle_maximize(window: tauri::Window) {
    let maximized = window.is_maximized().unwrap_or(false);
    let _ = if maximized {
        window.unmaximize()
    } else {
        window.maximize()
    };
}

/// macOS menu bar in the UI language. Custom menu WITHOUT Undo/Redo: the
/// default menu binds them to Cmd+Z/⇧Cmd+Z towards the native NSUndoManager,
/// which knows nothing about the CodeMirror history: the key would never reach
/// the webview. Without an accelerator in the menu, Cmd+Z goes down to the DOM
/// and the editor handles it. Cut/Copy/Paste/SelectAll stay: those are needed
/// native (password fields, webview). The standard items keep the labels of
/// the menu library.
#[cfg(target_os = "macos")]
fn build_menu(app: &AppHandle, italian: bool) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    // Quit: our own item, not `.quit()`. The default one sends
    // `terminate:` to NSApp and tao does not implement
    // `applicationShouldTerminate`: the process dies without ever
    // going through ExitRequested, so Cmd+Q bypassed the
    // pending-writes gate. `app.exit(0)` instead
    // goes through it (see `.run` at the bottom).
    let quit = MenuItemBuilder::with_id(
        "quit",
        if italian {
            "Esci da Alamo DB"
        } else {
            "Quit Alamo DB"
        },
    )
    .accelerator("CmdOrCtrl+Q")
    .build(app)?;
    // Settings… ⌘, in the app menu, as every macOS app: the webview opens
    // the window on the DOM event (`on_menu_event`).
    let settings = MenuItemBuilder::with_id(
        "settings",
        if italian {
            "Impostazioni…"
        } else {
            "Settings…"
        },
    )
    .accelerator("Cmd+,")
    .build(app)?;
    let app_menu = SubmenuBuilder::new(app, "Alamo")
        .about(None)
        .separator()
        .item(&settings)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .item(&quit)
        .build()?;
    let edit = SubmenuBuilder::new(app, if italian { "Modifica" } else { "Edit" })
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let window = SubmenuBuilder::new(app, if italian { "Finestra" } else { "Window" })
        .minimize()
        .maximize()
        .build()?;
    MenuBuilder::new(app)
        .items(&[&app_menu, &edit, &window])
        .build()
}

/// macOS: the native menu follows the UI language (§16); rebuilt only when
/// the language changed. No-op elsewhere.
fn menu_follow_language(
    app: &AppHandle,
    before: settings::Language,
    now: &settings::Settings,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if now.appearance.language != before {
        let italian = now.appearance.language == settings::Language::It;
        app.set_menu(build_menu(app, italian).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, before, now);
    Ok(())
}

/// After a settings change (§16): the Execution settings in force on every
/// open connection, pushed into the sessions when an Execution value
/// changed (each one keeps its profile's overrides), and returned for the
/// frontend either way.
async fn apply_limits(
    app: &AppHandle,
    before: &settings::Settings,
    after: &settings::Settings,
) -> Vec<EffectiveOut> {
    let changed = before.execution != after.execution;
    let mut out = Vec::new();
    for (conn_id, session, execution) in effective_executions(app, after, None) {
        if changed {
            push_execution(&session, &execution).await;
        }
        out.push(EffectiveOut { conn_id, execution });
    }
    out
}

/// "Copy diagnostics" (§16): what a bug report needs and nothing that
/// identifies the user's servers: version, system, webview, engines with
/// their server versions, the settings that differ from the defaults and
/// where the file is. No hosts, profile names, passwords or SQL.
#[tauri::command]
fn diagnostics_text(
    app: AppHandle,
    state: State<'_, AppState>,
    store: State<'_, settings::SettingsStore>,
) -> String {
    let info = app.package_info();
    let mut lines = vec![
        format!(
            "{} {}{}",
            info.name,
            info.version,
            if cfg!(debug_assertions) {
                " (development build)"
            } else {
                ""
            }
        ),
        format!(
            "System: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        format!(
            "WebView: {}",
            tauri::webview_version().unwrap_or_else(|_| "unknown".into())
        ),
    ];
    let sessions: Vec<Arc<Session>> = locked(&state.sessions).values().cloned().collect();
    let mut engines: Vec<(String, usize)> = Vec::new();
    for session in sessions {
        let name = format!(
            "{} {}",
            session.engine_name(),
            session.server_version().unwrap_or_else(|| "?".into())
        );
        match engines.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => engines.push((name, 1)),
        }
    }
    lines.push(if engines.is_empty() {
        "Connections: none".into()
    } else {
        format!(
            "Connections: {}",
            engines
                .iter()
                .map(|(n, c)| format!("{n} ({c})"))
                .collect::<Vec<_>>()
                .join("; ")
        )
    });
    let changed = settings::non_default_leaves(&store.snapshot());
    lines.push(if changed.is_empty() {
        "Settings: defaults".into()
    } else {
        format!(
            "Settings: {}",
            changed
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    lines.push(format!("Settings file: {}", store.path().display()));
    lines.join("\n")
}

/// "Open settings folder" (§16): settings.json selected in the file
/// manager, so a hand edit or a bug report starts from the right file.
#[tauri::command]
fn settings_reveal(
    app: AppHandle,
    store: State<'_, settings::SettingsStore>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(store.path())
        .map_err(|e| e.to_string())
}

/// The Execution settings in force on one open connection (§16).
#[derive(Serialize)]
struct EffectiveOut {
    conn_id: u64,
    execution: settings::Execution,
}

/// The settings (§16) as the frontend receives them, with the problems
/// found in the file at load (only the first `settings_get` carries them)
/// and the Execution settings in force on each open connection.
#[derive(Serialize)]
struct SettingsOut {
    settings: settings::Settings,
    issues: Vec<settings::Issue>,
    effective: Vec<EffectiveOut>,
}

#[tauri::command]
fn settings_get(app: AppHandle, store: State<'_, settings::SettingsStore>) -> SettingsOut {
    let settings = store.snapshot();
    let effective = effective_executions(&app, &settings, None)
        .into_iter()
        .map(|(conn_id, _, execution)| EffectiveOut { conn_id, execution })
        .collect();
    SettingsOut {
        settings,
        issues: store.take_issues(),
        effective,
    }
}

/// `patch`: `category.field` → value, all applied or none (§16).
#[tauri::command]
async fn settings_set(
    app: AppHandle,
    store: State<'_, settings::SettingsStore>,
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<SettingsOut, String> {
    let before = store.snapshot();
    let settings = store.apply(&patch)?;
    menu_follow_language(&app, before.appearance.language, &settings)?;
    let effective = apply_limits(&app, &before, &settings).await;
    Ok(SettingsOut {
        settings,
        issues: Vec::new(),
        effective,
    })
}

/// Back to the defaults of one category, or of everything.
#[tauri::command]
async fn settings_reset(
    app: AppHandle,
    store: State<'_, settings::SettingsStore>,
    category: Option<String>,
) -> Result<SettingsOut, String> {
    let before = store.snapshot();
    let settings = store.reset(category.as_deref())?;
    menu_follow_language(&app, before.appearance.language, &settings)?;
    let effective = apply_limits(&app, &before, &settings).await;
    Ok(SettingsOut {
        settings,
        issues: Vec::new(),
        effective,
    })
}

/// macOS smart quotes, smart dashes and text replacements off in the
/// webview: typed `"` became `“”` and `--` became `—` in cells, SQL and
/// filters. WebKit reads these keys from the app's defaults and falls back
/// to the system setting when they are missing; registered (lowest
/// priority), so the Edit > Substitutions menu can still turn them on.
/// Before the webview starts: WebKit reads them once.
#[cfg(target_os = "macos")]
fn disable_text_substitutions() {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};
    let keys = [
        "WebAutomaticQuoteSubstitutionEnabled",
        "WebAutomaticDashSubstitutionEnabled",
        "WebAutomaticTextReplacementEnabled",
        "WebAutomaticSpellingCorrectionEnabled",
    ]
    .map(NSString::from_str);
    let off = NSNumber::numberWithBool(false);
    let off: &AnyObject = off.as_ref();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let dict = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[off; 4]);
    // SAFETY: string keys and NSNumber values, the types the defaults expect.
    unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&dict) };
}

/// Starts the app. `context` is `tauri::generate_context!()` of the calling
/// binary: its configuration and its assets.
pub fn run(context: tauri::Context<tauri::Wry>, extensions: Extensions) {
    #[cfg(target_os = "macos")]
    disable_text_substitutions();
    let mut drivers = Drivers::builtin();
    for driver in extensions.drivers {
        drivers.register(driver);
    }
    let mut builder = tauri::Builder::default();
    for plugin in extensions.plugins {
        builder = builder.plugin_boxed(plugin);
    }
    // Size, position and maximized state of the window across launches, as
    // in DataGrip and DBeaver. Not in e2e (same config folder as the everyday
    // app) nor in M0 auto mode (the measurements need the fixed config size).
    let remember_window = cfg!(not(feature = "e2e")) && !auto_mode_flag();
    if remember_window {
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        );
    }
    builder
        // Export (dec. 11): native save dialog and clipboard, used
        // ONLY on the Rust side — no JS binding, no ACL capability.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        // Alpha updates (Windows): this one too only on the Rust side.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            if remember_window {
                maximize_on_first_launch(app)?;
            }
            // In auto mode the window must stay visible: WKWebView suspends
            // the rAF when occluded and the suspensions would pollute the measurement.
            if auto_mode_flag() {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_always_on_top(true);
                    let _ = w.set_focus();
                }
            }
            // §13.1: data directory derived from the identifier, never rewritten.
            #[cfg(not(feature = "e2e"))]
            let dir = app.path().app_data_dir()?;
            #[cfg(feature = "e2e")]
            let dir = e2e::data_dir()?;
            let db = storage::open(&dir).map_err(Box::<dyn std::error::Error>::from)?;
            app.manage(Db(Mutex::new(db)));
            // §16: `settings.json` in the config directory (the same folder
            // as the data on macOS and Windows, `~/.config` on Linux); in
            // e2e next to the test database.
            #[cfg(not(feature = "e2e"))]
            let config_dir = app.path().app_config_dir()?;
            #[cfg(feature = "e2e")]
            let config_dir = dir;
            let store = settings::SettingsStore::load(&config_dir);
            // The menu in the UI language from the first frame: the file
            // is Rust's, no round trip through the webview.
            #[cfg(target_os = "macos")]
            app.set_menu(build_menu(
                app.handle(),
                store.snapshot().appearance.language == settings::Language::It,
            )?)?;
            app.manage(store);
            watch_idle_transactions(app.handle().clone());
            #[cfg(feature = "e2e")]
            e2e::start(app.handle());
            Ok(())
        })
        .manage(AppState {
            started: Instant::now(),
            sessions: Mutex::new(HashMap::new()),
            drivers,
            profiles: Mutex::new(HashMap::new()),
            tunnels: Mutex::new(HashMap::new()),
            next_conn_id: AtomicU64::new(1),
            quit_confirmed: AtomicBool::new(false),
            unsaved_edits: AtomicBool::new(false),
            pending_host_keys: Mutex::new(HashMap::new()),
            db_filters: Mutex::new(HashMap::new()),
            reopen: Mutex::new(HashMap::new()),
            siblings: Mutex::new(HashMap::new()),
        })
        // Red button / Cmd+W: with uncommitted writes the close
        // stops and the frontend shows the dialog (§9.7.2). Cmd+Q does not go
        // through here: its gate is ExitRequested, at the bottom.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if !state.quit_confirmed.load(Ordering::Relaxed) && writes_pending(&state) {
                    api.prevent_close();
                    signal_quit_blocked(window.app_handle());
                }
            }
        })
        // "Quit" menu item (Cmd+Q): requests the exit, which goes through the
        // ExitRequested gate like the red button. "Settings…" (Cmd+,) opens
        // the window in the webview.
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "quit" => app.exit(0),
            "settings" => dom_event(app, "alamo:open-settings"),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            settings_get,
            settings_set,
            settings_reset,
            diagnostics_text,
            settings_reveal,
            engines,
            connect,
            connect_profile,
            test_profile,
            profiles_list,
            profile_save,
            profile_delete,
            profile_password_present,
            ssh_profiles_list,
            ssh_profile_save,
            ssh_profile_delete,
            ssh_secret_forget,
            ssh_host_key_forget,
            ssh_profile_test,
            ssh_profile_host_key_trust,
            connections_export,
            connections_import_open,
            pick_root_cert,
            inspect_root_cert,
            connections_import_apply,
            ssh_host_key_trust,
            autosave_get,
            autosave_set,
            autosave_delete,
            workspace_get,
            workspace_set,
            table_default_sort_get,
            table_default_sort_set,
            favorites_list,
            favorite_set,
            schema_list,
            schema_stats,
            schema_relations,
            schema_relations_all,
            schema_columns,
            schema_foreign_keys,
            autocomplete_catalog,
            terminate_connection,
            disconnect,
            connection_status,
            execute,
            txn_state,
            txn_log,
            set_manual_commit,
            txn_commit,
            txn_rollback,
            classify_sql,
            split_sql,
            server_databases,
            open_database,
            explain_estimate,
            export_result,
            export_script,
            copy_result_cells,
            table_data,
            table_data_more,
            table_data_sql,
            table_update_sql,
            table_update,
            table_update_sql_batch,
            table_update_batch,
            table_write_sql_batch,
            table_write_batch,
            table_refresh_rows,
            result_source,
            table_ddl,
            current_database,
            use_database,
            cancel,
            budget_resume,
            budget_more,
            ensure_connected,
            reconnect,
            budget_stop,
            fetch_window,
            sort_result,
            get_cell_full,
            release_result,
            bench_run_query,
            report_ready,
            report_frames,
            report_error,
            app_quit,
            set_unsaved_edits,
            window_start_drag,
            window_toggle_maximize,
            update_check,
            update_install,
            harness_mode,
            m1_smoke,
            smoke_done,
            #[cfg(feature = "e2e")]
            e2e::e2e_result,
            #[cfg(feature = "e2e")]
            e2e::e2e_next_pick
        ])
        .build({
            #[allow(unused_mut)]
            let mut context = context;
            #[cfg(feature = "e2e")]
            e2e::configure(context.config_mut());
            context
        })
        .expect("tauri startup failed")
        .run(|app, event| {
            // Cmd+Q (and last window closed): same gate as
            // CloseRequested. Confirmed via app_quit or without pending
            // writes it exits; otherwise it blocks and the dialog appears.
            match event {
                tauri::RunEvent::ExitRequested { api, .. } => {
                    let state = app.state::<AppState>();
                    if !state.quit_confirmed.load(Ordering::Relaxed) && writes_pending(&state) {
                        api.prevent_exit();
                        signal_quit_blocked(app);
                    }
                }
                // Quitting after a sleep leaves the server connections as a
                // Disconnect does: kept for the next launch.
                tauri::RunEvent::Exit => {
                    let state = app.state::<AppState>();
                    if let Some(db) = app.try_state::<Db>() {
                        let ids: Vec<u64> = locked(&state.profiles).keys().copied().collect();
                        for conn_id in ids {
                            state.remember_abandoned(&db, conn_id);
                        }
                    }
                }
                _ => {}
            }
        })
}
