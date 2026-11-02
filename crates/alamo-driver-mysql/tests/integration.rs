//! Integration tests against real MySQL/MariaDB (§15.1).
//!
//! They need `ALAMO_MYSQL_TEST_URL` (MySQL 8.x) and, optionally,
//! `ALAMO_MARIADB_TEST_URL`; without them they are skipped — so `cargo test`
//! stays green in CI without services.
//!
//! ```sh
//! ALAMO_MYSQL_TEST_URL='mysql://root:alamo@localhost:3307/alamo_bench' \
//! ALAMO_MARIADB_TEST_URL='mysql://root:alamo@localhost:3308/alamo_bench' \
//!   cargo test -p alamo-driver-mysql --test integration
//! ```

use std::time::Duration;

use alamo_core::error::ErrorCategory;
use alamo_driver_mysql::MySession;

const MYSQL_VAR: &str = "ALAMO_MYSQL_TEST_URL";
const MARIADB_VAR: &str = "ALAMO_MARIADB_TEST_URL";

fn mysql_url() -> Option<String> {
    std::env::var(MYSQL_VAR).ok()
}

fn mariadb_url() -> Option<String> {
    std::env::var(MARIADB_VAR).ok()
}

/// Noisy skip or hard gate (same deal as the session suite):
/// if `ALAMO_REQUIRE_TEST_DBS` lists the variable, its absence is a
/// FAILURE, not a skip — the matrix always sets it, per engine. The
/// skip remains for the other engine (here MySQL and MariaDB live in the
/// same suite and each matrix run exercises only one) and for the
/// local `cargo test` without services.
fn skip_or_fail(var: &str) {
    let required = std::env::var("ALAMO_REQUIRE_TEST_DBS").unwrap_or_default();
    if required.split(',').any(|v| v.trim() == var) {
        panic!("{var} not set but required (ALAMO_REQUIRE_TEST_DBS={required})");
    }
    eprintln!("skipped: {var} not set");
}

macro_rules! require_url {
    ($f:expr, $var:expr) => {
        match $f {
            Some(u) => u,
            None => {
                skip_or_fail($var);
                return;
            }
        }
    };
}

#[tokio::test]
async fn connects_to_mysql_and_is_not_flagged_mariadb() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let s = MySession::connect_plain(&url)
        .await
        .expect("MySQL connection");
    assert!(!s.is_mariadb, "MySQL flagged as MariaDB");
    assert!(s.connection_id > 0);
}

#[tokio::test]
async fn connects_without_default_database() {
    // Profile without a database (§9.1): the URL has no path and the connection
    // must succeed anyway — the schema is chosen later with USE.
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let base = url
        .rfind('/')
        .filter(|i| *i > "mysql://".len())
        .map(|i| url[..i].to_string())
        .unwrap_or(url);
    let s = MySession::connect_plain(&base)
        .await
        .expect("connection without database");
    assert!(s.connection_id > 0);
}

/// A reply with the wrong shape is an error, not a panic: with
/// panic=abort the panic closed the app (alpha.3 crash, connection
/// out of phase). The phase shift is created by abandoning a ping halfway.
#[tokio::test]
async fn out_of_phase_reply_is_an_error_not_a_panic() {
    use futures_util::FutureExt;
    use mysql_async::prelude::Queryable;
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    // A single poll: the ping starts and its reply is left unread.
    assert!(
        s.conn.ping().now_or_never().is_none(),
        "ping completed on the first poll"
    );
    assert!(s.in_sync().is_err(), "interrupted command not reported");
    // Reads the ping's OK: the two columns are left behind.
    s.conn
        .query_drop("SELECT 1 AS a, 2 AS b")
        .await
        .expect("query");
    let e = alamo_driver_mysql::current_database(&mut s.conn)
        .await
        .expect_err("two-column row read as the database name");
    assert!(e.to_string().contains("unexpected reply"), "{e}");
}

#[tokio::test]
async fn connects_to_mariadb_and_flag_is_set() {
    let url = require_url!(mariadb_url(), MARIADB_VAR);
    let s = MySession::connect_plain(&url)
        .await
        .expect("MariaDB connection");
    assert!(s.is_mariadb, "MariaDB not recognized");
}

#[tokio::test]
async fn kill_query_interrupts_and_classifies_as_cancelled() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");

    // Long SLEEP on the main connection; KILL QUERY from the second one.
    let killer = MySession::connect_plain(&url)
        .await
        .expect("second connection");
    let target = s.connection_id;
    let kill = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        use mysql_async::prelude::Queryable;
        let mut k = killer;
        k.conn
            .query_drop(format!("KILL QUERY {target}"))
            .await
            .expect("KILL QUERY");
    });

    use mysql_async::prelude::Queryable;
    let res: Result<Option<i64>, _> = s.conn.query_first("SELECT SLEEP(30)").await;
    kill.await.expect("task kill");
    match res {
        // MySQL may also answer SLEEP=1 (interrupted without error):
        // both forms are a successful cancellation.
        Ok(v) => assert_eq!(v, Some(1), "SLEEP not interrupted"),
        Err(e) => {
            let db = alamo_driver_mysql::MyError::from(e).into_db_error();
            assert_eq!(db.category, ErrorCategory::Cancelled, "{db:?}");
        }
    }
}

#[tokio::test]
async fn auth_failure_is_classified_authentication() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let wrong = url.replacen(":alamo@", ":wrong@", 1);
    if wrong == url {
        eprintln!("skipped: URL without the expected password");
        return;
    }
    let err = MySession::connect_plain(&wrong)
        .await
        .err()
        .expect("connection should have failed");
    let db = err.into_db_error();
    assert_eq!(db.category, ErrorCategory::Authentication, "{db:?}");
}

#[tokio::test]
async fn user_connection_limit_has_hint() {
    // Second connection over the user's limit: it is the error you see
    // opening several profiles on the same server, and it must come with
    // the hint, not just the server message. Two forms: account limit
    // (1226) and global limit `@@max_user_connections` (1203).
    use mysql_async::prelude::Queryable;
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let limited = match url.split_once("://").and_then(|(_, r)| r.split_once('@')) {
        Some((_, rest)) => format!("mysql://alamo_limit:alamo@{rest}"),
        None => {
            eprintln!("skipped: URL without credentials");
            return;
        }
    };
    let mut admin = mysql_async::Conn::from_url(url.as_str())
        .await
        .expect("admin connection");
    for (errno, with, global) in [(1226u16, "WITH MAX_USER_CONNECTIONS 1", 0), (1203, "", 1)] {
        admin
            .query_drop(format!(
                "DROP USER IF EXISTS 'alamo_limit'@'%';
                 CREATE USER 'alamo_limit'@'%' IDENTIFIED BY 'alamo' {with};
                 GRANT SELECT ON alamo_bench.* TO 'alamo_limit'@'%';
                 SET GLOBAL max_user_connections = {global};"
            ))
            .await
            .expect("user with limit");
        let first = MySession::connect_plain(&limited).await;
        let second = MySession::connect_plain(&limited).await.err();
        drop(first);
        admin
            .query_drop(
                "SET GLOBAL max_user_connections = 0;
                 DROP USER IF EXISTS 'alamo_limit'@'%';",
            )
            .await
            .expect("cleanup");
        let db = second
            .expect("second connection should have failed")
            .into_db_error();
        assert!(
            matches!(db.code, alamo_core::error::ErrorCode::MySql { errno: e, .. } if e == errno),
            "{db:?}"
        );
        assert!(
            db.hint
                .as_deref()
                .is_some_and(|h| h.contains("max_user_connections")),
            "{db:?}"
        );
    }
}

#[tokio::test]
async fn connection_budget_reads_limit_and_own_connections() {
    // User with 3 connections and two already open: the budget must see them
    // without privileges (no PROCESS, no mysql.user). The `@` in the name
    // checks that the name is cut at the last `@` of CURRENT_USER().
    // The global limit is not touched: MariaDB started with
    // `--max-user-connections=0` rejects the SET GLOBAL.
    use alamo_core::schema_info::ConnectionBudget;
    use alamo_driver_mysql::connection_budget;
    use mysql_async::prelude::Queryable;
    let urls: Vec<String> = [mysql_url(), mariadb_url()].into_iter().flatten().collect();
    if urls.is_empty() {
        skip_or_fail(MYSQL_VAR);
        return;
    }
    for url in urls {
        let Some(server) = url
            .split_once("://")
            .and_then(|(_, r)| r.split_once('@'))
            .and_then(|(_, rest)| rest.split('/').next())
        else {
            eprintln!("skipped: URL without credentials");
            return;
        };
        let limited = format!("mysql://alamo%40budget:alamo@{server}");
        let mut admin = MySession::connect_plain(&url)
            .await
            .expect("admin connection");
        assert_eq!(
            connection_budget(&mut admin.conn)
                .await
                .expect("admin budget"),
            None,
            "root without limit: {url}"
        );
        admin
            .conn
            .query_drop(
                "DROP USER IF EXISTS 'alamo@budget'@'%';
                 CREATE USER 'alamo@budget'@'%' IDENTIFIED BY 'alamo'
                     WITH MAX_USER_CONNECTIONS 3;",
            )
            .await
            .expect("user with limit");
        let first = MySession::connect_plain(&limited).await;
        let second = MySession::connect_plain(&limited).await;
        let budget = match first {
            Ok(mut s) => connection_budget(&mut s.conn)
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        drop(second);
        admin
            .conn
            .query_drop("DROP USER IF EXISTS 'alamo@budget'@'%'")
            .await
            .expect("cleanup");
        assert_eq!(
            budget.expect("limited user budget"),
            Some(ConnectionBudget {
                limit: 3,
                in_use: 2
            }),
            "{url}"
        );
    }
}

// ------------------------------------------------------------------ streaming

use alamo_core::result_set::{ResultSetBuilder, ResultSetState};
use alamo_core::stream::{FetchOptions, StreamEvent};
use alamo_core::value::ValueRef;
use alamo_driver_mysql::{execute_streaming, MyDialect};
use std::sync::{Arc, Mutex};

/// Runs and collects the chunks into a ResultSet, as the session will.
async fn collect(
    s: &mut MySession,
    sql: &str,
    opts: FetchOptions,
) -> (
    Option<alamo_core::result_set::ResultSet>,
    alamo_core::stream::StreamOutcome,
) {
    let (_tx, mut rx) = tokio::sync::mpsc::channel(4);
    let builder: Arc<Mutex<Option<ResultSetBuilder>>> = Arc::default();
    let b = Arc::clone(&builder);
    let dialect = MyDialect {
        mariadb: s.is_mariadb,
        ..MyDialect::default()
    };
    let outcome = execute_streaming(
        &mut s.conn,
        sql,
        &dialect,
        &opts,
        &mut rx,
        async {},
        move |ev| {
            let mut g = b.lock().unwrap();
            match ev {
                StreamEvent::Schema { schema, .. } => *g = Some(ResultSetBuilder::new(schema)),
                StreamEvent::Chunk { chunk, stats, .. } => g
                    .as_mut()
                    .expect("schema before the chunks")
                    .push_chunk(chunk, stats),
                StreamEvent::BudgetReached { .. } => {}
            }
        },
    )
    .await
    .expect("execute_streaming");
    let rs = builder
        .lock()
        .unwrap()
        .take()
        .map(|b| b.finish(outcome.state));
    (rs, outcome)
}

#[tokio::test]
async fn select_decodes_typed_columns() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    let (rs, outcome) = collect(
        &mut s,
        "SELECT CAST(7 AS SIGNED) AS n,
                CAST(18446744073709551615 AS UNSIGNED) AS u,
                CAST(1.5 AS DOUBLE) AS f,
                CAST('12345.67' AS DECIMAL(10,2)) AS dec_txt,
                'hello' AS t,
                CAST('2024-06-15' AS DATE) AS d,
                CAST('2024-06-15 12:30:45.250000' AS DATETIME(6)) AS ts,
                NULL AS nothing",
        FetchOptions::default(),
    )
    .await;
    assert_eq!(outcome.state, ResultSetState::Complete);
    let rs = rs.expect("result set");
    assert_eq!(rs.rows(), 1);
    assert_eq!(rs.value_at(0, 0), Some(ValueRef::I64(7)));
    assert_eq!(rs.value_at(0, 1), Some(ValueRef::U64(18446744073709551615)));
    assert_eq!(rs.value_at(0, 2), Some(ValueRef::F64(1.5)));
    assert_eq!(rs.value_at(0, 3), Some(ValueRef::Utf8("12345.67")));
    assert_eq!(rs.value_at(0, 4), Some(ValueRef::Utf8("hello")));
    assert_eq!(
        rs.value_at(0, 5).unwrap().to_canonical_string().as_deref(),
        Some("2024-06-15")
    );
    assert_eq!(
        rs.value_at(0, 6).unwrap().to_canonical_string().as_deref(),
        Some("2024-06-15 12:30:45.250000")
    );
    assert_eq!(rs.value_at(0, 7), Some(ValueRef::Null));
    // decimal: right-aligned text (§4.5)
    assert!(rs.schema()[3].numeric_text);
}

#[tokio::test]
async fn dml_reports_affected_without_result_set() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    use mysql_async::prelude::Queryable;
    s.conn
        .query_drop("CREATE TABLE IF NOT EXISTS dml_fixture (id INT PRIMARY KEY)")
        .await
        .expect("ddl");
    s.conn
        .query_drop("TRUNCATE dml_fixture")
        .await
        .expect("truncate");
    let (rs, outcome) = collect(
        &mut s,
        "INSERT INTO dml_fixture VALUES (1),(2),(3)",
        FetchOptions::default(),
    )
    .await;
    assert!(rs.is_none());
    assert_eq!(outcome.rows_affected, Some(3));
}

/// BIT columns reach the grid as numbers (0/1 like DBeaver), not as the
/// raw `\x00`/`\x01` bytes the text protocol carries.
#[tokio::test]
async fn bit_columns_decode_as_numbers() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    use mysql_async::prelude::Queryable;
    s.conn
        .query_drop("CREATE TABLE IF NOT EXISTS bit_fixture (id INT PRIMARY KEY, flag BIT(1), wide BIT(64))")
        .await
        .expect("ddl");
    s.conn
        .query_drop("TRUNCATE bit_fixture")
        .await
        .expect("truncate");
    s.conn
        .query_drop("INSERT INTO bit_fixture VALUES (1, b'0', b'0'), (2, b'1', 18446744073709551615), (3, NULL, 256)")
        .await
        .expect("insert");
    let (rs, outcome) = collect(
        &mut s,
        "SELECT flag, wide FROM bit_fixture ORDER BY id",
        FetchOptions::default(),
    )
    .await;
    assert_eq!(outcome.state, ResultSetState::Complete);
    let rs = rs.expect("result set");
    assert_eq!(rs.schema()[0].type_display, "bit");
    assert_eq!(rs.value_at(0, 0), Some(ValueRef::U64(0)));
    assert_eq!(rs.value_at(0, 1), Some(ValueRef::U64(0)));
    assert_eq!(rs.value_at(1, 0), Some(ValueRef::U64(1)));
    assert_eq!(rs.value_at(1, 1), Some(ValueRef::U64(u64::MAX)));
    assert_eq!(rs.value_at(2, 0), Some(ValueRef::Null));
    assert_eq!(rs.value_at(2, 1), Some(ValueRef::U64(256)));
}

#[tokio::test]
async fn generated_series_streams_in_chunks() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    // 100k rows via recursive CTE (cte_max_recursion_depth default 1000).
    use mysql_async::prelude::Queryable;
    s.conn
        .query_drop("SET SESSION cte_max_recursion_depth = 200000")
        .await
        .expect("set");
    let (rs, outcome) = collect(
        &mut s,
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < 100000)
         SELECT n, CONCAT('row ', n) FROM seq",
        FetchOptions::default(),
    )
    .await;
    assert_eq!(outcome.state, ResultSetState::Complete);
    let rs = rs.expect("result set");
    assert_eq!(rs.rows(), 100_000);
    assert!(outcome.stats.chunks > 1, "single chunk over 100k rows");
    assert_eq!(rs.value_at(99_999, 0), Some(ValueRef::I64(100_000)));
}

#[tokio::test]
async fn warnings_via_show_warnings() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let mut s = MySession::connect_plain(&url).await.expect("connection");
    let (_, _) = collect(
        &mut s,
        "SELECT CAST('not-a-number' AS SIGNED)",
        FetchOptions::default(),
    )
    .await;
    let warnings = alamo_driver_mysql::fetch_warnings(&mut s.conn)
        .await
        .expect("show warnings");
    assert!(!warnings.is_empty(), "no warning from the invalid cast");
}

/// "Stop" on a paused stream (§5): `KILL QUERY` before the drain, so the
/// rest of a huge result is not read (minutes behind SSH), and the
/// interruption never reaches the next statement — not even when the
/// server had already sent everything and the KILL finds it idle.
async fn stop_kills_instead_of_draining(url: &str) {
    use alamo_core::stream::{StopReason, StreamControl};
    use mysql_async::prelude::Queryable;

    let mut s = MySession::connect_plain(url).await.expect("connection");
    let (kill_url, id) = s.cancel_parts();
    let tls = s.tls_options();
    let dialect = MyDialect {
        mariadb: s.is_mariadb,
        ..MyDialect::default()
    };
    let opts = FetchOptions {
        pause_after_rows: Some(100),
        ..FetchOptions::default()
    };
    let seq = |n: u32| {
        format!("WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < {n})")
    };
    for sql in [
        // 125M rows: draining them would take minutes.
        format!("{} SELECT a.n, b.n, c.n FROM seq a, seq b, seq c", seq(500)),
        // Fits in the socket buffers: the server is done before the KILL.
        format!("{} SELECT n FROM seq", seq(150)),
    ] {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let stream = execute_streaming(
            &mut s.conn,
            &sql,
            &dialect,
            &opts,
            &mut rx,
            async {
                alamo_driver_mysql::kill(&kill_url, id, false, &tls)
                    .await
                    .expect("KILL QUERY");
            },
            move |ev| {
                if let StreamEvent::BudgetReached { .. } = ev {
                    let _ = tx.try_send(StreamControl::Stop(StopReason::UserStopped));
                }
            },
        );
        let outcome = tokio::time::timeout(Duration::from_secs(20), stream)
            .await
            .expect("stop drained the whole result instead of killing it")
            .expect("execute_streaming");
        assert_eq!(outcome.state, ResultSetState::Truncated);
        assert_eq!(outcome.stats.rows, 100);
        let next: Option<i64> = s
            .conn
            .query_first("SELECT 1")
            .await
            .expect("next statement");
        assert_eq!(next, Some(1));
    }
}

#[tokio::test]
async fn stop_kills_instead_of_draining_mysql() {
    let url = require_url!(mysql_url(), MYSQL_VAR);
    stop_kills_instead_of_draining(&url).await;
}

#[tokio::test]
async fn stop_kills_instead_of_draining_mariadb() {
    let url = require_url!(mariadb_url(), MARIADB_VAR);
    stop_kills_instead_of_draining(&url).await;
}

#[tokio::test]
async fn mariadb_streams_too() {
    let url = require_url!(mariadb_url(), MARIADB_VAR);
    let mut s = MySession::connect_plain(&url)
        .await
        .expect("MariaDB connection");
    let (rs, outcome) = collect(
        &mut s,
        "SELECT 1, 'text', CAST('2024-01-01' AS DATE)",
        FetchOptions::default(),
    )
    .await;
    assert_eq!(outcome.state, ResultSetState::Complete);
    assert_eq!(rs.expect("result set").rows(), 1);
}

// ------------------------------------------------------------------ TLS (§12)

/// TLS cipher of the current session: empty string = plaintext.
async fn ssl_cipher(s: &mut MySession) -> String {
    use mysql_async::prelude::Queryable;
    let row: Option<(String, String)> = s
        .conn
        .query_first("SHOW STATUS LIKE 'Ssl_cipher'")
        .await
        .expect("SHOW STATUS");
    row.map(|(_, v)| v).unwrap_or_default()
}

/// MySQL 8.x containers generate self-signed certificates and enable TLS
/// by default: `require` must establish an encrypted channel.
#[tokio::test]
async fn tls_require_establishes_encrypted_channel() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let opts = TlsOptions {
        mode: TlsMode::Require,
        root_cert: None,
        tls_server_name: None,
    };
    let mut s = MySession::connect(&url, &opts)
        .await
        .expect("TLS require connection");
    let cipher = ssl_cipher(&mut s).await;
    assert!(!cipher.is_empty(), "require but session in plaintext");
    assert_eq!(s.tls_options().mode, TlsMode::Require);
}

/// `verify-full` against the container's self-signed certificate: the chain
/// leads to no trusted root, the connect MUST fail — never a silent
/// downgrade to unverified.
#[tokio::test]
async fn tls_verify_full_rejects_self_signed() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let opts = TlsOptions {
        mode: TlsMode::VerifyFull,
        root_cert: None,
        tls_server_name: None,
    };
    assert!(
        MySession::connect(&url, &opts).await.is_err(),
        "verify-full accepted a self-signed certificate"
    );
}

/// `verify-ca` does not check the name: without the profile's CA it would accept
/// the public certificate of any site. Rejected before the socket,
/// with the reason in the message.
#[tokio::test]
async fn tls_verify_ca_without_ca_rejected() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let opts = TlsOptions {
        mode: TlsMode::VerifyCa,
        root_cert: None,
        tls_server_name: None,
    };
    let err = match MySession::connect(&url, &opts).await {
        Ok(_) => panic!("verify-ca without CA connected"),
        Err(e) => e.into_db_error(),
    };
    assert!(
        err.message.starts_with("verify-ca requires"),
        "{}",
        err.message
    );
}

/// With the CA, verify-ca on MySQL is rejected with a clear message
/// until mysql_async publishes the fix for the name check
/// (see `ssl_opts`).
#[tokio::test]
async fn tls_verify_ca_mysql_not_available() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let opts = TlsOptions {
        mode: TlsMode::VerifyCa,
        root_cert: Some("/any/ca.pem".into()),
        tls_server_name: None,
    };
    let err = match MySession::connect(&url, &opts).await {
        Ok(_) => panic!("verify-ca on MySQL connected"),
        Err(e) => e.into_db_error(),
    };
    assert!(
        err.message.starts_with("verify-ca is not yet available"),
        "{}",
        err.message
    );
}

/// `prefer` works both with and without TLS: if the server does not offer it
/// it downgrades to plaintext, and the recorded mode (for cancel/KILL) matches
/// the actual channel.
#[tokio::test]
async fn tls_prefer_matches_the_channel() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    for url in [mysql_url(), mariadb_url()].into_iter().flatten() {
        let opts = TlsOptions {
            mode: TlsMode::Prefer,
            root_cert: None,
            tls_server_name: None,
        };
        let mut s = MySession::connect(&url, &opts)
            .await
            .expect("prefer connection");
        let cipher = ssl_cipher(&mut s).await;
        match s.tls_options().mode {
            TlsMode::Prefer => assert!(!cipher.is_empty(), "TLS mode but plaintext channel"),
            TlsMode::Disable => assert!(cipher.is_empty(), "downgraded but encrypted channel"),
            other => panic!("unexpected mode after prefer: {other:?}"),
        }
    }
}

/// The out-of-band KILL must speak the target session's TLS.
#[tokio::test]
async fn tls_kill_query_on_encrypted_session() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!(mysql_url(), MYSQL_VAR);
    let opts = TlsOptions {
        mode: TlsMode::Require,
        root_cert: None,
        tls_server_name: None,
    };
    let s = MySession::connect(&url, &opts)
        .await
        .expect("TLS connection");
    let ok = s.cancel_query().await.expect("KILL QUERY via TLS");
    assert!(ok, "KILL QUERY rejected");
}

/// `kill_idle` closes only idle connections of the same user: one running a
/// query, or another user's idle one with an id in the list, is never
/// touched (a proxy sharing server connections could put them there).
#[tokio::test]
async fn kill_idle_spares_busy_and_other_users() {
    use mysql_async::prelude::Queryable;
    let Some(url) = mysql_url() else {
        skip_or_fail("ALAMO_MYSQL_TEST_URL");
        return;
    };
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let mut admin = MySession::connect_plain(&url).await.expect("admin");
    for sql in [
        "DROP USER IF EXISTS 'alamo_other'@'%'".to_string(),
        "CREATE USER 'alamo_other'@'%' IDENTIFIED BY 'alamo'".to_string(),
        format!("GRANT SELECT ON `{db}`.* TO 'alamo_other'@'%'"),
    ] {
        admin.conn.query_drop(sql).await.expect("user");
    }
    let other_url = format!("mysql://alamo_other:alamo@{server}/{db}");
    let mut other = MySession::connect_plain(&other_url)
        .await
        .expect("other user");
    let mut idle = MySession::connect_plain(&url).await.expect("idle");
    let mut busy = MySession::connect_plain(&url).await.expect("busy");
    let busy_id = busy.connection_id;
    let running = tokio::spawn(async move {
        let r = busy.conn.query_drop("SELECT SLEEP(2)").await;
        (r, busy)
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut killer = MySession::connect_plain(&url).await.expect("killer");
    let killed = killer
        .kill_idle(&[idle.connection_id, busy_id, other.connection_id])
        .await;
    let (r, _busy) = running.await.expect("busy task");
    let idle_alive = idle.ping().await;
    let other_alive = other.ping().await;
    let _ = admin
        .conn
        .query_drop("DROP USER IF EXISTS 'alamo_other'@'%'")
        .await;
    assert_eq!(killed, 1);
    assert!(!idle_alive, "the idle connection should have been closed");
    r.expect("the running query must not be killed");
    assert!(other_alive, "another user's connection must not be killed");
}
