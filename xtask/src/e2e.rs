//! `cargo xtask e2e` — build the app with the `e2e` feature and launch it with
//! the remote control on (see `app/src-tauri/src/e2e.rs`): fresh data folder
//! in `target/e2e/data`, random token, port chosen by the system.
//! Port and token end up in `target/e2e/session` while the app is open.
//!
//! `cargo xtask e2e-eval [file.js]` — sends a piece of JS to the open app
//! (from the file or from stdin, body of an async function: `return` for the
//! value) and prints the JSON response. Exits with 1 if the JS threw.
//! Before the JS comes `PRELUDE`, with the helpers for clicking and typing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Helpers available in every `e2e-eval`. `type` uses the native setter:
/// React ignores a `value` assigned by hand without the `input` event.
const PRELUDE: &str = r#"
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const visible = (el) => !!el && el.getClientRects().length > 0;
const byText = (text, sel = "button, [role=button], [role=tab], [role=treeitem], a, li, span, div") =>
  $$(sel).filter(visible).find((e) => e.textContent.trim() === text);
const byTitle = (title) => $$(`[title="${title}"]`).filter(visible)[0];
async function waitFor(fn, ms = 10000) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > ms) throw new Error("waitFor: timeout on " + fn);
    await sleep(50);
  }
}
function type(el, value) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
    : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype
    : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
}
function click(el) {
  if (!el) throw new Error("click: element missing");
  el.click();
}
const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
const text = (el = document.body) => el.innerText;
"#;

fn e2e_dir(root: &Path) -> PathBuf {
    root.join("target/e2e")
}

pub fn run(root: &Path) {
    for (cmd, args, dir) in [
        ("pnpm", vec!["build"], root.join("app")),
        (
            "cargo",
            vec!["build", "-p", "alamo-app", "--features", "e2e"],
            root.to_path_buf(),
        ),
    ] {
        let status = Command::new(cmd)
            .args(&args)
            .current_dir(&dir)
            .status()
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    let dir = e2e_dir(root);
    let data = dir.join("data");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("e2e data folder");
    let token = random_token();
    let mut child = Command::new(root.join("target/debug/alamo-app"))
        .current_dir(root)
        .env("ALAMO_E2E_TOKEN", &token)
        .env("ALAMO_E2E_DATA_DIR", &data)
        .env("ALAMO_E2E_PORT", "0")
        .stdout(Stdio::piped())
        .spawn()
        .expect("launch alamo-app");
    let session = dir.join("session");
    let stdout = child.stdout.take().expect("stdout");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(port) = line.strip_prefix("ALAMO_E2E_PORT=") {
            std::fs::write(&session, format!("{port}\n{token}\n")).expect("session file");
            println!(
                "e2e ready: port {port} (session in {})",
                session.display()
            );
        } else {
            println!("{line}");
        }
    }
    let status = child.wait().expect("wait for alamo-app");
    let _ = std::fs::remove_file(&session);
    std::process::exit(status.code().unwrap_or(1));
}

pub fn eval(root: &Path, file: Option<&str>) {
    let session = std::fs::read_to_string(e2e_dir(root).join("session"))
        .expect("no e2e app open: run `cargo xtask e2e` first");
    let mut lines = session.lines();
    let (Some(port), Some(token)) = (lines.next(), lines.next()) else {
        panic!("malformed e2e session file");
    };
    let js = match file {
        Some(f) => std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{f}: {e}")),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).expect("stdin");
            s
        }
    };
    let body = format!("{PRELUDE}\n{js}");
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>().expect("port")))
        .expect("e2e app not reachable");
    write!(
        stream,
        "POST /eval HTTP/1.1\r\nhost: 127.0.0.1\r\nx-alamo-e2e: {token}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("send");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    let out = response
        .split_once("\r\n\r\n")
        .map_or(response.as_str(), |(_, b)| b);
    println!("{out}");
    if !out.starts_with(r#"{"ok":true"#) {
        std::process::exit(1);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("/dev/urandom");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
