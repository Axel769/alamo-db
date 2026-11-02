//! Remote control for the end-to-end tests, only with the `e2e` feature
//! (never in the `cargo xtask run` build nor in release): the real app —
//! webview, UI and backend — is driven from outside without recording the
//! screen or using macOS accessibility.
//!
//! A minimal HTTP server on `127.0.0.1` receives `POST /eval` with a piece of
//! JavaScript (body of an async function), runs it in the window and replies
//! with the returned value, serialized as JSON. The JS sends the result back
//! with the `e2e_result` command. The header
//! `x-alamo-e2e: <ALAMO_E2E_TOKEN>` is required: without a token the server
//! does not start.
//!
//! Isolation from the everyday app: the data folder comes from
//! `ALAMO_E2E_DATA_DIR` (required), the Keychain uses a separate service
//! (`keychain_service`) and the webview its own storage (`configure`), so
//! the tests neither see nor touch real profiles, passwords and history.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};

/// Past this time the JS reply is considered lost (page reloaded, exception
/// outside the wrapper).
const EVAL_TIMEOUT: Duration = Duration::from_secs(60);

static PENDING: OnceLock<Mutex<HashMap<u64, mpsc::Sender<String>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn pending() -> &'static Mutex<HashMap<u64, mpsc::Sender<String>>> {
    PENDING.get_or_init(Default::default)
}

/// Test data folder: required, so the real one is never opened.
pub fn data_dir() -> Result<std::path::PathBuf, String> {
    std::env::var_os("ALAMO_E2E_DATA_DIR")
        .map(Into::into)
        .ok_or_else(|| "e2e build: set ALAMO_E2E_DATA_DIR (test data folder)".into())
}

/// Keychain service separate from the real app's one.
pub fn keychain_service(identifier: &str) -> String {
    format!("{identifier}.e2e")
}

/// The webview is never suspended, not even with the window covered or in
/// another Space: the injected JS must run even while the user works
/// elsewhere (macOS 14+; before that there is no way and `set_always_on_top`
/// remains).
pub fn configure(config: &mut tauri::Config) {
    for w in &mut config.app.windows {
        w.background_throttling = Some(tauri::utils::config::BackgroundThrottlingPolicy::Disabled);
        // Webview storage (localStorage: query history, panels, recent
        // databases) in memory, fresh at every start: the everyday app has
        // the same binary and therefore the same storage, and without this
        // the tests wrote into its history. (`data_store_identifier` from
        // the Tauri 2.11 config does not apply it.)
        w.incognito = true;
    } // The test JS arrives as a string and is compiled with `AsyncFunction`
      // inside a try: a syntax error comes back as a readable error instead
      // of silencing the page. `unsafe-eval` is needed, only here.
    if let Some(tauri::utils::config::Csp::Policy(p)) = &mut config.app.security.csp {
        *p = p.replacen("script-src 'self'", "script-src 'self' 'unsafe-eval'", 1);
    }
}

/// Result of an `/eval`, sent by the injected JS.
#[tauri::command]
pub fn e2e_result(id: u64, result: String) {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        let _ = tx.send(result);
    }
}

/// Path that the next native file or folder dialog returns without opening:
/// the native dialog cannot be driven from here (no accessibility).
static NEXT_PICK: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// The test chooses the path of the next native dialog (import, script export).
#[tauri::command]
pub fn e2e_next_pick(path: String) {
    *NEXT_PICK.lock().unwrap() = Some(path.into());
}

/// Taken once: the following dialogs open normally.
pub fn take_pick() -> Option<std::path::PathBuf> {
    NEXT_PICK.lock().unwrap().take()
}

/// Starts the server if the token is there. Port from `ALAMO_E2E_PORT` (0 =
/// chosen by the system); the real one is printed on stdout for whoever
/// launched the app.
pub fn start(app: &AppHandle) {
    let Ok(token) = std::env::var("ALAMO_E2E_TOKEN") else {
        eprintln!("e2e: ALAMO_E2E_TOKEN not set, remote control off");
        return;
    };
    let port = std::env::var("ALAMO_E2E_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0u16);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("e2e: bind failed: {e}");
            return;
        }
    };
    // Covered window = webview suspended by macOS: the injected JS would no
    // longer run. Like the M0 harness, it is kept always in front.
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(true);
    }
    if let Ok(addr) = listener.local_addr() {
        println!("ALAMO_E2E_PORT={}", addr.port());
        let _ = std::io::stdout().flush();
    }
    let app = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (app, token) = (app.clone(), token.clone());
            std::thread::spawn(move || {
                if let Err(e) = serve(stream, &app, &token) {
                    eprintln!("e2e: request failed: {e}");
                }
            });
        }
    });
}

fn serve(stream: TcpStream, app: &AppHandle, token: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut length = 0usize;
    let mut authorized = false;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            match name.as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "x-alamo-e2e" => authorized = value == token,
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let (status, out) = if !authorized {
        (
            "403 Forbidden",
            r#"{"ok":false,"error":"missing or wrong token"}"#.into(),
        )
    } else if !request_line.starts_with("POST /eval ") {
        (
            "404 Not Found",
            r#"{"ok":false,"error":"only POST /eval"}"#.into(),
        )
    } else {
        match eval(app, &String::from_utf8_lossy(&body)) {
            Ok(r) => ("200 OK", r),
            Err(e) => (
                "504 Gateway Timeout",
                serde_json::json!({ "ok": false, "error": e }).to_string(),
            ),
        }
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
        out.len()
    )
}

/// Runs `body` as the body of an async function in the window and waits for
/// the value. Exceptions, syntax errors included, come back as
/// `{"ok":false,"error":...}`.
fn eval(app: &AppHandle, body: &str) -> Result<String, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("main window missing")?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let body = serde_json::to_string(body).map_err(|e| e.to_string())?;
    pending().lock().unwrap().insert(id, tx);
    let script = format!(
        "(async () => {{
            let r;
            try {{
                const AsyncFunction = (async () => {{}}).constructor;
                const value = await new AsyncFunction({body})();
                r = {{ ok: true, value: value === undefined ? null : value }};
            }} catch (e) {{
                r = {{ ok: false, error: String(e) + (e && e.stack ? '\\n' + e.stack : '') }};
            }}
            let json;
            try {{ json = JSON.stringify(r); }}
            catch (e) {{ json = JSON.stringify({{ ok: false, error: 'result not serializable: ' + e }}); }}
            window.__TAURI_INTERNALS__.invoke('e2e_result', {{ id: {id}, result: json }});
        }})();"
    );
    if let Err(e) = window.eval(&script) {
        pending().lock().unwrap().remove(&id);
        return Err(format!("eval failed: {e}"));
    }
    rx.recv_timeout(EVAL_TIMEOUT).map_err(|_| {
        pending().lock().unwrap().remove(&id);
        "no reply from the JS within 60 s".to_string()
    })
}
