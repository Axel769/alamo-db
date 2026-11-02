//! `cargo xtask test-matrix [filter]` — DB version matrix of §15.1.
//! `cargo xtask run` — build + sign + launch the app (macOS).
//! `cargo xtask bundle` — release build + distributable bundle (.app/.dmg).
//! `cargo xtask e2e` / `e2e-eval` — app with the remote control for tests (`e2e.rs`).
//!
//! For every engine/version in the matrix it starts a throwaway container
//! (testcontainers-rs, random port: does not collide with the development
//! containers `alamo-pg`/`alamo-mysql`/`alamo-mariadb`), waits until the
//! server accepts TCP connections and runs the existing integration suites
//! with the env vars they already expect. The filter is a substring match
//! on the id (`postgres:15`, `mysql:8.4`, ...).
//!
//! A reachable Docker daemon is required. Exits with code 1 if any suite
//! fails, but still tries every entry of the matrix.

mod e2e;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use testcontainers::core::{CmdWaitFor, ExecCommand};
use testcontainers::runners::SyncRunner;
use testcontainers::{Container, GenericImage, ImageExt};

#[derive(Clone, Copy, PartialEq)]
enum Engine {
    Postgres,
    MySql,
    MariaDb,
}

struct Target {
    image: &'static str,
    tag: &'static str,
    engine: Engine,
}

impl Target {
    fn id(&self) -> String {
        format!("{}:{}", self.image, self.tag)
    }
}

/// §15.1: PG 15 and 18, MySQL 8.0 and 8.4 LTS, MariaDB 10.11 and 11.8 LTS.
const MATRIX: &[Target] = &[
    Target {
        image: "postgres",
        tag: "15",
        engine: Engine::Postgres,
    },
    Target {
        image: "postgres",
        tag: "18",
        engine: Engine::Postgres,
    },
    Target {
        image: "mysql",
        tag: "8.0",
        engine: Engine::MySql,
    },
    Target {
        image: "mysql",
        tag: "8.4",
        engine: Engine::MySql,
    },
    Target {
        image: "mariadb",
        tag: "10.11",
        engine: Engine::MariaDb,
    },
    Target {
        image: "mariadb",
        tag: "11.8",
        engine: Engine::MariaDb,
    },
];

const PASSWORD: &str = "alamo";
const DATABASE: &str = "alamo_bench";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let filter = match args.split_first() {
        Some((cmd, rest)) if cmd == "test-matrix" => rest.first().cloned(),
        Some((cmd, _)) if cmd == "run" => {
            run_app();
            return;
        }
        Some((cmd, _)) if cmd == "bundle" => {
            bundle_app();
            return;
        }
        Some((cmd, _)) if cmd == "e2e" => {
            e2e::run(&repo_root());
            return;
        }
        Some((cmd, rest)) if cmd == "e2e-eval" => {
            e2e::eval(&repo_root(), rest.first().map(String::as_str));
            return;
        }
        _ => {
            eprintln!("usage: cargo xtask test-matrix [filter]");
            eprintln!("     filter: substring of the id, e.g. `mysql`, `mariadb:11.8`");
            eprintln!("     cargo xtask run — build + sign + launch the app");
            eprintln!("     cargo xtask bundle — release build + bundle (.app/.dmg on macOS)");
            eprintln!("     cargo xtask e2e — app with the remote control for end-to-end tests");
            eprintln!("     cargo xtask e2e-eval [file.js] — send JS to the open e2e app (or from stdin)");
            std::process::exit(2);
        }
    };

    let root = repo_root();
    let mut failures: Vec<String> = Vec::new();
    let mut ran = 0;

    for target in MATRIX {
        if let Some(f) = &filter {
            if !target.id().contains(f.as_str()) {
                continue;
            }
        }
        ran += 1;
        println!("\n=== {} ===", target.id());
        if let Err(e) = run_target(&root, target) {
            eprintln!("--- {} FAILED: {e}", target.id());
            failures.push(target.id());
        }
    }

    if ran == 0 {
        eprintln!("no matrix entry matches the filter");
        std::process::exit(2);
    }
    if !failures.is_empty() {
        eprintln!("\nfailed suites: {}", failures.join(", "));
        std::process::exit(1);
    }
    println!("\nmatrix complete: {ran} entries green");
}

/// `cargo xtask run`: build the app, sign it (macOS) and launch it.
///
/// Why the signature: Keychain ties "Always Allow" to the SIGNATURE of the
/// binary. An unsigned dev build changes identity at every compilation
/// → macOS asks for permission on every run. Signing with a stable
/// identity (`sign_identity`) makes the permission granted once survive
/// rebuilds. Without an available identity it launches anyway, unsigned.
fn run_app() {
    let root = repo_root();
    // Tauri embeds `app/dist` at compile time: without rebuilding the
    // frontend, `cargo build` packages the UI of the last `pnpm build` and
    // changes to `app/src` do not show up.
    let status = Command::new("pnpm")
        .arg("build")
        .current_dir(root.join("app"))
        .status()
        .expect("pnpm build (requires pnpm and `pnpm install` in app/)");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "alamo-app"])
        .current_dir(&root)
        .status()
        .expect("cargo build");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    let binary = root.join("target/debug/alamo-app");
    if cfg!(target_os = "macos") {
        let identity = sign_identity();
        // --force: replaces the signature of the previous build; the
        // requirement stays the one of the certificate, which is the point.
        let signed = Command::new("codesign")
            .args(["--force", "--sign", &identity])
            .arg(&binary)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !signed {
            eprintln!(
                "⚠️ signing failed (identity `{identity}`): launching unsigned — \
                 Keychain will ask for permission on every build"
            );
        }
    }
    let status = Command::new(&binary)
        .current_dir(&root)
        .status()
        .expect("launch alamo-app");
    std::process::exit(status.code().unwrap_or(1));
}

/// `cargo xtask bundle`: `pnpm tauri build` with the default signing
/// identity. Produces the bundle in `target/release/bundle/` (on macOS
/// `macos/Alamo DB.app` and `dmg/Alamo DB_<ver>_<arch>.dmg`).
///
/// The Tauri CLI reads `APPLE_SIGNING_IDENTITY`; without it, it would sign
/// ad-hoc and Keychain would go back to asking for permission on every
/// build. Same identity as `run` (`sign_identity`), or
/// `APPLE_SIGNING_IDENTITY` directly. The bundle is meant for
/// this Mac or for colleagues who accept the Gatekeeper warning: without
/// Developer ID + notarization macOS blocks an app downloaded from the internet.
fn bundle_app() {
    let root = repo_root();
    let mut cmd = Command::new("pnpm");
    cmd.args(["tauri", "build"]).current_dir(root.join("app"));
    if cfg!(target_os = "macos") && std::env::var_os("APPLE_SIGNING_IDENTITY").is_none() {
        cmd.env("APPLE_SIGNING_IDENTITY", sign_identity());
    }
    let status = cmd
        .status()
        .expect("pnpm tauri build (requires pnpm and `pnpm install` in app/)");
    std::process::exit(status.code().unwrap_or(1));
}

/// macOS signing identity, the same as `scripts/release-alpha.sh`:
/// `ALAMO_SIGN_IDENTITY`, otherwise `~/.tauri/alamo-sign-identity` (outside
/// the repo: the certificate name contains the email), otherwise
/// "Apple Development". What matters is the certificate's TEAM: Keychain
/// remembers "Always Allow" per team, so dev and alpha with the same team
/// share the permission. With several Apple Development certificates in
/// Keychain the generic name is ambiguous and codesign refuses.
fn sign_identity() -> String {
    if let Ok(id) = std::env::var("ALAMO_SIGN_IDENTITY") {
        return id;
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".tauri/alamo-sign-identity"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Apple Development".into())
}

fn repo_root() -> PathBuf {
    // xtask lives in <root>/xtask: the repo root is the parent of the manifest.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask without a parent directory")
        .to_path_buf()
}

fn run_target(root: &Path, target: &Target) -> Result<(), String> {
    let (env_password, env_database, port, ping) = match target.engine {
        Engine::Postgres => (
            "POSTGRES_PASSWORD",
            "POSTGRES_DB",
            5432u16,
            // -h forces TCP: the temporary init server listens only on the
            // unix socket, so we do not mistake the init for the real server.
            vec!["pg_isready", "-h", "127.0.0.1", "-U", "postgres"],
        ),
        Engine::MySql => (
            "MYSQL_ROOT_PASSWORD",
            "MYSQL_DATABASE",
            3306u16,
            vec!["mysqladmin", "ping", "-h127.0.0.1", "-uroot", "-palamo"],
        ),
        Engine::MariaDb => (
            "MARIADB_ROOT_PASSWORD",
            "MARIADB_DATABASE",
            3306u16,
            vec!["mariadb-admin", "ping", "-h127.0.0.1", "-uroot", "-palamo"],
        ),
    };

    println!("starting {} ...", target.id());
    let container = GenericImage::new(target.image, target.tag)
        .with_exposed_port(port.into())
        .with_env_var(env_password, PASSWORD)
        .with_env_var(env_database, DATABASE)
        .start()
        .map_err(|e| format!("container start: {e}"))?;

    wait_ready(&container, &ping)?;
    let host_port = container
        .get_host_port_ipv4(port)
        .map_err(|e| format!("host port: {e}"))?;

    let user = match target.engine {
        Engine::Postgres => "postgres",
        _ => "root",
    };
    let scheme = match target.engine {
        Engine::Postgres => "postgres",
        _ => "mysql",
    };
    let url = format!("{scheme}://{user}:{PASSWORD}@127.0.0.1:{host_port}/{DATABASE}");
    println!("ready at {url}");

    match target.engine {
        Engine::Postgres => {
            // The PG session tests read bench_narrow/bench_wide (§15):
            // alamo-bench seeds them, as in the integration-postgres job.
            cargo(
                root,
                &[
                    "run",
                    "-p",
                    "alamo-bench",
                    "--",
                    "prepare",
                    "--rows",
                    "20000",
                    "--force",
                ],
                &[("ALAMO_BENCH_URL", &url)],
            )?;
            // ALAMO_REQUIRE_TEST_DBS: in the matrix the silent skip is
            // forbidden — if the env wiring breaks, the listed variable is
            // missing and the suite turns RED instead of "green by skip".
            cargo(
                root,
                &["test", "-p", "alamo-session", "--test", "integration"],
                &[
                    ("ALAMO_TEST_URL", &url),
                    ("ALAMO_REQUIRE_TEST_DBS", "ALAMO_TEST_URL"),
                ],
            )?;
        }
        Engine::MySql => {
            cargo(
                root,
                &["test", "-p", "alamo-driver-mysql", "--test", "integration"],
                &[
                    ("ALAMO_MYSQL_TEST_URL", &url),
                    ("ALAMO_REQUIRE_TEST_DBS", "ALAMO_MYSQL_TEST_URL"),
                ],
            )?;
            cargo(
                root,
                &["test", "-p", "alamo-session", "--test", "integration"],
                &[
                    ("ALAMO_MYSQL_TEST_URL", &url),
                    ("ALAMO_REQUIRE_TEST_DBS", "ALAMO_MYSQL_TEST_URL"),
                ],
            )?;
        }
        Engine::MariaDb => {
            // The driver tells the two urls apart (on ALAMO_MYSQL_TEST_URL it
            // asserts `!is_mariadb`); the session speaks the "mysql dialect" for both.
            cargo(
                root,
                &["test", "-p", "alamo-driver-mysql", "--test", "integration"],
                &[
                    ("ALAMO_MARIADB_TEST_URL", &url),
                    ("ALAMO_REQUIRE_TEST_DBS", "ALAMO_MARIADB_TEST_URL"),
                ],
            )?;
            cargo(
                root,
                &["test", "-p", "alamo-session", "--test", "integration"],
                &[
                    ("ALAMO_MYSQL_TEST_URL", &url),
                    ("ALAMO_REQUIRE_TEST_DBS", "ALAMO_MYSQL_TEST_URL"),
                ],
            )?;
        }
    }
    Ok(())
}

/// Retries the ping inside the container until the server answers over TCP.
fn wait_ready(container: &Container<GenericImage>, ping: &[&str]) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let done = container
            .exec(
                ExecCommand::new(ping.iter().copied()).with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .ok()
            .and_then(|r| r.exit_code().ok().flatten())
            == Some(0);
        if done {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("server not ready within 120s (ping: {ping:?})"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn cargo(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<(), String> {
    println!("$ cargo {}", args.join(" "));
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let status = cmd.status().map_err(|e| format!("cargo launch: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` exited with {status}", args.join(" ")))
    }
}
