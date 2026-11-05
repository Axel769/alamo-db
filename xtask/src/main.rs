//! `cargo xtask test-matrix [filter]` — DB version matrix of §15.1.
//! `cargo xtask run` — build + sign + launch the app (macOS).
//! `cargo xtask bundle` — release build + distributable bundle (.app/.dmg).
//! `cargo xtask e2e` / `e2e-eval` — app with the remote control for tests (`e2e.rs`).
//! `cargo xtask tls-lab` — the TLS targets of the matrix kept running, with
//! their generated CA, for the e2e session and manual checks (`tls.rs`).
//! `cargo xtask gen-settings` — regenerate `app/src/settingsSchema.ts` (§16).
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
//!
//! The `+tls` targets run the same images with a server certificate signed
//! by a CA generated on the spot (`tls.rs`), and only the TLS suites: they
//! are the only automatic coverage of `verify-ca`/`verify-full` succeeding.

mod e2e;
mod tls;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use testcontainers::core::{CmdWaitFor, ExecCommand};
use testcontainers::runners::SyncRunner;
use testcontainers::{
    Container, ContainerRequest, CopyDataSource, CopyTargetOptions, GenericImage, ImageExt,
};

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
    /// Server certificate from the lab CA (`tls.rs`): only the TLS suites
    /// run, with `ALAMO_TEST_TLS_CA` pointing at the CA.
    tls: bool,
}

impl Target {
    fn id(&self) -> String {
        let suffix = if self.tls { "+tls" } else { "" };
        format!("{}:{}{suffix}", self.image, self.tag)
    }
}

const fn plain(image: &'static str, tag: &'static str, engine: Engine) -> Target {
    Target {
        image,
        tag,
        engine,
        tls: false,
    }
}

const fn with_tls(image: &'static str, tag: &'static str, engine: Engine) -> Target {
    Target {
        image,
        tag,
        engine,
        tls: true,
    }
}

/// §15.1: PG 15 and 18, MySQL 8.0 and 8.4 LTS, MariaDB 10.11 and 11.8 LTS;
/// the newest of each engine again with the lab certificate (`+tls`, also
/// the `tls-lab` set).
const MATRIX: &[Target] = &[
    plain("postgres", "15", Engine::Postgres),
    plain("postgres", "18", Engine::Postgres),
    plain("mysql", "8.0", Engine::MySql),
    plain("mysql", "8.4", Engine::MySql),
    plain("mariadb", "10.11", Engine::MariaDb),
    plain("mariadb", "11.8", Engine::MariaDb),
    with_tls("postgres", "18", Engine::Postgres),
    with_tls("mysql", "8.4", Engine::MySql),
    with_tls("mariadb", "11.8", Engine::MariaDb),
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
        Some((cmd, _)) if cmd == "tls-lab" => {
            tls_lab();
            return;
        }
        Some((cmd, _)) if cmd == "gen-settings" => {
            gen_settings();
            return;
        }
        _ => {
            eprintln!("usage: cargo xtask test-matrix [filter]");
            eprintln!("     filter: substring of the id, e.g. `mysql`, `mariadb:11.8`, `tls`");
            eprintln!("     cargo xtask run — build + sign + launch the app");
            eprintln!("     cargo xtask bundle — release build + bundle (.app/.dmg on macOS)");
            eprintln!("     cargo xtask e2e — app with the remote control for end-to-end tests");
            eprintln!(
                "     cargo xtask e2e-eval [file.js] — send JS to the open e2e app (or from stdin)"
            );
            eprintln!("     cargo xtask tls-lab — PG/MySQL/MariaDB with the lab CA on fixed ports, until Enter or Ctrl-C");
            eprintln!("     cargo xtask gen-settings — regenerate app/src/settingsSchema.ts from settings.rs");
            std::process::exit(2);
        }
    };

    let root = repo_root();
    let mut failures: Vec<String> = Vec::new();
    let mut ran = 0;
    let selected: Vec<&Target> = MATRIX
        .iter()
        .filter(|t| filter.as_ref().is_none_or(|f| t.id().contains(f.as_str())))
        .collect();
    // One CA for the whole run, generated only if a TLS target is selected,
    // in a folder of its own: the lab's CA must stay the one its running
    // servers were started with.
    let lab = selected
        .iter()
        .any(|t| t.tls)
        .then(|| tls::generate(&root.join("target/tls/matrix")).unwrap_or_else(|e| fail(&e)));

    for target in selected {
        ran += 1;
        println!("\n=== {} ===", target.id());
        if let Err(e) = run_target(&root, target, lab.as_ref()) {
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

fn run_target(root: &Path, target: &Target, lab: Option<&tls::TlsFiles>) -> Result<(), String> {
    println!("starting {} ...", target.id());
    let (_container, url) = start(target, lab, None)?;
    println!("ready at {url}");

    if let Some(lab) = lab.filter(|_| target.tls) {
        // TLS targets: only the TLS suites (`tls_*`), with the CA. The rest
        // of the matrix already covers the engine; here what matters is a
        // server whose certificate the profile CA verifies.
        let ca = lab.ca_path();
        let ca = ca.to_str().ok_or("CA path is not UTF-8")?;
        let session_var = match target.engine {
            Engine::Postgres => "ALAMO_TEST_URL",
            _ => "ALAMO_MYSQL_TEST_URL",
        };
        let driver_suite = match target.engine {
            Engine::Postgres => None,
            Engine::MySql => Some(("alamo-driver-mysql", "ALAMO_MYSQL_TEST_URL")),
            Engine::MariaDb => Some(("alamo-driver-mysql", "ALAMO_MARIADB_TEST_URL")),
        };
        for (package, var) in driver_suite
            .into_iter()
            .chain([("alamo-session", session_var)])
        {
            let require = format!("{var},ALAMO_TEST_TLS_CA");
            cargo(
                root,
                &["test", "-p", package, "--test", "integration", "--", "tls"],
                &[
                    (var, &url),
                    ("ALAMO_TEST_TLS_CA", ca),
                    ("ALAMO_REQUIRE_TEST_DBS", &require),
                ],
            )?;
        }
        return Ok(());
    }

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

/// Starts the target's container and waits for the server. `host_port`
/// fixes the host side (the lab); `None` = random (the matrix, which must
/// not collide with the development containers). Returns the container,
/// alive until dropped, and the url of the server.
fn start(
    target: &Target,
    lab: Option<&tls::TlsFiles>,
    host_port: Option<u16>,
) -> Result<(Container<GenericImage>, String), String> {
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

    let mut request: ContainerRequest<GenericImage> = GenericImage::new(target.image, target.tag)
        .with_exposed_port(port.into())
        .with_env_var(env_password, PASSWORD)
        .with_env_var(env_database, DATABASE);
    if let Some(host_port) = host_port {
        request = request.with_mapped_port(host_port, port.into());
    }
    if target.tls {
        let lab = lab.ok_or("TLS target without the lab files")?;
        request = with_lab_certificate(request, target.engine, lab);
    }
    let container = request
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
    Ok((
        container,
        format!("{scheme}://{user}:{PASSWORD}@127.0.0.1:{host_port}/{DATABASE}"),
    ))
}

/// The lab certificate copied into the image and the server told to use
/// it. TLS is offered, not required: the plain suites (`TlsOptions::plain`)
/// must keep working against these servers too.
fn with_lab_certificate(
    request: ContainerRequest<GenericImage>,
    engine: Engine,
    lab: &tls::TlsFiles,
) -> ContainerRequest<GenericImage> {
    let request = request
        .with_copy_to(
            CopyTargetOptions::new("/tls/ca.pem").with_mode(0o644),
            CopyDataSource::Data(lab.ca_pem.clone().into_bytes()),
        )
        .with_copy_to(
            CopyTargetOptions::new("/tls/server.pem").with_mode(0o644),
            CopyDataSource::Data(lab.server_pem.clone().into_bytes()),
        )
        .with_copy_to(
            CopyTargetOptions::new("/tls/server-key.pem").with_mode(0o644),
            CopyDataSource::Data(lab.key_pem.clone().into_bytes()),
        );
    match engine {
        // PG refuses a key it does not own with mode > 0600: copy and chown
        // as root, then hand over to the image's entrypoint as usual.
        Engine::Postgres => request.with_cmd([
            "bash",
            "-c",
            "cp /tls/server.pem /tls/server-key.pem /var/lib/postgresql/ \
             && chown postgres:postgres /var/lib/postgresql/server.pem /var/lib/postgresql/server-key.pem \
             && chmod 600 /var/lib/postgresql/server-key.pem \
             && exec docker-entrypoint.sh postgres -c ssl=on \
                -c ssl_cert_file=/var/lib/postgresql/server.pem \
                -c ssl_key_file=/var/lib/postgresql/server-key.pem",
        ]),
        Engine::MySql => request.with_cmd([
            "mysqld",
            "--ssl-ca=/tls/ca.pem",
            "--ssl-cert=/tls/server.pem",
            "--ssl-key=/tls/server-key.pem",
        ]),
        Engine::MariaDb => request.with_cmd([
            "mariadbd",
            "--ssl-ca=/tls/ca.pem",
            "--ssl-cert=/tls/server.pem",
            "--ssl-key=/tls/server-key.pem",
        ]),
    }
}

/// `cargo xtask gen-settings`: rewrites `app/src/settingsSchema.ts` from
/// the `settings!` block of the app crate (§16). The generator is a test
/// of that crate — this workspace cannot link it — switched to writing by
/// `ALAMO_GEN_SETTINGS`; without the variable the same test checks that
/// the file is current.
fn gen_settings() {
    let status = Command::new("cargo")
        .args(["test", "-p", "alamo-app", "settings_schema_ts_is_current"])
        .env("ALAMO_GEN_SETTINGS", "1")
        .current_dir(repo_root())
        .status()
        .expect("cargo test");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("app/src/settingsSchema.ts regenerated");
}

/// `cargo xtask tls-lab`: the three `+tls` targets on fixed ports, for the
/// e2e app and manual checks with `verify-ca`/`verify-full`. Stops at Enter
/// or Ctrl-C (the containers are then reaped by testcontainers' ryuk).
fn tls_lab() {
    const PORTS: [(Engine, u16); 3] = [
        (Engine::Postgres, 5433),
        (Engine::MySql, 3309),
        (Engine::MariaDb, 3310),
    ];
    let root = repo_root();
    let lab = tls::generate(&root.join("target/tls/lab")).unwrap_or_else(|e| fail(&e));
    let mut containers = Vec::new();
    for (engine, port) in PORTS {
        let target = MATRIX
            .iter()
            .find(|t| t.tls && t.engine == engine)
            .expect("matrix without a TLS target for the engine");
        println!("starting {} on 127.0.0.1:{port} ...", target.id());
        let (container, url) = start(target, Some(&lab), Some(port)).unwrap_or_else(|e| fail(&e));
        println!("ready at {url}");
        containers.push(container);
    }
    println!("CA: {}", lab.ca_path().display());
    println!(
        "profiles: host 127.0.0.1, user postgres/root, password {PASSWORD}, database {DATABASE}"
    );
    println!("press Enter (or Ctrl-C) to stop the lab");
    let mut line = String::new();
    // Closed stdin (started in the background): stay up until killed.
    if matches!(std::io::stdin().read_line(&mut line), Ok(0)) {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    drop(containers);
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
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
