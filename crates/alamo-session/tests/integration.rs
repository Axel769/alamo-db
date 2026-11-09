//! Integration tests against a real PostgreSQL (§15: from M1).
//!
//! They need `ALAMO_TEST_URL` (or `ALAMO_BENCH_URL`); without it they are
//! skipped — so `cargo test` stays green in CI without services.
//!
//! ```sh
//! ALAMO_TEST_URL='postgres://postgres:alamo@localhost:5432/alamo_bench' \
//!   cargo test -p alamo-session --test integration
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alamo_driver_postgres::FetchOptions;
use alamo_session::driver::builtin_for_url;
use alamo_session::events::{ExecEvent, OutcomeOut};
use alamo_session::{CellAssign, ColumnFilter, FilterOp, OrderBy, RowUpdate, Session, TableQuery};

fn test_url() -> Option<String> {
    std::env::var("ALAMO_TEST_URL")
        .or_else(|_| std::env::var("ALAMO_BENCH_URL"))
        .ok()
}

/// Noisy skip or hard gate: if `ALAMO_REQUIRE_TEST_DBS` lists the
/// variable (comma-separated list), its absence is a FAILURE,
/// not a skip. The matrix (`cargo xtask test-matrix`) always sets it:
/// broken env wiring there must turn red, not "green by skip" — it has
/// already happened that a whole run passed without touching the server.
/// Locally without the flag it stays a skip with eprintln: bare `cargo test`
/// must stay green without services.
fn skip_or_fail(var: &str) {
    let required = std::env::var("ALAMO_REQUIRE_TEST_DBS").unwrap_or_default();
    if required.split(',').any(|v| v.trim() == var) {
        panic!("{var} not set but required (ALAMO_REQUIRE_TEST_DBS={required})");
    }
    eprintln!("skipped: {var} not set");
}

macro_rules! require_url {
    () => {
        match test_url() {
            Some(u) => u,
            None => {
                skip_or_fail("ALAMO_TEST_URL");
                return;
            }
        }
    };
}

struct Collected {
    events: Arc<Mutex<Vec<ExecEvent>>>,
    done: Arc<tokio::sync::Notify>,
}

fn collector() -> (Collected, impl Fn(ExecEvent) + Send + Sync + 'static) {
    let events: Arc<Mutex<Vec<ExecEvent>>> = Arc::default();
    let done = Arc::new(tokio::sync::Notify::new());
    let (e2, d2) = (Arc::clone(&events), Arc::clone(&done));
    let emit = move |ev: ExecEvent| {
        let finished = matches!(ev, ExecEvent::Finished { .. });
        e2.lock().unwrap().push(ev);
        if finished {
            d2.notify_one();
        }
    };
    (Collected { events, done }, emit)
}

async fn wait_done(c: &Collected) {
    tokio::time::timeout(Duration::from_secs(30), c.done.notified())
        .await
        .expect("execution did not finish within 30s");
}

#[tokio::test]
async fn select_streams_and_windows_during_and_after() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT g AS id, 'row ' || g AS label FROM generate_series(1, 5000) g".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    let result_id = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema {
                result_id, columns, ..
            } => {
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].name, "id");
                Some(*result_id)
            }
            _ => None,
        })
        .expect("schema never arrived");
    assert!(events.iter().any(|e| matches!(
        e,
        ExecEvent::StatementFinished {
            outcome: OutcomeOut::RowSet { rows: 5000, .. },
            ..
        }
    )));

    let live = session
        .result(result_id)
        .expect("result set in the registry");
    assert_eq!(live.rows(), 5000);
    let w = live.window(4990..5000, 0..2);
    assert_eq!(w.rows, 4990..5000);
    assert_eq!(w.columns[1].values[9], "row 5000");
}

#[tokio::test]
async fn error_position_is_relative_to_statement() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    // First statement valid, second with a syntax error: the position
    // must be relative to the second, with the offset of its start (§6.1).
    session
        .execute(
            "SELECT 1; SELECT donde FROM;".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    let err = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Failed { error },
                index,
                ..
            } => Some((error.clone(), *index)),
            _ => None,
        })
        .expect("error expected");
    assert_eq!(err.1, 1, "the error belongs to the second statement");
    assert!(err.0.position_char.is_some(), "PG reports the position");
    assert_eq!(err.0.statement_char_offset, Some(10));
    assert_eq!(err.0.category, "sql");

    // §6.4: after the error nothing else runs; here there is no third
    // statement, but the final state must be failed.
    assert!(events.iter().any(|e| matches!(
        e,
        ExecEvent::Finished {
            state: "failed",
            ..
        }
    )));
}

#[tokio::test]
async fn cancel_stops_long_query_and_is_not_a_red_error() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    let exec_id = session
        .execute("SELECT pg_sleep(30)".into(), FetchOptions::default(), emit)
        .expect("execute");

    tokio::time::sleep(Duration::from_millis(300)).await;
    session.cancel(exec_id);
    // Idempotence (§7.2): the second click must not break anything.
    session.cancel(exec_id);
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Cancelled { .. },
                ..
            }
        )),
        "expected outcome: Cancelled, never Failed (§6.2). Events: {events:?}"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        ExecEvent::Finished {
            state: "cancelled",
            ..
        }
    )));
}

#[tokio::test]
async fn budget_pause_waits_for_user_and_stop_keeps_partial_data() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    // Tiny budget: the pause triggers after a few chunks.
    let opts = FetchOptions {
        soft_budget_bytes: Some(256 * 1024),
        ..FetchOptions::default()
    };
    let exec_id = session
        .execute(
            "SELECT g, repeat('x', 100) FROM generate_series(1, 200000) g".into(),
            opts,
            emit,
        )
        .expect("execute");

    // Wait for the pause.
    let paused = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some((rid, rows)) = c.events.lock().unwrap().iter().find_map(|e| match e {
                ExecEvent::BudgetPaused {
                    result_id, rows, ..
                } => Some((*result_id, *rows)),
                _ => None,
            }) {
                break (rid, rows);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("BudgetPaused never arrived");

    // During the pause the window is readable (§8: fetch_window on the live set).
    let live = session.result(paused.0).expect("result");
    assert!(live.rows() > 0);
    let _ = live.window(0..10, 0..2);

    // "Stop": partial data kept, state truncated (§5, §7.3).
    session.budget_stop(exec_id);
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    let (rows, state) = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, state, .. },
                ..
            } => Some((*rows, *state)),
            _ => None,
        })
        .expect("RowSet expected");
    assert!(rows > 0 && rows < 200_000, "partial: {rows}");
    assert_eq!(state, "truncated");
}

/// Precautionary pause at N rows (SQL tab): it stops EXACTLY at N,
/// reported as a row pause; "Continue" fetches the rest without another
/// pause at 2N. The user's SQL stays as written (§5.2).
async fn row_pause_then_resume(session: &Session, sql: &str, total: u64) {
    let (c, emit) = collector();
    let opts = FetchOptions {
        pause_after_rows: Some(10_000),
        ..FetchOptions::default()
    };
    let exec_id = session.execute(sql.into(), opts, emit).expect("execute");
    let paused = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(p) = c.events.lock().unwrap().iter().find_map(|e| match e {
                ExecEvent::BudgetPaused { rows, by_rows, .. } => Some((*rows, *by_rows)),
                _ => None,
            }) {
                break p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("row pause never arrived");
    assert_eq!(paused, (10_000, true));

    session.budget_resume(exec_id);
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    let pauses = events
        .iter()
        .filter(|e| matches!(e, ExecEvent::BudgetPaused { .. }))
        .count();
    assert_eq!(pauses, 1, "after Continue no other row pause");
    let (rows, state) = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, state, .. },
                ..
            } => Some((*rows, *state)),
            _ => None,
        })
        .expect("RowSet expected");
    assert_eq!((rows, state), (total, "complete"));
}

/// Pages on demand (SQL tab, like DBeaver): pause at 200, "more rows"
/// up to 400 and to 600 — a double request does not skip a page — then
/// "Load all" fetches the rest.
async fn row_pages_then_all(session: &Session, sql: &str, total: u64) {
    let (c, emit) = collector();
    let opts = FetchOptions {
        pause_after_rows: Some(200),
        ..FetchOptions::default()
    };
    let exec_id = session.execute(sql.into(), opts, emit).expect("execute");
    let pause_n = |n: usize| {
        let c = &c;
        async move {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let pauses: Vec<(u64, bool)> = c
                        .events
                        .lock()
                        .unwrap()
                        .iter()
                        .filter_map(|e| match e {
                            ExecEvent::BudgetPaused { rows, by_rows, .. } => {
                                Some((*rows, *by_rows))
                            }
                            _ => None,
                        })
                        .collect();
                    if pauses.len() >= n {
                        break pauses[n - 1];
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("pause {n} never arrived"))
        }
    };
    assert_eq!(pause_n(1).await, (200, true));
    session.budget_more(exec_id);
    assert_eq!(pause_n(2).await, (400, true));
    session.budget_more(exec_id);
    session.budget_more(exec_id); // double: ignored, does not consume the 3rd pause
    assert_eq!(pause_n(3).await, (600, true));
    session.budget_resume(exec_id);
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    let pauses = events
        .iter()
        .filter(|e| matches!(e, ExecEvent::BudgetPaused { .. }))
        .count();
    assert_eq!(pauses, 3);
    let rows = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, .. },
                ..
            } => Some(*rows),
            _ => None,
        })
        .expect("RowSet expected");
    assert_eq!(rows, total);
}

#[tokio::test]
async fn row_pages_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    row_pages_then_all(
        &session,
        "SELECT g FROM generate_series(1, 25000) g",
        25_000,
    )
    .await;
}

#[tokio::test]
async fn row_pause_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    row_pause_then_resume(
        &session,
        "SELECT g FROM generate_series(1, 25000) g",
        25_000,
    )
    .await;
}

#[tokio::test]
async fn pause_idle_timeout_stops_fetch_and_warns() {
    let url = require_url!();
    let mut session = Session::connect(&url).await.expect("connection");
    // §5.1: no answer to the banner → automatic stop. Short timeout for
    // the test; the default is PAUSE_TIMEOUT_DEFAULT.
    session.pause_timeout = Duration::from_millis(700);
    let (c, emit) = collector();
    let opts = FetchOptions {
        soft_budget_bytes: Some(256 * 1024),
        ..FetchOptions::default()
    };
    session
        .execute(
            "SELECT g, repeat('x', 100) FROM generate_series(1, 200000) g".into(),
            opts,
            emit,
        )
        .expect("execute");

    // No budget_resume/budget_stop: the watchdog decides.
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ExecEvent::BudgetPaused { .. })),
        "the budget pause must have triggered: {events:?}"
    );
    let (rows, state) = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, state, .. },
                ..
            } => Some((*rows, *state)),
            _ => None,
        })
        .expect("RowSet expected");
    assert!(rows > 0 && rows < 200_000, "partial: {rows}");
    assert_eq!(state, "truncated");
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::Notice { notice, .. }
                if notice.severity == "warning" && notice.message.contains("paused")
        )),
        "the user must know why the fetch stopped: {events:?}"
    );
}

#[tokio::test]
async fn terminate_backend_kills_session_and_execution_ends() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    let exec_id = session
        .execute("SELECT pg_sleep(30)".into(), FetchOptions::default(), emit)
        .expect("execute");

    tokio::time::sleep(Duration::from_millis(300)).await;
    // Escalation §7.2: first the cancel (here we simulate that it had no
    // effect), then the termination. The Cancelling state maps the death
    // of the connection to "cancelled", not to a red error.
    session.cancel(exec_id);
    let terminated = session.terminate().await.expect("terminate");
    assert!(terminated, "pg_terminate_backend must succeed locally");

    wait_done(&c).await;
    {
        let events = c.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ExecEvent::Finished {
                    state: "cancelled",
                    ..
                }
            )),
            "after cancel+terminate the final state is cancelled: {events:?}"
        );
    }

    // The session is dead: a new execution must fail, not hang.
    let (c2, emit2) = collector();
    if session
        .execute("SELECT 1".into(), FetchOptions::default(), emit2)
        .is_ok()
    {
        wait_done(&c2).await;
        let ev2 = c2.events.lock().unwrap();
        assert!(
            ev2.iter().any(|e| matches!(
                e,
                ExecEvent::Finished {
                    state: "failed",
                    ..
                }
            )),
            "execution on a dead connection: failed expected: {ev2:?}"
        );
    }
}

#[tokio::test]
async fn plpgsql_notices_reach_the_ui() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DO $$ BEGIN RAISE NOTICE 'debug from the stored proc'; END $$".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::Notice { notice, .. } if notice.message.contains("debug from the stored proc")
        )),
        "RAISE NOTICE is *the* way to debug in PL/pgSQL (§6): {events:?}"
    );
}

#[tokio::test]
async fn dml_reports_affected_and_second_execution_rejected_while_running() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "CREATE TEMP TABLE m1_t (a int); INSERT INTO m1_t SELECT * FROM generate_series(1,7); DROP TABLE m1_t"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");

    // While it runs, a second execution must be rejected (§3: single sticky).
    let err = session.execute("SELECT 1".into(), FetchOptions::default(), |_| {});
    assert!(err.is_err());

    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        ExecEvent::StatementFinished {
            outcome: OutcomeOut::Affected { rows: 7 },
            ..
        }
    )));
}

// ---------------------------------------------------------------- introspection (M2)

#[tokio::test]
async fn introspection_schemas_relations_columns() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");

    let schemas = session.schemas().await.expect("schemas");
    assert!(schemas.iter().any(|s| s.name == "public" && !s.is_system));
    assert!(schemas
        .iter()
        .any(|s| s.name == "pg_catalog" && s.is_system));

    let rels = session.relations("public").await.expect("relations");
    let wide = rels
        .iter()
        .find(|r| r.name == "bench_wide")
        .expect("bench_wide in the tree");
    assert_eq!(wide.kind, alamo_core::schema_info::RelKind::Table);
    // reltuples: estimate present after the first autovacuum/analyze of prepare.
    // The value is not asserted: it is a planner estimate by definition.

    let cols = session
        .columns("public", "bench_wide")
        .await
        .expect("columns");
    assert_eq!(cols.len(), 14);
    let payload = cols.iter().find(|c| c.name == "payload").expect("payload");
    assert_eq!(payload.type_display, "jsonb");
    assert!(payload.nullable);
    assert_eq!(cols[0].ordinal, 1);
}

/// §3.3: the databases of the server, the connected one included, without
/// the templates (template0 refuses connections).
#[tokio::test]
async fn databases_postgres() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let current = session
        .current_database()
        .await
        .expect("current_database")
        .expect("PG always has one");
    let dbs: Vec<_> = session
        .databases()
        .await
        .expect("databases")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(dbs.contains(&current), "{current} missing in {dbs:?}");
    assert!(dbs.iter().all(|d| d != "template0" && d != "template1"));
    let mut sorted = dbs.clone();
    sorted.sort();
    assert_eq!(dbs, sorted);
}

/// Held by the tests that create a schema and by the one that compares two
/// reads of the schema list: tests run in parallel on the same database, and a
/// schema created between the two reads made them differ.
static SCHEMA_LIST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn schema_stats_postgres() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let _list = SCHEMA_LIST.lock().await;
    let stats = session.schema_stats().await.expect("schema_stats");
    let names: Vec<_> = session
        .schemas()
        .await
        .expect("schemas")
        .into_iter()
        .map(|s| s.name)
        .collect();
    // Same schemas as the tree, in the same order: the connection page
    // shows the names from the list and enriches them with these.
    assert_eq!(
        stats.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        names
    );
    let catalog = stats.iter().find(|s| s.name == "pg_catalog").unwrap();
    assert!(catalog.is_system);
    assert!(catalog.tables > 50, "{catalog:?}");
    assert!(catalog.size_bytes.unwrap_or(0) > 0, "{catalog:?}");
    // Encoding and collation belong to the database: set and equal for all.
    assert!(catalog.charset.is_some() && catalog.collation.is_some());
    assert!(stats.iter().all(|s| s.charset == catalog.charset));
}

/// Runs a setup script and waits for it to finish.
async fn run_setup(session: &Session, sql: &str) {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .expect("setup");
    wait_done(&c).await;
}

#[tokio::test]
async fn foreign_keys_postgres() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS fk_riga, fk_ordine, fk_cliente;
         CREATE TABLE fk_cliente (id int PRIMARY KEY, capo_id int REFERENCES fk_cliente(id));
         CREATE TABLE fk_ordine (anno int, num int, cliente_id int NOT NULL,
             PRIMARY KEY (anno, num),
             CONSTRAINT fk_ordine_cliente FOREIGN KEY (cliente_id) REFERENCES fk_cliente(id));
         CREATE TABLE fk_riga (id int PRIMARY KEY, num int, anno int,
             CONSTRAINT fk_riga_ordine FOREIGN KEY (anno, num) REFERENCES fk_ordine(anno, num));",
    )
    .await;

    // fk_ordine: one outgoing (to fk_cliente) and one incoming (from fk_riga).
    let fks = session
        .foreign_keys("public", "fk_ordine")
        .await
        .expect("foreign_keys");
    let out = fks.iter().find(|f| f.name == "fk_ordine_cliente").unwrap();
    assert_eq!(
        (out.table.as_str(), out.ref_table.as_str()),
        ("fk_ordine", "fk_cliente")
    );
    assert_eq!(out.columns, ["cliente_id"]);
    assert_eq!(out.ref_columns, ["id"]);
    // Composite: the order is the constraint's (anno, num), not that
    // of the columns in the child table (num, anno).
    let inc = fks.iter().find(|f| f.name == "fk_riga_ordine").unwrap();
    assert_eq!(inc.table, "fk_riga");
    assert_eq!(inc.columns, ["anno", "num"]);
    assert_eq!(inc.ref_columns, ["anno", "num"]);
    assert_eq!(fks.len(), 2);

    // Self-reference: a single row, valid in both directions.
    let own = session
        .foreign_keys("public", "fk_cliente")
        .await
        .expect("foreign_keys");
    assert_eq!(own.iter().filter(|f| f.table == "fk_cliente").count(), 1);
    assert!(own.iter().any(|f| f.table == "fk_ordine"));

    run_setup(&session, "DROP TABLE fk_riga, fk_ordine, fk_cliente").await;
}

#[tokio::test]
async fn introspection_runs_while_execution_streams() {
    // §3: introspection has its own connection — it must answer even
    // while the sticky is executing.
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute("SELECT pg_sleep(2)".into(), FetchOptions::default(), emit)
        .expect("execute");

    let schemas = tokio::time::timeout(Duration::from_secs(1), session.schemas())
        .await
        .expect("introspection blocked behind the sticky")
        .expect("schemas");
    assert!(!schemas.is_empty());
    wait_done(&c).await;
}

#[tokio::test]
async fn autocomplete_catalog_flat() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let entries = session.autocomplete_catalog(None).await.expect("catalog");
    assert!(entries
        .iter()
        .any(|(s, t, c)| s == "public" && t == "bench_wide" && c == "payload"));
    // No system schemas in the autocomplete catalog.
    assert!(!entries.iter().any(|(s, _, _)| s == "pg_catalog"));
    let one = session
        .autocomplete_catalog(Some("public"))
        .await
        .expect("catalog of one schema");
    assert!(one
        .iter()
        .any(|(_, t, c)| t == "bench_wide" && c == "payload"));
    assert!(one.iter().all(|(s, _, _)| s == "public"));
}

// ------------------------------------------------------------------ M2: table tab and sort

#[tokio::test]
async fn table_data_runs_on_shared_lane_while_sticky_executes() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");

    // The sticky is busy: the table tab must load anyway (§3.1).
    let (c_sticky, emit_sticky) = collector();
    session
        .execute(
            "SELECT pg_sleep(2)".into(),
            FetchOptions::default(),
            emit_sticky,
        )
        .expect("execute");

    let (c_table, emit_table) = collector();
    let exec_id = tokio::time::timeout(
        Duration::from_secs(1),
        session.fetch_table(
            "public",
            "bench_narrow",
            &Default::default(),
            FetchOptions::default(),
            emit_table,
        ),
    )
    .await
    .expect("fetch_table blocked behind the sticky")
    .expect("fetch_table");
    assert!(exec_id > 0);
    wait_done(&c_table).await;

    // Block: the MutexGuard must not cross the await (clippy
    // `await_holding_lock` does not recognise the explicit `drop`).
    let rows = {
        let events = c_table.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, .. },
                    ..
                } => Some(*rows),
                _ => None,
            })
            .expect("no result set from the table tab")
    };
    // LIMIT of the first page (`LimitsIn::page_rows`).
    assert_eq!(rows, alamo_session::LimitsIn::default().page_rows);
    wait_done(&c_sticky).await;
}

#[tokio::test]
async fn table_query_requeries_with_server_sort_and_filter() {
    // Decision 10, second half: sort and filter as a server re-query, not a
    // local permutation. Order and cut already come from the server.
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    let query = TableQuery {
        order_by: vec![OrderBy {
            column: "id".into(),
            desc: true,
        }],
        filters: vec![ColumnFilter {
            column: "id".into(),
            op: FilterOp::Le,
            value: "10".into(),
        }],
        ..Default::default()
    };
    session
        .fetch_table(
            "public",
            "bench_narrow",
            &query,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;

    let (result_id, rows) = {
        let events = c.events.lock().unwrap();
        let rid = events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema never arrived");
        let rows = events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, .. },
                    ..
                } => Some(*rows),
                _ => None,
            })
            .expect("no result set");
        (rid, rows)
    };
    assert_eq!(rows, 10, "WHERE id <= 10 on a 1..N dataset");
    let live = session
        .result(result_id)
        .expect("result set in the registry");
    let w = live.window(0..2, 0..1);
    assert_eq!(
        w.columns[0].values[0], "10",
        "ORDER BY id DESC: first row = 10"
    );
    assert_eq!(w.columns[0].values[1], "9");
}

/// Hand-written WHERE bar: it is ANDed with the chips, a trailing comment
/// does not swallow the LIMIT, and a `;` does not start a second statement.
#[tokio::test]
async fn table_typed_where_filters_and_rejects_a_second_statement() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS typed_where;
         CREATE TABLE typed_where (id int PRIMARY KEY);
         INSERT INTO typed_where SELECT generate_series(1, 20)",
    )
    .await;

    let (c, emit) = collector();
    let query = TableQuery {
        filters: vec![ColumnFilter {
            column: "id".into(),
            op: FilterOp::Le,
            value: "18".into(),
        }],
        where_sql: "id > 15 OR id = 1 -- trailing comment".into(),
        ..Default::default()
    };
    session
        .fetch_table(
            "public",
            "typed_where",
            &query,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    assert_eq!(table_result(&c).1, 4, "id <= 18 AND (id > 15 OR id = 1)");

    for typed in [
        "1=1; DELETE FROM typed_where",
        "1=1); DELETE FROM typed_where; SELECT (1",
    ] {
        let query = TableQuery {
            where_sql: typed.into(),
            ..Default::default()
        };
        let err = session
            .fetch_table(
                "public",
                "typed_where",
                &query,
                FetchOptions::default(),
                |_| {},
            )
            .await
            .expect_err(typed);
        assert!(
            err.message.contains("a single condition"),
            "{typed}: {}",
            err.message
        );
        let err = session
            .fetch_table_more("public", "typed_where", &query, 0)
            .await
            .expect_err(typed);
        assert!(
            err.message.contains("a single condition"),
            "{typed}: {}",
            err.message
        );
    }
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM typed_where").await,
        "20"
    );
}

/// Read-only profile: the hand-written WHERE runs in a
/// READ ONLY transaction (a function in the WHERE could write), both
/// on the first page and on the following ones.
#[tokio::test]
async fn pg_read_only_typed_where_runs_in_a_read_only_txn() {
    let url = require_url!();
    let setup = Session::connect(&url).await.expect("connection");
    run_setup(
        &setup,
        "DROP TABLE IF EXISTS typed_where_ro;
         DROP SEQUENCE IF EXISTS typed_where_seq;
         CREATE SEQUENCE typed_where_seq;
         CREATE TABLE typed_where_ro (id int PRIMARY KEY);
         INSERT INTO typed_where_ro SELECT generate_series(1, 600)",
    )
    .await;
    let ro = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        false,
        true,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("read-only connection");

    // nextval writes: in a READ ONLY transaction the server rejects it.
    let (c, emit) = collector();
    let query = TableQuery {
        where_sql: "nextval('typed_where_seq') > 0".into(),
        ..Default::default()
    };
    ro.fetch_table(
        "public",
        "typed_where_ro",
        &query,
        FetchOptions::default(),
        emit,
    )
    .await
    .expect("the SELECT starts: the server is the one rejecting");
    wait_done(&c).await;
    assert!(
        c.events.lock().unwrap().iter().any(|e| matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Failed { .. },
                ..
            }
        )),
        "nextval must fail in read-only mode"
    );
    assert_eq!(
        scalar(&setup, "SELECT is_called::text FROM typed_where_seq").await,
        "false"
    );

    // A normal read passes, and the next page opens and closes its
    // transaction (the second page finds the connection clean).
    let (c, emit) = collector();
    let query = TableQuery {
        where_sql: "id > 0".into(),
        ..Default::default()
    };
    ro.fetch_table(
        "public",
        "typed_where_ro",
        &query,
        FetchOptions::default(),
        emit,
    )
    .await
    .expect("fetch_table");
    wait_done(&c).await;
    let (rid, rows) = table_result(&c);
    assert_eq!(rows, alamo_session::LimitsIn::default().page_rows);
    let more = ro
        .fetch_table_more("public", "typed_where_ro", &query, rid)
        .await
        .expect("next page");
    assert_eq!(
        more.added,
        600 - alamo_session::LimitsIn::default().page_rows
    );
    let again = ro
        .fetch_table_more("public", "typed_where_ro", &query, rid)
        .await
        .expect("third page: no transaction left open");
    assert_eq!(again.added, 0);
}

/// Reads the result set (id and rows) of the last collected `fetch_table`.
fn table_result(c: &Collected) -> (u64, u64) {
    let events = c.events.lock().unwrap();
    let rid = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("schema never arrived");
    let rows = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, .. },
                ..
            } => Some(*rows),
            _ => None,
        })
        .expect("no result set");
    (rid, rows)
}

/// Pages through the table to the end: 2500 rows, full pages of
/// `LimitsIn::page_rows` and the last one short (or empty, if 2500 is a multiple).
/// The rows are appended to the SAME result set, in order.
async fn page_through_fixture(session: &Session, schema: &str) {
    let query = TableQuery {
        order_by: vec![OrderBy {
            column: "id".into(),
            desc: false,
        }],
        filters: vec![],
        ..Default::default()
    };
    let (c, emit) = collector();
    session
        .fetch_table(
            schema,
            "more_fixture",
            &query,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let (rid, rows) = table_result(&c);
    let page = alamo_session::LimitsIn::default().page_rows;
    assert_eq!(rows, page);

    let mut total = page;
    loop {
        let more = session
            .fetch_table_more(schema, "more_fixture", &query, rid)
            .await
            .expect("next page");
        let added = (2500 - total).min(page);
        total += added;
        assert_eq!(
            (more.rows, more.added, more.more),
            (total, added, added == page)
        );
        if !more.more {
            break;
        }
    }
    // At the end: one more request adds nothing and does not fail.
    let more = session
        .fetch_table_more(schema, "more_fixture", &query, rid)
        .await
        .expect("past the end");
    assert_eq!((more.rows, more.added, more.more), (2500, 0, false));

    let live = session.result(rid).expect("result set in the registry");
    assert_eq!(live.rows(), 2500);
    let w = live.window(page - 1..page + 1, 0..1);
    assert_eq!(
        w.columns[0].values,
        vec![page.to_string(), (page + 1).to_string()]
    );
    let w = live.window(2499..2500, 0..1);
    assert_eq!(w.columns[0].values, vec!["2500"]);

    // The shared lane is free again: another table loads.
    let (c, emit) = collector();
    session
        .fetch_table(
            schema,
            "more_fixture",
            &query,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table after the pages");
    wait_done(&c).await;

    // Result set released (tab reloaded): clear error, lane free.
    session.release_result(rid);
    let err = session
        .fetch_table_more(schema, "more_fixture", &query, rid)
        .await
        .expect_err("result set released");
    assert!(err.message.contains("reload"), "{}", err.message);
}

#[tokio::test]
async fn table_more_appends_next_pages_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS more_fixture;
             CREATE TABLE more_fixture (id int PRIMARY KEY, label text);
             INSERT INTO more_fixture
               SELECT g, 'row ' || g FROM generate_series(1, 2500) g"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    page_through_fixture(&session, "public").await;
}

/// §16: the server-side query timeout. With a 1 s limit a 5 s sleep fails
/// as a Timeout (not a user cancel, which shares PG's 57014), well within
/// the sleep. Zero clears it: the sleep then runs to the end.
#[tokio::test]
async fn query_timeout_stops_a_slow_statement_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    session.set_limits(alamo_session::LimitsIn {
        query_timeout: Duration::from_secs(1),
        ..alamo_session::LimitsIn::default()
    });
    session.apply_query_timeout().await.expect("apply timeout");

    let (c, emit) = collector();
    session
        .execute("SELECT pg_sleep(5)".into(), FetchOptions::default(), emit)
        .expect("execute");
    tokio::time::timeout(Duration::from_secs(4), c.done.notified())
        .await
        .expect("the 1 s timeout did not stop the 5 s sleep");
    let category = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::Failed { error },
                    ..
                } => Some(error.category),
                _ => None,
            })
            .expect("a failure was expected")
    };
    assert_eq!(category, "timeout");

    // Cleared: the same sleep now runs to completion.
    session.set_limits(alamo_session::LimitsIn::default());
    session.apply_query_timeout().await.expect("clear timeout");
    let (c, emit) = collector();
    session
        .execute("SELECT pg_sleep(2)".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let failed = c.events.lock().unwrap().iter().any(|e| {
        matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Failed { .. },
                ..
            }
        )
    });
    assert!(!failed, "with no timeout the 2 s sleep must succeed");
}

/// §16: `set_limits` changes the SQL-tab pause and the table-tab page
/// end-to-end. The session is born with the defaults; a settings change
/// makes a 300-row query pause at 50 and a table page hold 100.
#[tokio::test]
async fn set_limits_changes_pause_and_page_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    session.set_limits(alamo_session::LimitsIn {
        pause_rows: 50,
        page_rows: 100,
        ..alamo_session::LimitsIn::default()
    });

    // SQL tab: the fetch pauses at the new row limit, reported as a row pause.
    let (c, emit) = collector();
    let exec_id = session
        .execute(
            "SELECT g AS id FROM generate_series(1, 300) g".into(),
            session.editor_fetch_options(),
            emit,
        )
        .expect("execute");
    let paused = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(p) = c.events.lock().unwrap().iter().find_map(|e| match e {
                ExecEvent::BudgetPaused { rows, by_rows, .. } => Some((*rows, *by_rows)),
                _ => None,
            }) {
                break p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("row pause never arrived");
    assert_eq!(paused, (50, true));
    // Free the sticky: a paused fetch holds the connection (§5.1).
    session.budget_resume(exec_id);
    wait_done(&c).await;

    // Table tab: the first page holds the new page size, with more to come.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS limits_fixture;
             CREATE TABLE limits_fixture (id int PRIMARY KEY);
             INSERT INTO limits_fixture SELECT g FROM generate_series(1, 300) g"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let query = TableQuery {
        order_by: vec![OrderBy {
            column: "id".into(),
            desc: false,
        }],
        ..Default::default()
    };
    let (c, emit) = collector();
    session
        .fetch_table(
            "public",
            "limits_fixture",
            &query,
            session.fetch_options(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let (rid, rows) = table_result(&c);
    assert_eq!(rows, 100);
    let more = session
        .fetch_table_more("public", "limits_fixture", &query, rid)
        .await
        .expect("next page");
    assert_eq!((more.added, more.more), (100, true));
}

#[tokio::test]
async fn table_ddl_covers_columns_constraints_and_indexes() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    // Fixture with constraints and an index: the canonical dataset has none.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS ddl_fixture;
             CREATE TABLE ddl_fixture (
                 id bigint NOT NULL,
                 label text DEFAULT 'n/d',
                 CONSTRAINT ddl_fixture_pk PRIMARY KEY (id)
             );
             CREATE INDEX ddl_fixture_label_idx ON ddl_fixture (label);"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let ddl = session
        .table_ddl("public", "ddl_fixture")
        .await
        .expect("ddl");
    assert!(
        ddl.contains("CREATE TABLE \"public\".\"ddl_fixture\""),
        "{ddl}"
    );
    assert!(ddl.contains("\"id\" bigint NOT NULL"), "{ddl}");
    assert!(ddl.contains("\"label\" text DEFAULT 'n/d'::text"), "{ddl}");
    assert!(
        ddl.contains("CONSTRAINT \"ddl_fixture_pk\" PRIMARY KEY (id)"),
        "{ddl}"
    );
    assert!(ddl.contains("ddl_fixture_label_idx"), "{ddl}");

    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE ddl_fixture".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
}

#[tokio::test]
async fn sort_permutation_reads_view_and_flags_incomplete() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT g AS n FROM generate_series(1, 100) g ORDER BY g".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let result_id = c
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("schema never arrived");
    let live = session.result(result_id).expect("result set");

    // Descending: there are no NULLs, the first row of the view is 100.
    let info = live.sort(0, true);
    assert_eq!(info.rows, 100);
    assert!(!info.incomplete, "complete result set marked incomplete");
    let w = live.window(0..3, 0..1);
    assert_eq!(w.columns[0].values, vec!["100", "99", "98"]);
    // Cell inspection reads in view space (§4.8).
    assert_eq!(live.cell_full(0, 0).as_deref(), Some("100"));

    // Without the sort we go back to the physical order.
    live.clear_sort();
    let w = live.window(0..2, 0..1);
    assert_eq!(w.columns[0].values, vec!["1", "2"]);
}

// ------------------------------------------------------------------ M3: multi-dialect session

fn mysql_url() -> Option<String> {
    std::env::var("ALAMO_MYSQL_TEST_URL").ok()
}

macro_rules! require_mysql {
    () => {
        match mysql_url() {
            Some(u) => u,
            None => {
                skip_or_fail("ALAMO_MYSQL_TEST_URL");
                return;
            }
        }
    };
}

/// Runs `sql` and waits for it to finish; a statement error fails the test.
async fn run_ok(session: &Session, sql: &str) {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    let failed = events.iter().find_map(|e| match e {
        ExecEvent::StatementFinished {
            outcome: OutcomeOut::Failed { error },
            ..
        } => Some(error.message.clone()),
        _ => None,
    });
    assert!(failed.is_none(), "{sql}: {failed:?}");
}

/// User with few connections (`max_user_connections`): tree, table
/// tab and statistics share a single auxiliary and one slot stays
/// free. With limit 2 the table tab and the statistics fall back on the
/// tree lane instead of failing. With plenty of slots the lanes
/// stay separate, as before. One user per case: the connections of a
/// closed session may stay counted for a moment.
#[tokio::test]
async fn mysql_lanes_fit_user_connection_limit() {
    use alamo_core::schema_info::ConnectionBudget;
    use alamo_driver_mysql::{connection_budget, MySession};
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    run_ok(
        &admin,
        &format!("CREATE TABLE IF NOT EXISTS `{db}`.alamo_lanes (id INT PRIMARY KEY)"),
    )
    .await;
    for (limit, lanes) in [(10u32, 4u32), (3, 2), (2, 2)] {
        let user = format!("alamo_lanes_{limit}");
        run_ok(
            &admin,
            &format!(
                "DROP USER IF EXISTS '{user}'@'%';
                 CREATE USER '{user}'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS {limit};
                 GRANT SELECT ON `{db}`.* TO '{user}'@'%'"
            ),
        )
        .await;
        let limited = format!("mysql://{user}:alamo@{server}/{db}");
        let s = Session::connect(&limited)
            .await
            .expect("limited connection");
        s.schemas().await.expect("tree");
        let (c, emit) = collector();
        s.fetch_table(
            db,
            "alamo_lanes",
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("table tab");
        wait_done(&c).await;
        assert!(
            c.events.lock().unwrap().iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { .. },
                    ..
                }
            )),
            "limit {limit}: the table tab did not read"
        );
        s.schema_stats().await.expect("statistics");
        // The probe takes the slot left free and counts who is there.
        let probe = match MySession::connect_plain(&limited).await {
            Ok(mut p) => connection_budget(&mut p.conn).await.expect("budget"),
            Err(_) => None,
        };
        drop(s);
        run_ok(&admin, &format!("DROP USER IF EXISTS '{user}'@'%'")).await;
        if limit > lanes {
            assert_eq!(
                probe,
                Some(ConnectionBudget {
                    limit,
                    in_use: lanes + 1
                }),
                "limit {limit}"
            );
        } else {
            assert_eq!(probe, None, "limit {limit}: no slot for the probe");
        }
    }
    run_ok(&admin, &format!("DROP TABLE IF EXISTS `{db}`.alamo_lanes")).await;
}

/// User with a single connection (`max_user_connections 1`, or the other
/// slots held by HeidiSQL): the sticky takes it and no lane can open. The
/// tree borrows the sticky between executions (§3.2); during one it fails
/// at once with the advice, instead of waiting for the end of the query.
#[tokio::test]
async fn mysql_tree_borrows_sticky_at_limit_one() {
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    let user = "alamo_lanes_1";
    run_ok(
        &admin,
        &format!(
            "DROP USER IF EXISTS '{user}'@'%';
             CREATE USER '{user}'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 1;
             GRANT SELECT ON `{db}`.* TO '{user}'@'%'"
        ),
    )
    .await;
    let s = Session::connect(&format!("mysql://{user}:alamo@{server}/{db}"))
        .await
        .expect("limited connection");
    let schemas = s.schemas().await.expect("tree on the sticky");
    assert!(schemas.iter().any(|x| x.name == db), "{db} missing");
    s.relations(db).await.expect("tables on the sticky");

    let (c, emit) = collector();
    let exec_id = s
        .execute("SELECT SLEEP(10)".into(), FetchOptions::default(), emit)
        .expect("execute");
    let busy = tokio::time::timeout(Duration::from_secs(2), s.schemas())
        .await
        .expect("the tree must not wait for the end of the query")
        .expect_err("busy sticky");
    assert!(busy.hint.is_some(), "{busy:?}");
    s.cancel(exec_id);
    wait_done(&c).await;
    s.schemas().await.expect("tree after the query");

    drop(s);
    run_ok(&admin, &format!("DROP USER IF EXISTS '{user}'@'%'")).await;
}

/// The calls the UI makes when opening the tree and a table, all together as
/// it makes them (it does not queue them). None must fail or hang.
async fn app_round(s: &Session, db: &str, table: &str) {
    let fetch = async {
        let (c, emit) = collector();
        s.fetch_table(
            db,
            table,
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("table tab");
        wait_done(&c).await;
        assert!(
            c.events.lock().unwrap().iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { .. },
                    ..
                }
            )),
            "the table tab did not read"
        );
    };
    let (schemas, relations, columns, fks, ddl, stats, ()) =
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                s.schemas(),
                s.relations(db),
                s.columns(db, table),
                s.foreign_keys(db, table),
                s.table_ddl(db, table),
                s.schema_stats(),
                fetch,
            )
        })
        .await
        .expect("app round hung");
    schemas.expect("schemas");
    relations.expect("tables");
    columns.expect("columns");
    fks.expect("foreign keys");
    ddl.expect("DDL");
    stats.expect("statistics");
}

/// The `mangeli` case: a user with 3 connections. The app round, twenty
/// times, uses 2 connections and leaves the third free; "Cancel" on a
/// long query works because the `KILL QUERY` finds that slot. Then the
/// same round with another program (HeidiSQL) holding a connection:
/// everything still passes, on the tree connection.
#[tokio::test]
async fn mysql_tight_limit_app_flow() {
    use alamo_core::schema_info::ConnectionBudget;
    use alamo_driver_mysql::{connection_budget, MySession};
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    run_ok(
        &admin,
        &format!(
            "CREATE TABLE IF NOT EXISTS `{db}`.alamo_tight (id INT PRIMARY KEY);
             DROP USER IF EXISTS 'alamo_tight_a'@'%';
             DROP USER IF EXISTS 'alamo_tight_b'@'%';
             CREATE USER 'alamo_tight_a'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 3;
             CREATE USER 'alamo_tight_b'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 3;
             GRANT SELECT ON `{db}`.* TO 'alamo_tight_a'@'%';
             GRANT SELECT ON `{db}`.* TO 'alamo_tight_b'@'%'"
        ),
    )
    .await;

    // Alamo DB only.
    let limited = format!("mysql://alamo_tight_a:alamo@{server}/{db}");
    let s = Session::connect(&limited).await.expect("connection");
    for _ in 0..20 {
        app_round(&s, db, "alamo_tight").await;
    }
    let mut probe = MySession::connect_plain(&limited)
        .await
        .expect("the third slot must stay free");
    assert_eq!(
        connection_budget(&mut probe.conn).await.expect("budget"),
        Some(ConnectionBudget {
            limit: 3,
            in_use: 3
        }),
        "Alamo must use 2 of them"
    );
    let _ = probe.conn.disconnect().await;
    let (c, emit) = collector();
    let exec_id = s
        .execute("SELECT SLEEP(60)".into(), FetchOptions::default(), emit)
        .expect("execute");
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.cancel(exec_id);
    wait_done(&c).await;
    assert!(
        c.events.lock().unwrap().iter().any(|e| matches!(
            e,
            ExecEvent::Finished {
                state: "cancelled",
                ..
            }
        )),
        "Cancel did not stop the query"
    );
    drop(s);

    // HeidiSQL open with the same user.
    let limited = format!("mysql://alamo_tight_b:alamo@{server}/{db}");
    let heidi = MySession::connect_plain(&limited).await.expect("HeidiSQL");
    let s = Session::connect(&limited).await.expect("connection");
    for _ in 0..5 {
        app_round(&s, db, "alamo_tight").await;
    }
    assert!(
        MySession::connect_plain(&limited).await.is_err(),
        "with HeidiSQL Alamo must use 2: slots exhausted"
    );
    drop((s, heidi));

    run_ok(
        &admin,
        &format!(
            "DROP USER IF EXISTS 'alamo_tight_a'@'%';
             DROP USER IF EXISTS 'alamo_tight_b'@'%';
             DROP TABLE IF EXISTS `{db}`.alamo_tight"
        ),
    )
    .await;
}

/// TCP proxy that can "freeze" already open connections: they stop
/// forwarding but stay open, so for the server they are alive and for the
/// client dead — as after a PC sleep or a network change. Their close does
/// not reach the server either. The connections opened after `freeze`
/// pass normally.
struct FreezableProxy {
    port: u16,
    /// Generation of the connections being born now.
    gen: Arc<std::sync::atomic::AtomicU64>,
    /// The connections born up to this generation are frozen.
    frozen: Arc<std::sync::atomic::AtomicU64>,
}

impl FreezableProxy {
    async fn start(target: String) -> FreezableProxy {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let gen = Arc::new(AtomicU64::new(1));
        let frozen = Arc::new(AtomicU64::new(0));
        let (g, f) = (Arc::clone(&gen), Arc::clone(&frozen));
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = tokio::net::TcpStream::connect(&target).await else {
                    continue;
                };
                let born = g.load(Ordering::SeqCst);
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                let pipes: [(
                    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ); 2] = [(Box::new(cr), Box::new(sw)), (Box::new(sr), Box::new(cw))];
                for (mut from, mut to) in pipes {
                    let f = Arc::clone(&f);
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 16 * 1024];
                        loop {
                            let n = match from.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ if f.load(Ordering::SeqCst) >= born => {
                                    std::future::pending::<()>().await;
                                    return;
                                }
                                _ => return,
                            };
                            if f.load(Ordering::SeqCst) >= born {
                                // Frozen: keeps the sockets, no longer forwards.
                                std::future::pending::<()>().await;
                            }
                            if to.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            }
        });
        FreezableProxy { port, gen, frozen }
    }

    fn freeze(&self) {
        use std::sync::atomic::Ordering;
        let g = self.gen.fetch_add(1, Ordering::SeqCst);
        self.frozen.store(g, Ordering::SeqCst);
    }
}

/// Connections of the `url` user (who must have a limit) as seen by the
/// server, zombies included, probe excluded. The `KILL` closes the thread in
/// a moment but not instantly: we wait for `want` for up to 3 s.
async fn server_connections(url: &str, want: u32) -> u32 {
    let mut seen = 0;
    for _ in 0..30 {
        let mut probe = alamo_driver_mysql::MySession::connect_plain(url)
            .await
            .expect("probe");
        let budget = alamo_driver_mysql::connection_budget(&mut probe.conn)
            .await
            .expect("budget")
            .expect("user without a limit");
        let _ = probe.conn.disconnect().await;
        seen = budget.in_use - 1;
        if seen == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    seen
}

/// Connections dead for Alamo but alive for the server (PC asleep, network
/// change, VPN dropped): when reopening them, Alamo closes the old ones on the server.
/// Without that, the zombies stay until `wait_timeout` (8 hours by default) and
/// fill the user's limit: the colleague on Windows could not even get
/// HeidiSQL in any more.
/// The server-life check (an old id after a restart may belong to another
/// live connection) is unit-tested in the driver.
#[tokio::test]
async fn mysql_kill_abandoned_closes_the_old_connection() {
    let url = require_mysql!();
    let tls = alamo_core::tls::TlsOptions::plain();
    let driver = builtin_for_url(&url);
    let fresh = driver
        .connect(&url, &tls, &alamo_session::NetOptions::default())
        .await
        .expect("connection");
    let other = driver
        .connect(&url, &tls, &alamo_session::NetOptions::default())
        .await
        .expect("other connection");
    assert!(other.ping().await);
    fresh
        .kill_abandoned(other.server_conn_id().as_slice())
        .await;
    assert!(
        !other.ping().await,
        "same server life: it should have been closed"
    );
}

#[tokio::test]
async fn mysql_reopen_kills_zombie_connections() {
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    run_ok(
        &admin,
        &format!(
            "DROP USER IF EXISTS 'alamo_zombie'@'%';
             CREATE USER 'alamo_zombie'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 10;
             GRANT SELECT ON `{db}`.* TO 'alamo_zombie'@'%'"
        ),
    )
    .await;
    let direct = format!("mysql://alamo_zombie:alamo@{server}/{db}");
    let proxy = FreezableProxy::start(server.to_string()).await;
    let via = format!("mysql://alamo_zombie:alamo@127.0.0.1:{}/{db}", proxy.port);
    let mut s = Session::connect(&via).await.expect("connection");
    s.ping_after_idle = Duration::ZERO;
    s.schemas().await.expect("tree");
    assert_eq!(server_connections(&direct, 2).await, 2, "editor + tree");

    proxy.freeze();
    // Execute goes through `ensure_connected`: the dead editor reopens.
    let r = s.ensure_connected().await.expect("editor reopen");
    assert!(
        r.reconnected,
        "the frozen editor should have been seen as dead"
    );
    // The dead tree reopens on next use.
    s.schemas().await.expect("tree after the reopen");
    let left = server_connections(&direct, 2).await;
    drop(s);
    run_ok(&admin, "DROP USER IF EXISTS 'alamo_zombie'@'%'").await;
    assert_eq!(left, 2, "zombies left on the server");
}

/// The colleague with 3 connections: after a sleep he disconnected and
/// connected again. The old session's 2 connections stayed on the server
/// (the close never reached it), the new one took the last seat, and the
/// next sleep left nothing to reopen. The app keeps the old session's
/// connections and the next one closes them.
#[tokio::test]
async fn mysql_next_session_closes_the_previous_ones() {
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    run_ok(
        &admin,
        &format!(
            "DROP USER IF EXISTS 'alamo_three'@'%';
             CREATE USER 'alamo_three'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 3;
             GRANT SELECT ON `{db}`.* TO 'alamo_three'@'%'"
        ),
    )
    .await;
    let direct = format!("mysql://alamo_three:alamo@{server}/{db}");
    let proxy = FreezableProxy::start(server.to_string()).await;
    let via = format!("mysql://alamo_three:alamo@127.0.0.1:{}/{db}", proxy.port);
    let s = Session::connect(&via).await.expect("connection");
    s.schemas().await.expect("tree");
    let old = s.server_conn_ids();
    assert_eq!(old.len(), 2, "editor + tree");

    proxy.freeze();
    drop(s);
    assert_eq!(
        server_connections(&direct, 2).await,
        2,
        "zombies after the sleep"
    );

    let s = Session::connect(&via).await.expect("connection again");
    s.kill_abandoned(&old).await;
    let left = server_connections(&direct, 1).await;
    let tree = s.schemas().await;
    drop(s);
    run_ok(&admin, "DROP USER IF EXISTS 'alamo_three'@'%'").await;
    assert_eq!(left, 1, "only the new editor should be left");
    tree.expect("the tree has a seat again");
}

/// TCP proxy that can stop the network and restart it: while paused the
/// open connections hold back the bytes (they deliver them on resume) and
/// new ones are closed immediately — like the network of a Mac just woken up,
/// which only comes back after a few seconds.
struct PausableProxy {
    port: u16,
    paused: tokio::sync::watch::Sender<bool>,
}

impl PausableProxy {
    async fn start(target: String) -> PausableProxy {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (paused, rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                if *rx.borrow() {
                    continue;
                }
                let Ok(server) = tokio::net::TcpStream::connect(&target).await else {
                    continue;
                };
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                let pipes: [(
                    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ); 2] = [(Box::new(cr), Box::new(sw)), (Box::new(sr), Box::new(cw))];
                for (mut from, mut to) in pipes {
                    let mut rx = rx.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 16 * 1024];
                        loop {
                            let n = match from.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => n,
                            };
                            if rx.wait_for(|p| !*p).await.is_err()
                                || to.write_all(&buf[..n]).await.is_err()
                            {
                                return;
                            }
                        }
                    });
                }
            }
        });
        PausableProxy { port, paused }
    }

    fn pause(&self) {
        self.paused.send_replace(true);
    }

    fn resume(&self) {
        self.paused.send_replace(false);
    }
}

/// alpha.3 crash (2026-10-21, after the Mac went to sleep): the ping
/// of `ensure_connected` timed out halfway and the reopen failed because the
/// network was not back yet. The old sticky remained, with the ping response
/// still to be read: from then on every command read the response of the
/// previous one, and a `SELECT DATABASE()` that receives the two columns of
/// another query panics inside mysql_async — with panic=abort the app
/// closes. A connection with an interrupted command is lost: never reused.
#[tokio::test]
async fn mysql_interrupted_ping_does_not_desync_the_sticky() {
    let url = require_mysql!();
    let Some((cred, server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(cred, rest)| rest.split_once('/').map(|(s, db)| (cred, s, db)))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let proxy = PausableProxy::start(server.to_string()).await;
    let via = format!("mysql://{cred}@127.0.0.1:{}/{db}", proxy.port);
    let mut s = Session::connect(&via).await.expect("connection");
    s.ping_after_idle = Duration::ZERO;
    let current = s.current_database().await.expect("current database");
    assert!(current.is_some(), "URL with database");

    proxy.pause();
    assert!(
        s.ensure_connected().await.is_err(),
        "network stopped: the reopen should have failed"
    );
    proxy.resume();

    let lost = s
        .current_database()
        .await
        .expect_err("used a sticky with the ping interrupted halfway");
    assert_eq!(lost.category, alamo_core::error::ErrorCategory::Transport);
    let r = s
        .ensure_connected()
        .await
        .expect("reopen with network back");
    assert!(
        r.reconnected,
        "the out-of-sync sticky should have been replaced"
    );
    run_ok(&s, "SELECT 1 AS a, 2 AS b").await;
    assert_eq!(s.current_database().await.expect("database"), current);
}

#[tokio::test]
async fn mysql_session_executes_batch_with_events() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("MySQL connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT 1 AS uno; SELECT 2 AS due, 'x' AS testo".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let events = c.events.lock().unwrap();
    let row_sets: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, .. },
                ..
            } => Some(*rows),
            _ => None,
        })
        .collect();
    assert_eq!(row_sets, vec![1, 1]);
    assert!(events.iter().any(|e| matches!(
        e,
        ExecEvent::Finished {
            state: "completed",
            ..
        }
    )));
}

#[tokio::test]
async fn mysql_warning_arrives_as_notice() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT CAST('x' AS SIGNED)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(e, ExecEvent::Notice { .. })),
        "the SHOW WARNINGS warning did not become a Notice"
    );
}

#[tokio::test]
async fn mysql_cancel_stops_long_query_as_cancelled() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    let exec_id = session
        .execute("SELECT SLEEP(30)".into(), FetchOptions::default(), emit)
        .expect("execute");
    tokio::time::sleep(Duration::from_millis(400)).await;
    session.cancel(exec_id);
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::Finished {
                state: "cancelled",
                ..
            }
        )),
        "{:?}",
        events
            .iter()
            .filter_map(|e| match e {
                ExecEvent::Finished { state, .. } => Some(*state),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
}

/// MySQL connection busy with another call (e.g. a slow `schema_stats`):
/// `healthy` must report it alive at once, not wait for the lock up to the
/// ping cap and have it reopened. `ping` instead stays the real check.
#[tokio::test]
async fn mysql_busy_connection_is_healthy_without_waiting() {
    let url = require_mysql!();
    let tls = alamo_core::tls::TlsOptions::default();
    let conn = builtin_for_url(&url)
        .connect(&url, &tls, &alamo_session::NetOptions::default())
        .await
        .expect("connection");
    let busy = tokio::spawn({
        let conn = conn.clone();
        async move {
            let (_ctl, mut rx) = tokio::sync::mpsc::channel(1);
            conn.execute_streaming(
                "SELECT SLEEP(2)",
                &FetchOptions::default(),
                &mut rx,
                &mut |_| {},
            )
            .await
            .expect("sleep");
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let healthy = tokio::time::timeout(Duration::from_secs(1), conn.healthy())
        .await
        .expect("healthy() waited for the lock of the busy connection");
    assert!(healthy);
    busy.await.expect("sleep task");
    assert!(conn.healthy().await, "free: the ping must pass");
    assert!(conn.ping().await);
}

#[tokio::test]
async fn mysql_fetch_table_quotes_with_backticks() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    // Fixture with a backtick in the name: wrong quoting would blow up here.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS `tab lella`;
             CREATE TABLE `tab lella` (id INT PRIMARY KEY);
             INSERT INTO `tab lella` VALUES (1),(2)"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let (c, emit) = collector();
    session
        .fetch_table(
            "alamo_bench",
            "tab lella",
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let rows = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, .. },
                    ..
                } => Some(*rows),
                _ => None,
            })
            .expect("no result set from the table tab")
    };
    assert_eq!(rows, 2);

    // Server-side re-query (decision 10, second half): column filter,
    // backtick quoting for identifiers too and MySQL escaping on the value.
    let (c, emit) = collector();
    let query = TableQuery {
        order_by: vec![OrderBy {
            column: "id".into(),
            desc: true,
        }],
        filters: vec![ColumnFilter {
            column: "id".into(),
            op: FilterOp::Ne,
            value: "1".into(),
        }],
        ..Default::default()
    };
    session
        .fetch_table(
            "alamo_bench",
            "tab lella",
            &query,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("filtered fetch_table");
    wait_done(&c).await;
    let rows = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, .. },
                    ..
                } => Some(*rows),
                _ => None,
            })
            .expect("no result set from the re-query")
    };
    assert_eq!(rows, 1, "the id <> 1 filter leaves a single row");
}

#[tokio::test]
async fn mysql_fetch_table_returns_full_first_page() {
    // Regression: table with hundreds of rows (676 = 26x26, like a real
    // distance matrix) — the first page must bring them all,
    // not stop early.
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS page_fixture;
             CREATE TABLE page_fixture (id INT PRIMARY KEY AUTO_INCREMENT, a INT, b INT);
             INSERT INTO page_fixture (a, b)
               WITH RECURSIVE seq (n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 26
               )
               SELECT s1.n, s2.n FROM seq s1 CROSS JOIN seq s2"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let (c, emit) = collector();
    session
        .fetch_table(
            "alamo_bench",
            "page_fixture",
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let (rows, state) = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, state, .. },
                    ..
                } => Some((*rows, *state)),
                _ => None,
            })
            .expect("no result set")
    };
    assert_eq!(
        rows,
        676.min(alamo_session::LimitsIn::default().page_rows),
        "state: {state}"
    );
    assert_eq!(state, "complete");
}

#[tokio::test]
async fn table_more_appends_next_pages_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let schema = session
        .current_database()
        .await
        .expect("current database")
        .expect("no database selected");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS more_fixture;
             CREATE TABLE more_fixture (id INT PRIMARY KEY, label VARCHAR(32));
             INSERT INTO more_fixture
               WITH RECURSIVE seq (n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 50
               )
               SELECT (a.n - 1) * 50 + b.n, CONCAT('row ', (a.n - 1) * 50 + b.n)
               FROM seq a CROSS JOIN seq b"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    page_through_fixture(&session, &schema).await;
}

#[tokio::test]
async fn mysql_fetch_table_wide_types_full_page() {
    // Like the real validation case: 26x26 matrix with DECIMAL,
    // DATETIME and VARCHAR — not just integers.
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS dist_fixture;
             CREATE TABLE dist_fixture (
               id INT PRIMARY KEY AUTO_INCREMENT,
               scenario_id INT NOT NULL,
               origin_location_id INT NOT NULL,
               dest_location_id INT NOT NULL,
               distance_km DECIMAL(10,2) NOT NULL,
               time_min INT NOT NULL,
               routing_provider VARCHAR(32) NOT NULL,
               calculated_at DATETIME NOT NULL
             );
             INSERT INTO dist_fixture
               (scenario_id, origin_location_id, dest_location_id, distance_km, time_min, routing_provider, calculated_at)
               WITH RECURSIVE seq (n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 26
               )
               SELECT 2, s1.n, s2.n, s1.n * 1.37 + s2.n, s1.n + s2.n, 'haversine', NOW()
               FROM seq s1 CROSS JOIN seq s2"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let (c, emit) = collector();
    session
        .fetch_table(
            "alamo_bench",
            "dist_fixture",
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    let (rows, state) = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { rows, state, .. },
                ..
            } => Some((*rows, *state)),
            _ => None,
        })
        .expect("no result set");
    assert_eq!(
        (rows, state),
        (
            676.min(alamo_session::LimitsIn::default().page_rows),
            "complete"
        ),
        "events: {events:?}"
    );
}

#[tokio::test]
async fn mysql_current_database_reflects_use() {
    // The context badge in the toolbar reads from here: after a USE it must show the
    // new database, not the one from the connect.
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    assert_eq!(
        session.current_database().await.expect("current_database"),
        Some("alamo_bench".into())
    );

    let (c, emit) = collector();
    session
        .execute("USE mysql".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    assert_eq!(
        session.current_database().await.expect("current_database"),
        Some("mysql".into())
    );

    // The select in the toolbar goes through use_database: same effect as a
    // hand-written `USE`.
    session
        .use_database("alamo_bench")
        .await
        .expect("use_database");
    assert_eq!(
        session.current_database().await.expect("current_database"),
        Some("alamo_bench".into())
    );
}

#[tokio::test]
async fn schema_stats_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let stats = session.schema_stats().await.expect("schema_stats");
    let names: Vec<_> = session
        .schemas()
        .await
        .expect("schemas")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        stats.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        names
    );
    let sys = stats.iter().find(|s| s.name == "mysql").unwrap();
    assert!(sys.is_system);
    assert!(sys.tables > 10, "{sys:?}");
    assert!(sys.size_bytes.unwrap_or(0) > 0, "{sys:?}");
    assert!(sys.charset.is_some() && sys.collation.is_some());
    // Empty database: zero tables, unknown size — not an error.
    let bench = stats.iter().find(|s| s.name == "alamo_bench").unwrap();
    assert!(!bench.is_system);
}

#[tokio::test]
async fn foreign_keys_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    for sql in [
        "DROP TABLE IF EXISTS fk_riga, fk_ordine, fk_cliente",
        "CREATE TABLE fk_cliente (id int PRIMARY KEY)",
        "CREATE TABLE fk_ordine (anno int, num int, cliente_id int NOT NULL,
             PRIMARY KEY (anno, num),
             CONSTRAINT fk_ordine_cliente FOREIGN KEY (cliente_id) REFERENCES fk_cliente(id))",
        "CREATE TABLE fk_riga (id int PRIMARY KEY, num int, anno int,
             CONSTRAINT fk_riga_ordine FOREIGN KEY (anno, num) REFERENCES fk_ordine(anno, num))",
    ] {
        run_setup(&session, sql).await;
    }
    let fks = session
        .foreign_keys("alamo_bench", "fk_ordine")
        .await
        .expect("foreign_keys");
    assert_eq!(fks.len(), 2, "{fks:?}");
    let out = fks.iter().find(|f| f.name == "fk_ordine_cliente").unwrap();
    assert_eq!(out.ref_schema, "alamo_bench");
    assert_eq!(
        (out.table.as_str(), out.ref_table.as_str()),
        ("fk_ordine", "fk_cliente")
    );
    assert_eq!(out.columns, ["cliente_id"]);
    assert_eq!(out.ref_columns, ["id"]);
    let inc = fks.iter().find(|f| f.name == "fk_riga_ordine").unwrap();
    assert_eq!(inc.table, "fk_riga");
    assert_eq!(inc.columns, ["anno", "num"]);
    assert_eq!(inc.ref_columns, ["anno", "num"]);
    run_setup(&session, "DROP TABLE fk_riga, fk_ordine, fk_cliente").await;
}

#[tokio::test]
async fn catalog_rows_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let long = "x".repeat(4000);
    let rows = session
        .catalog_rows(&format!(
            "SELECT 1, NULL, '{long}', CAST('{long}' AS BINARY)"
        ))
        .await
        .expect("catalog_rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_deref(), Some("1"));
    assert_eq!(rows[0][1], None);
    assert_eq!(rows[0][2].as_deref(), Some(long.as_str()));
    let err = session.catalog_rows("SELECT * FROM no_such_table").await;
    assert!(err.is_err(), "a SQL error is an error");
}

/// One connection only, the sticky's: the read shares it, as the tree does.
#[tokio::test]
async fn catalog_rows_with_one_connection_mysql() {
    let url = require_mysql!();
    let Some((server, db)) = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .and_then(|(_, rest)| rest.split_once('/'))
    else {
        eprintln!("skipped: URL without credentials or without database");
        return;
    };
    let admin = Session::connect(&url).await.expect("admin connection");
    run_ok(
        &admin,
        &format!(
            "DROP USER IF EXISTS 'alamo_one'@'%';
             CREATE USER 'alamo_one'@'%' IDENTIFIED BY 'alamo' WITH MAX_USER_CONNECTIONS 1;
             GRANT SELECT ON `{db}`.* TO 'alamo_one'@'%'"
        ),
    )
    .await;
    let limited = format!("mysql://alamo_one:alamo@{server}/{db}");
    let s = Session::connect(&limited).await.expect("connection");
    let rows = s.catalog_rows("SELECT 1").await.expect("catalog_rows");
    assert_eq!(rows[0][0].as_deref(), Some("1"));
}

#[tokio::test]
async fn mysql_introspection_and_ddl() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let schemas = session.schemas().await.expect("schemas");
    assert!(schemas
        .iter()
        .any(|s| s.name == "alamo_bench" && !s.is_system));
    assert!(schemas.iter().any(|s| s.name == "mysql" && s.is_system));

    let (c, emit) = collector();
    session
        .execute(
            "CREATE TABLE IF NOT EXISTS intro_fixture (
                 id BIGINT NOT NULL PRIMARY KEY,
                 label TEXT
             )"
            .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let rels = session.relations("alamo_bench").await.expect("relations");
    assert!(rels.iter().any(|r| r.name == "intro_fixture"));
    let cols = session
        .columns("alamo_bench", "intro_fixture")
        .await
        .expect("columns");
    assert_eq!(cols[0].name, "id");
    assert!(cols[0].is_primary_key);
    assert!(!cols[0].nullable);

    let ddl = session
        .table_ddl("alamo_bench", "intro_fixture")
        .await
        .expect("ddl");
    assert!(ddl.contains("CREATE TABLE"), "{ddl}");
    assert!(ddl.contains("intro_fixture"), "{ddl}");

    let auto = session
        .autocomplete_catalog(None)
        .await
        .expect("autocomplete");
    assert!(auto
        .iter()
        .any(|(s, t, c)| s == "alamo_bench" && t == "intro_fixture" && c == "label"));
    // A single database: its columns and nothing else.
    let one = session
        .autocomplete_catalog(Some("alamo_bench"))
        .await
        .expect("autocomplete of one database");
    assert!(one
        .iter()
        .any(|(_, t, c)| t == "intro_fixture" && c == "label"));
    assert!(one.iter().all(|(s, _, _)| s == "alamo_bench"));
    let none = session
        .autocomplete_catalog(Some("does_not_exist"))
        .await
        .expect("nonexistent database");
    assert!(none.is_empty());
}

// ---------------------------------------------------------------- M4 editing

#[tokio::test]
async fn table_update_updates_one_row_by_pk() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    // Throwaway fixture: the UPDATE must not touch the bench datasets.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_edit;
             CREATE TABLE m4_edit (id INT PRIMARY KEY, nome TEXT);
             INSERT INTO m4_edit VALUES (1, 'uno'), (2, 'due')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    let set = [CellAssign {
        column: "nome".into(),
        value: Some("l'uno".into()),
        default: false,
        numeric: false,
    }];
    let key = [CellAssign {
        column: "id".into(),
        value: Some("1".into()),
        default: false,
        numeric: false,
    }];
    // Preview and execution come from the same generation.
    let sql = session
        .update_sql("public", "m4_edit", &set, &key)
        .expect("preview");
    assert_eq!(
        sql,
        "UPDATE \"public\".\"m4_edit\" SET \"nome\" = 'l''uno' WHERE \"id\" = '1'"
    );

    let (c, emit) = collector();
    session
        .update_table(
            "public",
            "m4_edit",
            &set,
            &key,
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("update_table");
    wait_done(&c).await;
    {
        let events = c.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::Affected { rows: 1 },
                    ..
                }
            )),
            "expected Affected {{ rows: 1 }}"
        );
    }

    // The new value is read back from the server (with the quote intact).
    let (c, emit) = collector();
    session
        .fetch_table(
            "public",
            "m4_edit",
            &TableQuery {
                order_by: vec![OrderBy {
                    column: "id".into(),
                    desc: false,
                }],
                filters: vec![],
                ..Default::default()
            },
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema never arrived")
    };
    let live = session.result(rid).expect("result set in the registry");
    let w = live.window(0..1, 0..2);
    assert_eq!(w.columns[1].values[0], "l'uno");

    // Never an UPDATE without WHERE: empty key rejected before the server.
    assert!(session.update_sql("public", "m4_edit", &set, &[]).is_err());
}

/// Saving from the grid without reload: after the batch the saved
/// rows are read back by PK and replace those in the result set — with the
/// value in the server's form, with the changed PK, and the vanished row
/// reported as missing.
async fn refresh_rows_after_batch(session: &Session, schema: &str) {
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_refresh;
             CREATE TABLE m4_refresh (id INT PRIMARY KEY, nome VARCHAR(50), n INT DEFAULT 7);
             INSERT INTO m4_refresh VALUES (1, 'uno', 1), (2, 'due', 2), (3, 'tre', 3)"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let (c, emit) = collector();
    session
        .fetch_table(
            schema,
            "m4_refresh",
            &TableQuery {
                order_by: vec![OrderBy {
                    column: "id".into(),
                    desc: false,
                }],
                filters: vec![],
                ..Default::default()
            },
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let (rid, _) = table_result(&c);

    let a = |c: &str, v: Option<&str>| CellAssign {
        column: c.into(),
        value: v.map(Into::into),
        default: false,
        numeric: false,
    };
    let updates = vec![
        RowUpdate {
            set: vec![a("n", Some("0042"))],
            key: vec![a("id", Some("2"))],
        },
        RowUpdate {
            set: vec![a("id", Some("30")), a("nome", None)],
            key: vec![a("id", Some("3"))],
        },
        RowUpdate {
            set: vec![CellAssign {
                column: "n".into(),
                value: None,
                default: true,
                numeric: false,
            }],
            key: vec![a("id", Some("1"))],
        },
    ];
    let out = session
        .update_table_batch(schema, "m4_refresh", &updates)
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    let r = session
        .refresh_table_rows(schema, "m4_refresh", rid, &updates)
        .await
        .expect("refresh");
    assert_eq!((r.refreshed, r.missing), (3, 0));
    let live = session.result(rid).expect("result set in the registry");
    let w = live.window(0..3, 0..3);
    assert_eq!(w.columns[1].values[0], "uno", "untouched column intact");
    assert_eq!(
        w.columns[2].values[0], "7",
        "SET DEFAULT: the value chosen by the server"
    );
    assert_eq!(w.columns[2].values[1], "42", "server form, not '0042'");
    assert_eq!(w.columns[0].values[2], "30", "changed PK read back");
    assert!(w.columns[1].nulls[2], "NULL read back as NULL");

    let gone = vec![RowUpdate {
        set: vec![a("n", Some("1"))],
        key: vec![a("id", Some("99"))],
    }];
    let r = session
        .refresh_table_rows(schema, "m4_refresh", rid, &gone)
        .await
        .expect("refresh");
    assert_eq!((r.refreshed, r.missing), (0, 1));
}

#[tokio::test]
async fn refresh_rows_after_batch_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    refresh_rows_after_batch(&session, "public").await;
}

#[tokio::test]
async fn row_pages_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    row_pages_then_all(
        &session,
        "WITH RECURSIVE seq (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 200)
         SELECT a.n, b.n FROM seq a CROSS JOIN seq b",
        40_000,
    )
    .await;
}

#[tokio::test]
async fn row_pause_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    // 200 x 200 without deep recursion (cte_max_recursion_depth = 1000).
    row_pause_then_resume(
        &session,
        "WITH RECURSIVE seq (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 200)
         SELECT a.n, b.n FROM seq a CROSS JOIN seq b",
        40_000,
    )
    .await;
}

#[tokio::test]
async fn refresh_rows_after_batch_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    refresh_rows_after_batch(&session, "alamo_bench").await;
}

/// MySQL BIT: the grid shows 0/1 and the user types 0/1 back. The UPDATE
/// and the INSERT must send the number bare (`numeric`), `'1'` is "Data
/// too long"; the key keeps the quoted form, which compares fine.
#[tokio::test]
async fn bit_columns_write_as_numbers_mysql() {
    use alamo_session::TableWrite;
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_bit;
             CREATE TABLE m4_bit (id INT PRIMARY KEY, flag BIT(1), wide BIT(64));
             INSERT INTO m4_bit VALUES (1, b'0', 0), (2, b'1', 1)"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let a = |c: &str, v: &str, numeric: bool| CellAssign {
        column: c.into(),
        value: Some(v.into()),
        default: false,
        numeric,
    };
    let writes = vec![
        TableWrite::Update {
            set: vec![
                a("flag", "1", true),
                a("wide", "18446744073709551615", true),
            ],
            key: vec![a("id", "1", false)],
        },
        TableWrite::Insert {
            values: vec![
                a("id", "3", false),
                a("flag", "1", true),
                a("wide", "256", true),
            ],
        },
        // Key on the BIT column itself, quoted: the comparison converts.
        TableWrite::Update {
            set: vec![a("wide", "2", true)],
            key: vec![a("id", "2", false), a("flag", "1", false)],
        },
    ];
    let sql = session
        .write_sql_batch("alamo_bench", "m4_bit", &writes)
        .expect("preview");
    assert!(
        sql.contains("SET `flag` = 1, `wide` = 18446744073709551615 WHERE `id` = '1'"),
        "{sql}"
    );
    assert!(sql.contains("VALUES ('3', 1, 256)"), "{sql}");
    let out = session
        .write_table_batch("alamo_bench", "m4_bit", &writes, None)
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(
        scalar(
            &session,
            "SELECT GROUP_CONCAT(CONCAT(id, ':', flag + 0, ':', wide + 0) ORDER BY id) FROM m4_bit"
        )
        .await,
        "1:1:18446744073709551615,2:1:2,3:1:256"
    );

    // The quoted form in SET is the bug the flag fixes: still an error.
    let quoted = vec![TableWrite::Update {
        set: vec![a("flag", "1", false)],
        key: vec![a("id", "1", false)],
    }];
    let out = session
        .write_table_batch("alamo_bench", "m4_bit", &quoted, None)
        .await
        .expect("batch");
    assert!(
        out.error.is_some(),
        "'1' into a BIT must fail: the fix is the bare number"
    );
}

/// Editor connection killed by the server (like a network drop):
/// `ensure_connected` notices, reopens it, says that the transaction with
/// writes was lost, and the next query works — without closing and
/// reopening the session. On a live connection it reopens nothing.
async fn reconnect_after_kill(
    mut session: Session,
    killer: Session,
    id_sql: &str,
    kill_sql: impl Fn(&str) -> String,
    write_sql: &str,
    select_sql: &str,
) {
    session.ping_after_idle = Duration::ZERO;
    // Id of the editor connection, to kill it from outside.
    let (c, emit) = collector();
    session
        .execute(id_sql.into(), FetchOptions::default(), emit)
        .expect("execute id");
    wait_done(&c).await;
    let (rid, _) = table_result(&c);
    let backend_id = session.result(rid).unwrap().window(0..1, 0..1).columns[0].values[0].clone();
    let ok = session.ensure_connected().await.expect("alive");
    assert!(!ok.reconnected, "live connection: no reopen");

    // Transaction open with a write, then the sticky dies.
    let (c, emit) = collector();
    session
        .execute(write_sql.into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    assert!(session.txn_state().has_writes, "write in the transaction");
    // Killed by another session, like a drop: on the client side the
    // transaction still appears open with writes.
    let (c, emit) = collector();
    killer
        .execute(kill_sql(&backend_id), FetchOptions::default(), emit)
        .expect("kill");
    wait_done(&c).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let rc = session.ensure_connected().await.expect("reconnection");
    assert!(rc.reconnected, "dead connection: must be reopened");
    assert!(rc.lost_txn && rc.lost_writes, "{rc:?}");
    assert!(!session.txn_state().open, "no transaction on the new one");

    let (c, emit) = collector();
    session
        .execute(select_sql.into(), FetchOptions::default(), emit)
        .expect("execute after the reconnection");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::RowSet { .. },
                ..
            }
        )),
        "the SELECT after the reconnection must answer: {events:?}"
    );
}

#[tokio::test]
async fn reconnect_after_kill_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_rc; CREATE TABLE m4_rc (id INT PRIMARY KEY)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let killer = Session::connect(&url).await.expect("killer connection");
    reconnect_after_kill(
        session,
        killer,
        "SELECT pg_backend_pid()",
        |id| format!("SELECT pg_terminate_backend({id})"),
        "BEGIN; INSERT INTO m4_rc VALUES (1)",
        "SELECT 1",
    )
    .await;
}

#[tokio::test]
async fn reconnect_after_kill_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_rc; CREATE TABLE m4_rc (id INT PRIMARY KEY)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let killer = Session::connect(&url).await.expect("killer connection");
    reconnect_after_kill(
        session,
        killer,
        "SELECT CONNECTION_ID()",
        |id| format!("KILL {id}"),
        "START TRANSACTION; INSERT INTO m4_rc VALUES (1)",
        "SELECT 1",
    )
    .await;
}

#[tokio::test]
async fn mysql_table_update_by_pk_and_null() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_edit;
             CREATE TABLE m4_edit (id INT PRIMARY KEY, nota TEXT);
             INSERT INTO m4_edit VALUES (1, 'a\\\\b'), (2, 'due')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    // NULL in the SET and backticks in the quoting: the full MySQL path.
    let (c, emit) = collector();
    session
        .update_table(
            "alamo_bench",
            "m4_edit",
            &[CellAssign {
                column: "nota".into(),
                value: None,
                default: false,
                numeric: false,
            }],
            &[CellAssign {
                column: "id".into(),
                value: Some("2".into()),
                default: false,
                numeric: false,
            }],
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("update_table");
    wait_done(&c).await;
    {
        let events = c.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::Affected { rows: 1 },
                    ..
                }
            )),
            "expected Affected {{ rows: 1 }}"
        );
    }

    // Read back from the server: row 2 now has nota IS NULL.
    let (c, emit) = collector();
    session
        .fetch_table(
            "alamo_bench",
            "m4_edit",
            &TableQuery {
                filters: vec![ColumnFilter {
                    column: "nota".into(),
                    op: FilterOp::IsNull,
                    value: String::new(),
                }],
                ..Default::default()
            },
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let rows = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::RowSet { rows, .. },
                    ..
                } => Some(*rows),
                _ => None,
            })
            .expect("no result set")
    };
    assert_eq!(rows, 1);
}

// ------------------------------------------------------- M4 transactions §9.7

#[tokio::test]
async fn pg_prod_implicit_begin_real_rollback_and_commit() {
    let url = require_url!();
    // Prod profile: auto-commit off — every execution opens a transaction.
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");
    assert!(session.production());
    assert!(!session.txn_state().open);

    // A read opens the transaction (implicit BEGIN) but does not enable
    // Commit (§9.7.2: tell read-only transactions apart).
    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let st = session.txn_state();
    assert!(st.open, "implicit BEGIN on prod");
    assert!(!st.has_writes, "the SELECT does not enable Commit");
    assert!(st.open_for_ms.is_some());

    // DDL inside the transaction (on PG it is transactional): a write.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_txn; CREATE TABLE m4_txn (id INT PRIMARY KEY)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    assert!(session.txn_state().has_writes, "DDL = write");

    // Explicit rollback: the table must not exist.
    let st = session.rollback().await.expect("rollback");
    assert!(!st.open);
    let (c, emit) = collector();
    session
        .execute("SELECT * FROM m4_txn".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    {
        let events = c.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::Failed { .. },
                    ..
                }
            )),
            "the rollback must have discarded the CREATE"
        );
    }

    // The failed transaction is still open (aborted): a rollback is needed
    // before redoing. Then a real commit and the table survives.
    assert!(
        session.txn_state().open,
        "open even after the error (aborted)"
    );
    session.rollback().await.expect("post-error rollback");
    let (c, emit) = collector();
    session
        .execute(
            "CREATE TABLE m4_txn (id INT PRIMARY KEY)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let st = session.commit().await.expect("commit");
    assert!(!st.open);
    let (c, emit) = collector();
    session
        .execute("DROP TABLE m4_txn".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    session.commit().await.expect("commit of the drop");
}

#[tokio::test]
async fn pg_non_prod_tracks_user_begin_commit() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute("BEGIN; SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    assert!(session.txn_state().open, "user BEGIN tracked");
    let (c, emit) = collector();
    session
        .execute("COMMIT".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    assert!(!session.txn_state().open);
}

#[tokio::test]
async fn pg_prod_update_table_joins_sticky_transaction() {
    let url = require_url!();
    // Fixture prepared OUTSIDE the prod profile, so it stays committed.
    let setup = Session::connect(&url).await.expect("setup connection");
    let (c, emit) = collector();
    setup
        .execute(
            "DROP TABLE IF EXISTS m4_edit_prod;
             CREATE TABLE m4_edit_prod (id INT PRIMARY KEY, nome TEXT);
             INSERT INTO m4_edit_prod VALUES (1, 'orig')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;

    // §9.7: on prod the grid UPDATE runs on the sticky, opens the
    // transaction (implicit BEGIN) and does NOT commit by itself.
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("prod connection");
    let (c, emit) = collector();
    session
        .update_table(
            "public",
            "m4_edit_prod",
            &[CellAssign {
                column: "nome".into(),
                value: Some("edited".into()),
                default: false,
                numeric: false,
            }],
            &[CellAssign {
                column: "id".into(),
                value: Some("1".into()),
                default: false,
                numeric: false,
            }],
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("update_table");
    wait_done(&c).await;
    {
        let events = c.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ExecEvent::StatementFinished {
                    outcome: OutcomeOut::Affected { rows: 1 },
                    ..
                }
            )),
            "expected Affected {{ rows: 1 }}: {events:?}"
        );
    }
    let st = session.txn_state();
    assert!(
        st.open,
        "the prod UPDATE opens and does not commit by itself"
    );
    assert!(st.has_writes, "the UPDATE enables Commit");

    // Rollback: the original value must survive. It is read back from the
    // shared connection (auto-commit), which only sees what is committed.
    session.rollback().await.expect("rollback");
    let (c, emit) = collector();
    session
        .fetch_table(
            "public",
            "m4_edit_prod",
            &Default::default(),
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema never arrived")
    };
    let live = session.result(rid).expect("result set in the registry");
    let w = live.window(0..1, 0..2);
    assert_eq!(
        w.columns[1].values[0], "orig",
        "the rollback must have discarded the UPDATE"
    );
}

#[tokio::test]
async fn pg_prod_terminate_resets_transaction_state() {
    let url = require_url!();
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");
    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    assert!(session.txn_state().open, "implicit BEGIN on prod");

    // §7.2: after the terminate the server-side session no longer exists — the
    // UI must not keep showing the open transaction.
    let killed = session.terminate().await.expect("terminate");
    assert!(killed, "pg_terminate_backend must succeed locally");
    assert!(
        !session.txn_state().open,
        "the transaction is lost with the backend"
    );
}

#[tokio::test]
async fn mysql_prod_server_flag_and_ddl_implicit_commit() {
    let url = require_mysql!();
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");

    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let st = session.txn_state();
    assert!(st.open, "implicit BEGIN, confirmed by the server flag");
    assert!(!st.has_writes);

    // §9.7.1: DDL on MySQL commits implicitly — the server flag
    // says "outside transaction" without any COMMIT having been sent.
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_txn_my; CREATE TABLE m4_txn_my (id INT PRIMARY KEY)".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let st = session.txn_state();
    assert!(!st.open, "implicit commit of the DDL, seen by the server");

    // Cleanup (the DROP opens and closes by itself: DDL).
    let (c, emit) = collector();
    session
        .execute("DROP TABLE m4_txn_my".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
}

#[tokio::test]
async fn mysql_prod_idle_transaction_rolled_back() {
    let url = require_mysql!();
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");
    let run = |sql: &str| {
        let (c, emit) = collector();
        session
            .execute(sql.into(), FetchOptions::default(), emit)
            .expect("execute");
        c
    };
    // DDL commits implicitly (§9.7.1): the table exists outside the transaction.
    wait_done(&run(
        "DROP TABLE IF EXISTS idle_rb_my; CREATE TABLE idle_rb_my (id INT PRIMARY KEY)",
    ))
    .await;
    wait_done(&run("INSERT INTO idle_rb_my VALUES (1)")).await;
    assert!(session.txn_state().has_writes);
    // The log lists it with its rows, for the Commit dialog.
    let log = session.txn_log();
    assert_eq!(log.total, 1);
    assert_eq!(log.writes[0].sql, "INSERT INTO idle_rb_my VALUES (1)");
    assert_eq!(log.writes[0].rows, Some(1));

    let long = std::time::Duration::from_secs(3600);
    assert!(
        session
            .rollback_if_idle(long)
            .await
            .expect("check")
            .is_none(),
        "not idle long enough: untouched"
    );
    let before = session
        .rollback_if_idle(std::time::Duration::ZERO)
        .await
        .expect("rollback")
        .expect("idle transaction rolled back");
    assert!(before.has_writes, "the lost writes are reported");
    assert!(!session.txn_state().open);

    let c = run("SELECT COUNT(*) FROM idle_rb_my");
    wait_done(&c).await;
    let rid = c
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("schema never arrived");
    let w = session.result(rid).expect("result set").window(0..1, 0..1);
    assert_eq!(w.columns[0].values[0], "0", "the INSERT was rolled back");
    session.rollback().await.expect("rollback");
    wait_done(&run("DROP TABLE idle_rb_my")).await;
}

#[tokio::test]
async fn mysql_manual_commit_switch_on_non_prod() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let run = |sql: &str| {
        let (c, emit) = collector();
        session
            .execute(sql.into(), FetchOptions::default(), emit)
            .expect("execute");
        c
    };
    wait_done(&run(
        "DROP TABLE IF EXISTS manual_my; CREATE TABLE manual_my (id INT PRIMARY KEY)",
    ))
    .await;
    assert!(
        !session.manual_commit(),
        "auto-commit by default outside prod"
    );

    session.set_manual_commit(true).expect("switch on");
    wait_done(&run("INSERT INTO manual_my VALUES (1)")).await;
    let st = session.txn_state();
    assert!(st.open && st.has_writes, "implicit BEGIN, like prod");
    assert!(
        session.set_manual_commit(false).is_err(),
        "no switch with a transaction open"
    );
    session.rollback().await.expect("rollback");

    session.set_manual_commit(false).expect("switch off");
    wait_done(&run("INSERT INTO manual_my VALUES (2)")).await;
    assert!(!session.txn_state().open, "auto-commit again");

    let c = run("SELECT GROUP_CONCAT(id) FROM manual_my");
    wait_done(&c).await;
    let rid = c
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("schema never arrived");
    let w = session.result(rid).expect("result set").window(0..1, 0..1);
    assert_eq!(
        w.columns[0].values[0], "2",
        "row 1 rolled back, row 2 committed"
    );
    wait_done(&run("DROP TABLE manual_my")).await;

    let prod = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("prod connection");
    assert!(prod.manual_commit());
    assert!(prod.set_manual_commit(false).is_err(), "prod stays manual");
}

// -------------------------------------------- M4 §9.7: read-only, EXPLAIN

#[tokio::test]
async fn pg_read_only_blocks_client_and_server() {
    let url = require_url!();
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        false,
        true,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");
    assert!(session.read_only());

    // Certain write: the classifier stops it, before touching the server.
    let err = session
        .execute(
            "UPDATE bench_narrow SET id = 1 WHERE id = 1".into(),
            FetchOptions::default(),
            |_| {},
        )
        .expect_err("the write must be rejected");
    assert!(err.message.contains("read-only"), "{}", err.message);

    // Grid editing is switched off at the root.
    let err = session
        .update_table(
            "public",
            "bench_narrow",
            &[CellAssign {
                column: "id".into(),
                value: Some("1".into()),
                default: false,
                numeric: false,
            }],
            &[CellAssign {
                column: "id".into(),
                value: Some("1".into()),
                default: false,
                numeric: false,
            }],
            FetchOptions::default(),
            |_| {},
        )
        .await
        .expect_err("update_table must be rejected");
    assert!(err.message.contains("read-only"), "{}", err.message);

    // Disarming the server net (SET/RESET on the variable, BEGIN READ
    // WRITE) is class Control but is stopped at the client gate anyway: the
    // double net must not be disableable from user SQL.
    for sql in [
        "SET default_transaction_read_only = off",
        "BEGIN READ WRITE",
        "RESET ALL",
    ] {
        let err = session
            .execute(sql.into(), FetchOptions::default(), |_| {})
            .expect_err("the disarm must be rejected");
        assert!(
            err.message.contains("would disable"),
            "{sql}: {}",
            err.message
        );
    }

    // Reads pass normally.
    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;

    // Unknown that writes (DO): the classifier lets it through, the server
    // net stops it (session-level read-only transactions).
    let (c, emit) = collector();
    session
        .execute(
            "DO $$ BEGIN CREATE TABLE ro_smoke (id INT); END $$".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("the command starts: the server is the one rejecting");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Failed { .. },
                ..
            }
        )),
        "the server must reject the write in a read-only session"
    );
}

#[tokio::test]
async fn mysql_read_only_blocks_writes() {
    let url = require_mysql!();
    // If the SET of the read-only variable failed (wrong name for the
    // engine), the connect would fail: success here IS the
    // MySQL/MariaDB compatibility test.
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        false,
        true,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("read-only connection");

    let err = session
        .execute(
            "DELETE FROM alamo_smoke_ro".into(),
            FetchOptions::default(),
            |_| {},
        )
        .expect_err("the write must be rejected");
    assert!(err.message.contains("read-only"), "{}", err.message);

    // Disarming the server net: blocked at the client gate here too
    // (transaction_read_only on MySQL, tx_read_only on MariaDB).
    for sql in [
        "SET SESSION transaction_read_only = 0",
        "SET SESSION tx_read_only = 0",
        "START TRANSACTION READ WRITE",
    ] {
        let err = session
            .execute(sql.into(), FetchOptions::default(), |_| {})
            .expect_err("the disarm must be rejected");
        assert!(
            err.message.contains("would disable"),
            "{sql}: {}",
            err.message
        );
    }

    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
}

#[tokio::test]
async fn pg_explain_estimate_estimates_and_degrades() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    // A SELECT with a plan: the estimate is always there.
    let est = session
        .explain_estimate("SELECT * FROM generate_series(1, 100)")
        .await
        .expect("explain");
    assert!(est.is_some_and(|n| n > 0), "estimate expected: {est:?}");
    // Multi-statement batch: no estimate by contract.
    assert_eq!(
        session
            .explain_estimate("SELECT 1; SELECT 2")
            .await
            .unwrap(),
        None
    );
    // DDL: not explainable, None without even trying.
    assert_eq!(
        session
            .explain_estimate("CREATE TABLE x (id INT)")
            .await
            .unwrap(),
        None
    );
    // Unknown that EXPLAIN rejects: degrades to None, never an error.
    assert_eq!(
        session
            .explain_estimate("frobnicate the database")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn mysql_explain_estimate_on_table() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_explain;
             CREATE TABLE m4_explain (id INT PRIMARY KEY);
             INSERT INTO m4_explain VALUES (1),(2),(3)"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("fixture");
    wait_done(&c).await;

    let est = session
        .explain_estimate("SELECT * FROM m4_explain")
        .await
        .expect("explain");
    assert!(est.is_some_and(|n| n >= 1), "estimate expected: {est:?}");
    // Point UPDATE on the PK: the plan estimates one row — the grid
    // editing case.
    let est = session
        .explain_estimate("UPDATE m4_explain SET id = 5 WHERE id = 1")
        .await
        .expect("explain");
    assert_eq!(est, Some(1));

    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE m4_explain".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("cleanup");
    wait_done(&c).await;
}

/// First cell of the first result set of `sql`.
async fn scalar(session: &Session, sql: &str) -> String {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .expect("scalar");
    wait_done(&c).await;
    let result_id = c
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("no result set");
    let live = session
        .result(result_id)
        .expect("result set in the registry");
    live.window(0..1, 0..1).columns[0].values[0].clone()
}

/// The estimate runs BEFORE the user's confirmation: whatever is
/// passed to it, it must never execute the statement.
#[tokio::test]
async fn pg_explain_estimate_never_executes() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS sec_explain;
         CREATE TABLE sec_explain (id INT PRIMARY KEY);
         INSERT INTO sec_explain SELECT generate_series(1, 5)",
    )
    .await;
    for sql in [
        // `EXPLAIN (ANALYZE) DELETE …` executes the DELETE.
        "(ANALYZE) DELETE FROM sec_explain",
        // Dollar-quote opened inside an identifier: the splitter sees a single
        // statement, the server three.
        "WITH a$q$ AS (SELECT 1) SELECT 1; DELETE FROM sec_explain; WITH b$q$ AS (SELECT 1) SELECT 1",
        "/* /* */ SELECT */ (ANALYZE) DELETE FROM sec_explain",
    ] {
        let est = session.explain_estimate(sql).await.expect("explain");
        assert_eq!(est, None, "{sql}");
        assert_eq!(scalar(&session, "SELECT count(*) FROM sec_explain").await, "5", "{sql}");
    }
    // The use case remains: estimating a DELETE without executing it.
    let est = session
        .explain_estimate("DELETE FROM sec_explain WHERE id = 1")
        .await
        .expect("explain");
    assert!(est.is_some(), "estimate expected");
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM sec_explain").await,
        "5"
    );
    run_setup(&session, "DROP TABLE sec_explain").await;
}

#[tokio::test]
async fn mysql_explain_estimate_never_executes() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS sec_explain;
         CREATE TABLE sec_explain (id INT PRIMARY KEY);
         INSERT INTO sec_explain VALUES (1),(2),(3),(4),(5)",
    )
    .await;
    for sql in [
        // `--1` is not a comment for MySQL: the splitter sees one
        // statement, the server two.
        "SELECT 1 --1; DELETE FROM sec_explain",
        "SELECT 1 /* /* */; DELETE FROM sec_explain; /* */",
        // Executable comment: it would become `EXPLAIN ANALYZE DELETE`.
        "/*!ANALYZE */ DELETE s FROM sec_explain s, (SELECT 1) x",
    ] {
        let est = session.explain_estimate(sql).await.expect("explain");
        assert_eq!(est, None, "{sql}");
        assert_eq!(
            scalar(&session, "SELECT count(*) FROM sec_explain").await,
            "5",
            "{sql}"
        );
    }
    let est = session
        .explain_estimate("DELETE FROM sec_explain WHERE id = 1")
        .await
        .expect("explain");
    assert_eq!(est, Some(1));
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM sec_explain").await,
        "5"
    );
    run_setup(&session, "DROP TABLE sec_explain").await;
}

#[tokio::test]
async fn pg_prod_sets_idle_txn_timeout() {
    let url = require_url!();
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT current_setting('idle_in_transaction_session_timeout')".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema never arrived")
    };
    let live = session.result(rid).expect("result set");
    let w = live.window(0..1, 0..1);
    assert_eq!(
        w.columns[0].values[0], "20min",
        "IDLE_TXN_TIMEOUT_SECS (1200s) set on prod connect"
    );
    // Closes the transaction opened by the implicit BEGIN.
    session.rollback().await.expect("rollback");
}

#[tokio::test]
async fn mysql_prod_idle_timeout_mariadb_only() {
    let url = require_mysql!();
    // The SET is best-effort: on MySQL (without the variable) the connect passes
    // anyway — that is exactly the regression test.
    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("prod connection");
    let (c, emit) = collector();
    session
        .execute(
            "SELECT @@idle_transaction_timeout".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let events = c.events.lock().unwrap();
    let failed = events.iter().any(|e| {
        matches!(
            e,
            ExecEvent::StatementFinished {
                outcome: OutcomeOut::Failed { .. },
                ..
            }
        )
    });
    if failed {
        // MySQL: the variable does not exist and the timeout is a declared no-op.
        return;
    }
    let rid = events
        .iter()
        .find_map(|e| match e {
            ExecEvent::Schema { result_id, .. } => Some(*result_id),
            _ => None,
        })
        .expect("schema never arrived");
    drop(events);
    let live = session.result(rid).expect("result set");
    let w = live.window(0..1, 0..1);
    assert_eq!(
        w.columns[0].values[0], "1200",
        "MariaDB: timeout in seconds"
    );
}

#[tokio::test]
async fn mysql_explain_estimate_degrades_when_sticky_busy() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    let exec_id = session
        .execute("SELECT SLEEP(10)".into(), FetchOptions::default(), emit)
        .expect("execute");
    // Busy sticky: the estimate degrades AT ONCE to None instead of hanging
    // on the MySQL session mutex until the end of the query (the timeout here is the
    // regression: without the check it would wait the 10s of the SLEEP).
    let est = tokio::time::timeout(
        Duration::from_secs(2),
        session.explain_estimate("SELECT * FROM qualunque"),
    )
    .await
    .expect("the estimate must not wait for the end of the query")
    .expect("explain");
    assert_eq!(est, None);
    session.cancel(exec_id);
    wait_done(&c).await;
}

// ------------------------------------------------ M4 multi-cell batch (dec. 12)

#[tokio::test]
async fn pg_batch_update_atomic_on_shared() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_batch;
             CREATE TABLE m4_batch (id INT PRIMARY KEY, nome TEXT, note TEXT NOT NULL DEFAULT '');
             INSERT INTO m4_batch VALUES (1, 'a', ''), (2, 'b', ''), (3, 'c', '')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("fixture");
    wait_done(&c).await;

    let upd = |id: &str, col: &str, v: Option<&str>| alamo_session::RowUpdate {
        set: vec![CellAssign {
            column: col.into(),
            value: v.map(Into::into),
            default: false,
            numeric: false,
        }],
        key: vec![CellAssign {
            column: "id".into(),
            value: Some(id.into()),
            default: false,
            numeric: false,
        }],
    };

    // Success: two UPDATEs, atomic, already committed (normal profile).
    let out = session
        .update_table_batch(
            "public",
            "m4_batch",
            &[upd("1", "nome", Some("A")), upd("2", "nome", None)],
        )
        .await
        .expect("batch");
    assert_eq!(out.applied, vec![1, 1]);
    assert!(out.error.is_none());
    assert!(!out.in_transaction && !out.rolled_back);

    // Error halfway (NOT NULL on the second): all or nothing — the first
    // UPDATE must not survive the ROLLBACK.
    let out = session
        .update_table_batch(
            "public",
            "m4_batch",
            &[upd("3", "nome", Some("NEVER")), upd("1", "note", None)],
        )
        .await
        .expect("batch with error");
    assert_eq!(out.applied, vec![1], "the first one had passed");
    assert_eq!(out.error_index, Some(1));
    assert!(out.error.is_some());
    assert!(out.rolled_back, "atomic on the shared connection");

    // Check: nome of id=3 intact, id=1/2 updated by the first batch.
    let (c, emit) = collector();
    session
        .execute(
            "SELECT nome FROM m4_batch ORDER BY id".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("check");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema")
    };
    let live = session.result(rid).expect("result");
    let w = live.window(0..3, 0..1);
    assert_eq!(w.columns[0].values, vec!["A", "", "c"]);
    assert_eq!(w.columns[0].nulls, vec![false, true, false]);

    let (c, emit) = collector();
    session
        .execute("DROP TABLE m4_batch".into(), FetchOptions::default(), emit)
        .expect("cleanup");
    wait_done(&c).await;
}

#[tokio::test]
async fn pg_batch_update_prod_stays_in_transaction() {
    let url = require_url!();
    // Fixture outside the prod profile, so it stays committed.
    let setup = Session::connect(&url).await.expect("setup");
    let (c, emit) = collector();
    setup
        .execute(
            "DROP TABLE IF EXISTS m4_batch_prod;
             CREATE TABLE m4_batch_prod (id INT PRIMARY KEY, nome TEXT);
             INSERT INTO m4_batch_prod VALUES (1, 'orig1'), (2, 'orig2')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("fixture");
    wait_done(&c).await;

    let session = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        true,
        false,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("prod connection");
    let upd = |id: &str, v: &str| alamo_session::RowUpdate {
        set: vec![CellAssign {
            column: "nome".into(),
            value: Some(v.into()),
            default: false,
            numeric: false,
        }],
        key: vec![CellAssign {
            column: "id".into(),
            value: Some(id.into()),
            default: false,
            numeric: false,
        }],
    };
    let out = session
        .update_table_batch("public", "m4_batch_prod", &[upd("1", "X"), upd("2", "Y")])
        .await
        .expect("batch prod");
    assert_eq!(out.applied, vec![1, 1]);
    assert!(out.in_transaction, "prod: inside the sticky's transaction");
    let st = session.txn_state();
    assert!(st.open && st.has_writes, "implicit BEGIN + writes");

    // Rollback: the original values survive (the commit was up to the user).
    session.rollback().await.expect("rollback");
    let (c, emit) = collector();
    setup
        .execute(
            "SELECT nome FROM m4_batch_prod ORDER BY id".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("check");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema")
    };
    let live = setup.result(rid).expect("result");
    let w = live.window(0..2, 0..1);
    assert_eq!(w.columns[0].values, vec!["orig1", "orig2"]);

    let (c, emit) = collector();
    setup
        .execute(
            "DROP TABLE m4_batch_prod".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("cleanup");
    wait_done(&c).await;
}

#[tokio::test]
async fn mysql_batch_update_atomic() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_batch_my;
             CREATE TABLE m4_batch_my (id INT PRIMARY KEY, nome TEXT);
             INSERT INTO m4_batch_my VALUES (1, 'a'), (2, 'b')"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("fixture");
    wait_done(&c).await;

    let upd = |id: &str, v: &str| alamo_session::RowUpdate {
        set: vec![CellAssign {
            column: "nome".into(),
            value: Some(v.into()),
            default: false,
            numeric: false,
        }],
        key: vec![CellAssign {
            column: "id".into(),
            value: Some(id.into()),
            default: false,
            numeric: false,
        }],
    };
    let out = session
        .update_table_batch(
            "alamo_bench",
            "m4_batch_my",
            &[upd("1", "A"), upd("2", "B")],
        )
        .await
        .expect("batch");
    assert_eq!(out.applied, vec![1, 1]);
    assert!(!out.in_transaction && !out.rolled_back);

    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE m4_batch_my".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("cleanup");
    wait_done(&c).await;
}

// ------------------------------------------------ M4 new rows (INSERT in the batch)

/// Reads a text column of the whole table, in `id` order.
async fn read_column(session: &Session, sql: &str) -> (Vec<String>, Vec<bool>) {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .expect("check");
    wait_done(&c).await;
    let rid = {
        let events = c.events.lock().unwrap();
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Schema { result_id, .. } => Some(*result_id),
                _ => None,
            })
            .expect("schema")
    };
    let live = session.result(rid).expect("result");
    let n = live.rows();
    let w = live.window(0..n, 0..1);
    (w.columns[0].values.clone(), w.columns[0].nulls.clone())
}

fn cell(column: &str, value: Option<&str>) -> CellAssign {
    CellAssign {
        column: column.into(),
        value: value.map(Into::into),
        default: false,
        numeric: false,
    }
}

fn cell_default(column: &str) -> CellAssign {
    CellAssign {
        column: column.into(),
        value: None,
        default: true,
        numeric: false,
    }
}

#[tokio::test]
async fn pg_mixed_batch_update_then_insert_atomic() {
    use alamo_session::TableWrite;
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_ins;
         CREATE TABLE m4_ins (id SERIAL PRIMARY KEY, nome TEXT NOT NULL, n INT NOT NULL DEFAULT 7);
         INSERT INTO m4_ins (nome) VALUES ('a'), ('b')",
    )
    .await;

    // The preview and the execution come from the same generation: the
    // UPDATEs first, the INSERTs after, regardless of the order received.
    let writes = vec![
        TableWrite::Insert {
            values: vec![
                cell_default("id"),
                cell("nome", Some("c")),
                cell_default("n"),
            ],
        },
        TableWrite::Update {
            set: vec![cell("nome", Some("A"))],
            key: vec![cell("id", Some("1"))],
        },
        TableWrite::Insert { values: vec![] },
    ];
    let sql = session
        .write_sql_batch("public", "m4_ins", &writes)
        .expect("preview");
    assert_eq!(
        sql,
        "UPDATE \"public\".\"m4_ins\" SET \"nome\" = 'A' WHERE \"id\" = '1';\n\
         INSERT INTO \"public\".\"m4_ins\" (\"id\", \"nome\", \"n\") VALUES (DEFAULT, 'c', DEFAULT);\n\
         INSERT INTO \"public\".\"m4_ins\" DEFAULT VALUES"
    );
    let out = session
        .write_table_batch("public", "m4_ins", &writes, None)
        .await
        .expect("batch");
    // The third (all default) violates the NOT NULL on `nome`: ROLLBACK of
    // everything, the error index follows the execution order.
    assert_eq!(out.applied, vec![1, 1], "{:?}", out.error);
    assert_eq!(out.error_index, Some(2));
    assert!(out.rolled_back);
    let (names, _) = read_column(&session, "SELECT nome FROM m4_ins ORDER BY id").await;
    assert_eq!(names, vec!["a", "b"], "all or nothing");

    let out = session
        .write_table_batch("public", "m4_ins", &writes[..2], None)
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1, 1]);
    let (names, _) = read_column(&session, "SELECT nome FROM m4_ins ORDER BY id").await;
    assert_eq!(names, vec!["A", "b", "c"]);
    let (n, _) = read_column(&session, "SELECT n FROM m4_ins WHERE nome = 'c'").await;
    assert_eq!(
        n,
        vec!["7"],
        "explicit DEFAULT: the server chooses the value"
    );

    // Repeated column: rejected upfront, nothing is sent.
    let dup = vec![TableWrite::Insert {
        values: vec![cell("nome", Some("x")), cell("nome", Some("y"))],
    }];
    assert!(session
        .write_table_batch("public", "m4_ins", &dup, None)
        .await
        .is_err());

    run_setup(&session, "DROP TABLE m4_ins").await;
}

#[tokio::test]
async fn mysql_mixed_batch_insert_all_default_row() {
    use alamo_session::TableWrite;
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_ins_my;
         CREATE TABLE m4_ins_my (id INT AUTO_INCREMENT PRIMARY KEY, nome TEXT NULL, n INT NOT NULL DEFAULT 3);
         INSERT INTO m4_ins_my (nome) VALUES ('a')",
    )
    .await;
    let writes = vec![
        TableWrite::Insert { values: vec![] },
        TableWrite::Insert {
            values: vec![cell("nome", None), cell("n", Some("5"))],
        },
    ];
    let sql = session
        .write_sql_batch("test", "m4_ins_my", &writes)
        .expect("preview");
    assert!(
        sql.starts_with("INSERT INTO `test`.`m4_ins_my` () VALUES ();\n"),
        "{sql}"
    );
    let db = session
        .current_database()
        .await
        .expect("db")
        .expect("current database");
    let out = session
        .write_table_batch(&db, "m4_ins_my", &writes, None)
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1, 1]);
    let (n, nulls) = read_column(&session, "SELECT n FROM m4_ins_my ORDER BY id").await;
    assert_eq!(n, vec!["3", "3", "5"]);
    assert_eq!(nulls, vec![false, false, false]);
    let (_, nulls) = read_column(&session, "SELECT nome FROM m4_ins_my ORDER BY id").await;
    assert_eq!(nulls, vec![false, true, true]);
    run_setup(&session, "DROP TABLE m4_ins_my").await;
}

/// Opens the table (first page, ordered by `id`) and returns the result_id.
async fn open_table(session: &Session, schema: &str, table: &str) -> u64 {
    let (c, emit) = collector();
    session
        .fetch_table(
            schema,
            table,
            &TableQuery {
                order_by: vec![OrderBy {
                    column: "id".into(),
                    desc: false,
                }],
                ..Default::default()
            },
            FetchOptions::default(),
            emit,
        )
        .await
        .expect("fetch_table");
    wait_done(&c).await;
    table_result(&c).0
}

#[tokio::test]
async fn pg_insert_appends_reread_row_to_result_set() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_app;
         CREATE TABLE m4_app (id SERIAL PRIMARY KEY, nome TEXT NOT NULL, n INT NOT NULL DEFAULT 7);
         INSERT INTO m4_app (nome) VALUES ('a'), ('b')",
    )
    .await;
    let rid = open_table(&session, "public", "m4_app").await;
    let live = session.result(rid).expect("result set");
    assert_eq!(live.rows(), 2);
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![],
    };
    let writes = vec![
        TableWrite::Update {
            set: vec![cell("nome", Some("A"))],
            key: vec![cell("id", Some("1"))],
        },
        TableWrite::Insert {
            values: vec![cell("nome", Some("c"))],
        },
        TableWrite::Insert {
            values: vec![cell("nome", Some("d")), cell("n", Some("9"))],
        },
    ];
    let out = session
        .write_table_batch("public", "m4_app", &writes, Some(&append))
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1, 1, 1], "RETURNING counts as 1 row");
    assert_eq!(out.appended, 2);
    assert_eq!(live.rows(), 4, "the two new rows are at the end");
    let w = live.window(2..4, 0..3);
    assert_eq!(
        w.columns[0].values,
        vec!["3", "4"],
        "id from the server (serial)"
    );
    assert_eq!(w.columns[1].values, vec!["c", "d"]);
    assert_eq!(
        w.columns[2].values,
        vec!["7", "9"],
        "server default and given value"
    );

    // Error halfway on the shared connection: ROLLBACK, nothing appended.
    let bad = vec![
        TableWrite::Insert {
            values: vec![cell("nome", Some("e"))],
        },
        TableWrite::Insert { values: vec![] },
    ];
    let out = session
        .write_table_batch("public", "m4_app", &bad, Some(&append))
        .await
        .expect("batch");
    assert!(out.rolled_back);
    assert_eq!(out.appended, 0);
    assert_eq!(live.rows(), 4);
    run_setup(&session, "DROP TABLE m4_app").await;
}

#[tokio::test]
async fn mysql_insert_appends_reread_row_with_last_insert_id() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_app_my;
         CREATE TABLE m4_app_my (id INT AUTO_INCREMENT PRIMARY KEY, nome TEXT NULL, n INT NOT NULL DEFAULT 3);
         INSERT INTO m4_app_my (nome) VALUES ('a')",
    )
    .await;
    let db = session
        .current_database()
        .await
        .expect("db")
        .expect("current database");
    let rid = open_table(&session, &db, "m4_app_my").await;
    let live = session.result(rid).expect("result set");
    assert_eq!(live.rows(), 1);
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![],
    };
    let writes = vec![
        // auto_increment: read back with LAST_INSERT_ID()
        TableWrite::Insert {
            values: vec![cell("nome", Some("b"))],
        },
        // key given by the user: read back by key
        TableWrite::Insert {
            values: vec![
                cell("id", Some("10")),
                cell("nome", None),
                cell("n", Some("5")),
            ],
        },
    ];
    let out = session
        .write_table_batch(&db, "m4_app_my", &writes, Some(&append))
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1, 1]);
    assert_eq!(out.appended, 2);
    assert_eq!(live.rows(), 3);
    let w = live.window(1..3, 0..3);
    assert_eq!(w.columns[0].values, vec!["2", "10"]);
    assert_eq!(w.columns[1].values[0], "b");
    assert!(w.columns[1].nulls[1]);
    assert_eq!(w.columns[2].values, vec!["3", "5"]);
    run_setup(&session, "DROP TABLE m4_app_my").await;
}

#[tokio::test]
async fn pg_delete_removes_rows_from_view_and_next_page_stays_correct() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    // Two and a half pages: the first is full, the rest comes with `more`.
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_del;
         CREATE TABLE m4_del (id SERIAL PRIMARY KEY, nome TEXT NOT NULL);
         INSERT INTO m4_del (nome) SELECT 'r' || g FROM generate_series(1, 2500) g",
    )
    .await;
    let rid = open_table(&session, "public", "m4_del").await;
    let live = session.result(rid).expect("result set");
    let page = live.rows();
    assert!(page < 2500, "an incomplete first page is needed: {page}");
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![],
    };
    let del = |id: &str| TableWrite::Delete {
        key: vec![cell("id", Some(id))],
    };
    let sql = session
        .write_sql_batch(
            "public",
            "m4_del",
            &[
                TableWrite::Insert {
                    values: vec![cell("nome", Some("new"))],
                },
                del("2"),
            ],
        )
        .expect("preview");
    assert!(
        sql.starts_with("DELETE FROM \"public\".\"m4_del\" WHERE \"id\" = '2';\n"),
        "{sql}"
    );

    let writes = vec![
        del("2"),
        del("3"),
        TableWrite::Insert {
            values: vec![cell("nome", Some("new"))],
        },
    ];
    let out = session
        .write_table_batch("public", "m4_del", &writes, Some(&append))
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1, 1, 1]);
    assert_eq!((out.removed, out.appended), (2, 1));
    assert_eq!(out.rows, Some(page - 2 + 1));
    let w = live.window(0..3, 0..2);
    assert_eq!(
        w.columns[0].values,
        vec!["1", "4", "5"],
        "2 and 3 out of the view"
    );
    // The local sort does not bring them back.
    live.sort(0, true);
    let w = live.window(page - 2..page - 1, 0..1);
    assert_eq!(w.columns[0].values, vec!["1"]);
    live.clear_sort();

    // Next page: OFFSET = server rows still there (page - 2), not the
    // physical rows (page + 1). The new row (id 2501) arrives at the end.
    let more = session
        .fetch_table_more(
            "public",
            "m4_del",
            &TableQuery {
                order_by: vec![OrderBy {
                    column: "id".into(),
                    desc: false,
                }],
                ..Default::default()
            },
            rid,
        )
        .await
        .expect("next page");
    let n = live.rows();
    let w = live.window(0..n, 0..1);
    let ids: Vec<u64> = w.columns[0]
        .values
        .iter()
        .map(|v| v.parse().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "no duplicate row");
    let next_first = ids[(page - 2 + 1) as usize];
    assert_eq!(next_first, page + 1, "no row skipped after page 1");
    assert_eq!(more.rows, n);
    run_setup(&session, "DROP TABLE m4_del").await;
}

#[tokio::test]
async fn mysql_delete_removes_row_from_view() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_del_my;
         CREATE TABLE m4_del_my (id INT AUTO_INCREMENT PRIMARY KEY, nome TEXT);
         INSERT INTO m4_del_my (nome) VALUES ('a'), ('b'), ('c')",
    )
    .await;
    let db = session
        .current_database()
        .await
        .expect("db")
        .expect("current database");
    let rid = open_table(&session, &db, "m4_del_my").await;
    let live = session.result(rid).expect("result set");
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![],
    };
    let out = session
        .write_table_batch(
            &db,
            "m4_del_my",
            &[TableWrite::Delete {
                key: vec![cell("id", Some("2"))],
            }],
            Some(&append),
        )
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.applied, vec![1]);
    assert_eq!((out.removed, out.rows), (1, Some(2)));
    let w = live.window(0..2, 1..2);
    assert_eq!(w.columns[0].values, vec!["a", "c"]);
    let (left, _) = read_column(&session, "SELECT nome FROM m4_del_my ORDER BY id").await;
    assert_eq!(left, vec!["a", "c"]);
    run_setup(&session, "DROP TABLE m4_del_my").await;
}

#[tokio::test]
async fn mysql_inserted_rows_stay_where_the_grid_showed_them() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_at_my;
         CREATE TABLE m4_at_my (id INT AUTO_INCREMENT PRIMARY KEY, nome TEXT);
         INSERT INTO m4_at_my (nome) VALUES ('a'), ('b'), ('c'), ('d'), ('e')",
    )
    .await;
    let db = session
        .current_database()
        .await
        .expect("db")
        .expect("current database");
    let rid = open_table(&session, &db, "m4_at_my").await;
    let live = session.result(rid).expect("result set");
    // The grid showed: [N1, 1, 2 (to delete), 3, N2, 4, 5].
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![0, 4],
    };
    let writes = vec![
        TableWrite::Delete {
            key: vec![cell("id", Some("2"))],
        },
        TableWrite::Insert {
            values: vec![cell("nome", Some("N1"))],
        },
        TableWrite::Insert {
            values: vec![cell("nome", Some("N2"))],
        },
    ];
    let out = session
        .write_table_batch(&db, "m4_at_my", &writes, Some(&append))
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!((out.removed, out.appended, out.rows), (1, 2, Some(6)));
    let w = live.window(0..6, 1..2);
    assert_eq!(
        w.columns[0].values,
        vec!["N1", "a", "c", "N2", "d", "e"],
        "the new rows where they were shown, the deleted one gone"
    );
    assert_eq!(live.cell_full(3, 0).as_deref(), Some("7"));
    run_setup(&session, "DROP TABLE m4_at_my").await;
}

#[tokio::test]
async fn mysql_next_page_does_not_repeat_inserted_rows() {
    use alamo_session::{AppendTo, TableWrite};
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS m4_dup_my;
         CREATE TABLE m4_dup_my (id INT AUTO_INCREMENT PRIMARY KEY, nome TEXT);
         INSERT INTO m4_dup_my (id, nome)
           WITH RECURSIVE n(i) AS (SELECT 10 UNION ALL SELECT i + 1 FROM n WHERE i < 609)
           SELECT i, CONCAT('r', i) FROM n",
    )
    .await;
    let db = session
        .current_database()
        .await
        .expect("db")
        .expect("current database");
    let rid = open_table(&session, &db, "m4_dup_my").await;
    let live = session.result(rid).expect("result set");
    let page = live.rows();
    assert_eq!(page, 500, "first page: 10..509");
    // Two rows inserted near the top: id 5 sorts before the rows already
    // read (the server's rows shift by one), the next id (610) after them.
    let append = AppendTo {
        result_id: rid,
        key_columns: vec!["id".into()],
        at: vec![3, 4],
    };
    let writes = vec![
        TableWrite::Insert {
            values: vec![cell("id", Some("5")), cell("nome", Some("before"))],
        },
        TableWrite::Insert {
            values: vec![cell("nome", Some("after"))],
        },
    ];
    let out = session
        .write_table_batch(&db, "m4_dup_my", &writes, Some(&append))
        .await
        .expect("batch");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.appended, 2);
    let more = session
        .fetch_table_more(
            &db,
            "m4_dup_my",
            &TableQuery {
                order_by: vec![OrderBy {
                    column: "id".into(),
                    desc: false,
                }],
                ..Default::default()
            },
            rid,
        )
        .await
        .expect("next page");
    let n = live.rows();
    let ids: Vec<u64> = live.window(0..n, 0..1).columns[0]
        .values
        .iter()
        .map(|v| v.parse().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "no row twice");
    assert_eq!(n, 602, "5, 10..609 and 610, each once");
    assert_eq!(more.rows, n);
    assert_eq!(
        &ids[3..5],
        &[5, 610],
        "the inserted rows where they were added"
    );
    assert_eq!(*ids.last().unwrap(), 609);
    run_setup(&session, "DROP TABLE m4_dup_my").await;
}

// ------------------------------------------------------------------ TLS (§12)

/// `prefer` takes what the server offers: the plain PG container has no
/// TLS and the session goes on in plaintext without noticing; the TLS
/// target (lab CA set) answers with an encrypted channel. Either way the
/// session reports the truth.
#[tokio::test]
async fn tls_prefer_degrades_on_server_without_tls() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!();
    let tls = TlsOptions {
        mode: TlsMode::Prefer,
        root_cert: None,
        tls_server_name: None,
    };
    let session = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
        .await
        .expect("prefer must degrade to plaintext, not fail");
    let (c, emit) = collector();
    session
        .execute("SELECT 1".into(), FetchOptions::default(), emit)
        .expect("execution");
    c.done.notified().await;
    let server_has_tls = std::env::var_os("ALAMO_TEST_TLS_CA").is_some();
    let info = session.tls_info().await.unwrap();
    assert_eq!(info.is_some(), server_has_tls, "{info:?}");
    assert!(!info.is_some_and(|i| i.verified), "prefer never verifies");
}

/// `require` against a server without TLS: the connect MUST fail — the
/// plaintext downgrade is exactly what require rules out. Not applicable to
/// the TLS target.
#[tokio::test]
async fn tls_require_fails_on_server_without_tls() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!();
    if std::env::var_os("ALAMO_TEST_TLS_CA").is_some() {
        eprintln!("skipped: the server has TLS");
        return;
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{sep}sslmode=require");
    let tls = TlsOptions {
        mode: TlsMode::Require,
        root_cert: None,
        tls_server_name: None,
    };
    assert!(
        Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
            .await
            .is_err(),
        "require passed in plaintext on a server without TLS"
    );
}

/// Trojan in the `search_path` (CVE-2018-1058): whoever can create objects in a
/// schema on the connecting user's path puts a `count(oid)` there, more
/// specific than `pg_catalog`'s `count("any")`, and the server picks theirs. Catalog queries start by themselves when the tree opens: they must
/// never resolve to those objects.
#[tokio::test]
async fn pg_introspection_ignores_trojan_in_search_path() {
    let url = require_url!();
    let _list = SCHEMA_LIST.lock().await;
    let setup = Session::connect(&url).await.expect("connection");
    run_setup(
        &setup,
        "DROP SCHEMA IF EXISTS sec_trojan CASCADE;
         CREATE SCHEMA sec_trojan;
         CREATE TABLE sec_trojan.vittima (id INT PRIMARY KEY, nome TEXT);
         CREATE FUNCTION sec_trojan.boom_agg(bigint, oid) RETURNS bigint
             LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'trojan executed'; END$$;
         CREATE AGGREGATE sec_trojan.count(oid)
             (sfunc = sec_trojan.boom_agg, stype = bigint, initcond = '0')",
    )
    .await;
    // The victim has the attacker's schema in the path.
    let sep = if url.contains('?') { '&' } else { '?' };
    let victim = format!("{url}{sep}options=-c%20search_path%3Dsec_trojan%2Cpublic");
    let session = Session::connect(&victim).await.expect("victim connection");

    session.schemas().await.expect("schemas");
    session.schema_stats().await.expect("schema_stats");
    session.relations("sec_trojan").await.expect("relations");
    session
        .columns("sec_trojan", "vittima")
        .await
        .expect("columns");
    session
        .foreign_keys("sec_trojan", "vittima")
        .await
        .expect("foreign_keys");
    session
        .table_ddl("sec_trojan", "vittima")
        .await
        .expect("table_ddl");
    session
        .autocomplete_catalog(None)
        .await
        .expect("autocomplete");
    // The EXPLAIN estimate of user SQL still resolves names with the user's
    // path: `vittima` lives only in sec_trojan.
    let est = session
        .explain_estimate("DELETE FROM vittima WHERE id = 1")
        .await
        .expect("explain");
    assert!(
        est.is_some(),
        "estimate expected with the user's search_path"
    );
    // And after the estimate the catalog lane is still locked down.
    session
        .schema_stats()
        .await
        .expect("schema_stats after explain");

    run_setup(&setup, "DROP SCHEMA sec_trojan CASCADE").await;
}

/// A statement that the splitter believes single but the server reads as two
/// is not executed: the driver has the server confirm it with prepare. The
/// splitter cannot know the `sql_mode` (here `NO_BACKSLASH_ESCAPES`).
#[tokio::test]
async fn mysql_hidden_statements_not_executed() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    run_setup(
        &session,
        "DROP TABLE IF EXISTS sec_hidden;
         CREATE TABLE sec_hidden (id INT PRIMARY KEY, nota TEXT);
         INSERT INTO sec_hidden (id) VALUES (1),(2),(3),(4),(5)",
    )
    .await;
    for sql in [
        // Executable comment: the splitter keeps it whole.
        "SELECT 1 /*! ; DELETE FROM sec_hidden */",
        // With NO_BACKSLASH_ESCAPES `\'` closes the string: for the server
        // a DELETE follows, for the splitter it is all literal.
        "SET SESSION sql_mode = CONCAT(@@sql_mode, ',NO_BACKSLASH_ESCAPES');
         SELECT 'a\\'; DELETE FROM sec_hidden; -- '",
    ] {
        run_setup(&session, sql).await;
        assert_eq!(
            scalar(&session, "SELECT count(*) FROM sec_hidden").await,
            "5",
            "{sql}"
        );
    }
    run_setup(&session, "SET SESSION sql_mode = DEFAULT").await;
    // A `;` inside a literal stays allowed.
    run_setup(&session, "INSERT INTO sec_hidden VALUES (6, 'a;b')").await;
    assert_eq!(
        scalar(&session, "SELECT nota FROM sec_hidden WHERE id = 6").await,
        "a;b"
    );
    run_setup(&session, "DROP TABLE sec_hidden").await;
}

/// Runs `sql` and waits for it to finish; `Err` = rejected by the client gate.
async fn try_run(session: &Session, sql: &str) -> Result<(), String> {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .map_err(|e| e.message.clone())?;
    wait_done(&c).await;
    Ok(())
}

/// Read-only mode cannot be disabled from user SQL, not even with what
/// the classifier cannot read: `set_config` inside a SELECT, a
/// `DO`, the variable name in double quotes. Every execution runs in
/// an app READ ONLY transaction and cannot leave it.
#[tokio::test]
async fn pg_read_only_cannot_be_disarmed() {
    let url = require_url!();
    let setup = Session::connect(&url).await.expect("connection");
    run_setup(
        &setup,
        "DROP TABLE IF EXISTS sec_ro;
         CREATE TABLE sec_ro (id INT PRIMARY KEY);
         INSERT INTO sec_ro SELECT generate_series(1, 5)",
    )
    .await;
    let ro = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        false,
        true,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("read-only connection");
    let delete = "WITH d AS (DELETE FROM sec_ro RETURNING 1) SELECT count(*) FROM d";
    for disarm in [
        "SELECT set_config('default_transaction_read_only', 'off', false)",
        "DO $$BEGIN PERFORM set_config('default_transaction_read_only', 'off', false); END$$",
        "SELECT set_config('transaction_read_only', 'off', false)",
    ] {
        // Separate batches and in the same batch.
        let _ = try_run(&ro, disarm).await;
        let _ = try_run(&ro, delete).await;
        let _ = try_run(&ro, &format!("{disarm}; {delete}")).await;
        assert_eq!(
            scalar(&setup, "SELECT count(*) FROM sec_ro").await,
            "5",
            "{disarm}"
        );
    }
    // The quoted name is the same variable.
    assert!(try_run(&ro, "SET \"default_transaction_read_only\" = off")
        .await
        .is_err());
    // Leaving the app transaction is not allowed.
    for sql in [
        "COMMIT",
        "ROLLBACK",
        "END",
        "BEGIN",
        "PREPARE TRANSACTION 'x'",
    ] {
        let err = try_run(&ro, sql).await.expect_err(sql);
        assert!(err.contains("read-only"), "{sql}: {err}");
    }
    // Reads and harmless SETs remain: the SET survives the batch.
    try_run(&ro, "SET application_name = 'alamo_ro_test'")
        .await
        .expect("SET");
    assert_eq!(
        scalar(&ro, "SELECT current_setting('application_name')").await,
        "alamo_ro_test"
    );
    run_setup(&setup, "DROP TABLE sec_ro").await;
}

/// MySQL: the equivalent of `set_config` is dynamic SQL, and the variable
/// name can be written in backticks.
#[tokio::test]
async fn mysql_read_only_cannot_be_disarmed() {
    let url = require_mysql!();
    let setup = Session::connect(&url).await.expect("connection");
    run_setup(
        &setup,
        "DROP TABLE IF EXISTS sec_ro;
         CREATE TABLE sec_ro (id INT PRIMARY KEY);
         INSERT INTO sec_ro VALUES (1),(2),(3),(4),(5)",
    )
    .await;
    let ro = Session::connect_opts(
        builtin_for_url(&url),
        &url,
        false,
        true,
        alamo_core::tls::TlsOptions::plain(),
    )
    .await
    .expect("read-only connection");
    for sql in [
        "SET SESSION `transaction_read_only` = 0",
        "SET SESSION `tx_read_only` = 0",
        "PREPARE s FROM 'SET SESSION transaction_read_only = 0'",
        "EXECUTE s",
    ] {
        let err = try_run(&ro, sql).await.expect_err(sql);
        assert!(err.contains("read-only"), "{sql}: {err}");
    }
    // Unknown that writes: the server stops it.
    let _ = try_run(&ro, "WITH x AS (SELECT 1) DELETE FROM sec_ro").await;
    assert_eq!(scalar(&setup, "SELECT count(*) FROM sec_ro").await, "5");
    run_setup(&setup, "DROP TABLE sec_ro").await;
}

/// With `standard_conforming_strings = off` the backslash in normal
/// literals escapes: a key `\' OR true --` closed the string and
/// the grid UPDATE touched all the rows. Values with backslashes
/// must also stay the real ones.
#[tokio::test]
async fn pg_safe_literals_with_standard_conforming_strings_off() {
    let url = require_url!();
    let setup = Session::connect(&url).await.expect("connection");
    run_setup(
        &setup,
        r"DROP TABLE IF EXISTS sec_scs;
         CREATE TABLE sec_scs (id TEXT PRIMARY KEY, saldo INT);
         INSERT INTO sec_scs VALUES ('a', 100), ('b', 100), (E'x\\y', 100)",
    )
    .await;
    let sep = if url.contains('?') { '&' } else { '?' };
    let legacy = format!("{url}{sep}options=-c%20standard_conforming_strings%3Doff");
    let session = Session::connect(&legacy).await.expect("legacy connection");
    let set = [CellAssign {
        column: "saldo".into(),
        value: Some("0".into()),
        default: false,
        numeric: false,
    }];
    for (id, expected) in [
        (r"\' OR true --", "a=100,b=100,x\\y=100"),
        (r"x\y", "a=100,b=100,x\\y=0"),
    ] {
        let key = [CellAssign {
            column: "id".into(),
            value: Some(id.into()),
            default: false,
            numeric: false,
        }];
        let (c, emit) = collector();
        session
            .update_table(
                "public",
                "sec_scs",
                &set,
                &key,
                FetchOptions::default(),
                emit,
            )
            .await
            .expect("update");
        wait_done(&c).await;
        assert_eq!(
            scalar(
                &setup,
                "SELECT string_agg(id || '=' || saldo, ',' ORDER BY id) FROM sec_scs"
            )
            .await,
            expected,
            "{id}"
        );
    }
    run_setup(&setup, "DROP TABLE sec_scs").await;
}

/// The UI asks the channel what protects it: in plaintext `None`, with TLS
/// the protocol and cipher the server names. The MySQL 8.x and MariaDB 11.x
/// containers of the matrix turn TLS on by themselves; MariaDB 10.11 and
/// PG of the plain matrix do not.
#[tokio::test]
async fn tls_info_tells_the_truth() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    if let Some(url) = test_url() {
        let s = Session::connect(&url).await.expect("connection");
        assert_eq!(s.tls_info().await.expect("pg_stat_ssl"), None);
    }
    if let Some(url) = mysql_url() {
        let plain = Session::connect(&url).await.expect("connection");
        assert_eq!(plain.tls_info().await.unwrap(), None);
        // MariaDB without certificates (10.11 of the matrix): `require` gets
        // rejected, and rightly so — the encrypted branch is not tested here.
        // `@@have_ssl` only on MariaDB: MySQL 8.4 removed it.
        if scalar(&plain, "SELECT VERSION()").await.contains("MariaDB")
            && scalar(&plain, "SELECT @@have_ssl").await != "YES"
        {
            eprintln!("skipped the TLS branch: MariaDB without SSL configured");
            return;
        }
        let tls = TlsOptions {
            mode: TlsMode::Require,
            root_cert: None,
            tls_server_name: None,
        };
        let s = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
            .await
            .expect("TLS connection");
        let info = s.tls_info().await.unwrap().expect("encrypted channel");
        assert!(
            info.protocol.as_deref().unwrap_or("").starts_with("TLSv1."),
            "{info:?}"
        );
        assert!(info.cipher.is_some(), "{info:?}");
        assert!(!info.verified, "require never verifies the server");
    }
}

/// The lab CA (`cargo xtask test-matrix tls`, `cargo xtask tls-lab`):
/// the server certificate of the TLS targets is signed by it. `None` =
/// plain matrix, the verifying tests skip (or fail if required).
fn lab_ca() -> Option<std::path::PathBuf> {
    match std::env::var("ALAMO_TEST_TLS_CA") {
        Ok(p) => Some(p.into()),
        Err(_) => {
            skip_or_fail("ALAMO_TEST_TLS_CA");
            None
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// verify-full with the lab CA on PG: the connection passes and the UI gets
/// the whole picture — protocol, cipher, the server certificate as the
/// verifier saw it, verified.
#[tokio::test]
async fn tls_verify_full_with_lab_ca_reports_certificate() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!();
    let Some(ca) = lab_ca() else { return };
    let tls = TlsOptions {
        mode: TlsMode::VerifyFull,
        root_cert: Some(ca),
        tls_server_name: None,
    };
    let s = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
        .await
        .expect("verify-full with the lab CA");
    let info = s.tls_info().await.unwrap().expect("encrypted channel");
    assert!(
        info.protocol.as_deref().unwrap_or("").starts_with("TLSv1."),
        "{info:?}"
    );
    assert!(
        info.cipher.as_deref().is_some_and(|c| !c.is_empty()),
        "{info:?}"
    );
    assert!(info.verified, "{info:?}");
    let cert = info.server_cert.expect("server certificate captured");
    assert!(
        cert.subject.as_deref().unwrap().contains("CN=localhost"),
        "{cert:?}"
    );
    assert!(
        cert.issuer.as_deref().unwrap().contains("CN=Alamo Test CA"),
        "{cert:?}"
    );
    assert!(cert.not_before.unwrap() < unix_now(), "{cert:?}");
    assert!(cert.not_after.unwrap() > unix_now(), "{cert:?}");
}

/// verify-ca with the lab CA on PG: chain verified, verified for the UI.
#[tokio::test]
async fn tls_verify_ca_with_lab_ca() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!();
    let Some(ca) = lab_ca() else { return };
    let tls = TlsOptions {
        mode: TlsMode::VerifyCa,
        root_cert: Some(ca),
        tls_server_name: None,
    };
    let s = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
        .await
        .expect("verify-ca with the lab CA");
    let info = s.tls_info().await.unwrap().expect("encrypted channel");
    assert!(info.verified, "{info:?}");
    assert!(info.server_cert.is_some(), "{info:?}");
}

/// verify-full without the lab CA: the system roots do not know the lab,
/// the connection is refused — not degraded, not "verified".
#[tokio::test]
async fn tls_verify_full_without_lab_ca_is_refused() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_url!();
    if lab_ca().is_none() {
        return;
    }
    let tls = TlsOptions {
        mode: TlsMode::VerifyFull,
        root_cert: None,
        tls_server_name: None,
    };
    let err = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
        .await
        .err()
        .expect("unknown CA must be refused");
    assert!(err.message.contains("UnknownIssuer"), "{}", err.message);
}

/// verify-full with the lab CA on MySQL/MariaDB: encrypted and verified,
/// protocol, cipher and validity from the server; subject and issuer are
/// not available there, and the UI says so.
#[tokio::test]
async fn tls_verify_full_with_lab_ca_reports_cipher_and_validity() {
    use alamo_core::tls::{TlsMode, TlsOptions};
    let url = require_mysql!();
    let Some(ca) = lab_ca() else { return };
    let tls = TlsOptions {
        mode: TlsMode::VerifyFull,
        root_cert: Some(ca),
        tls_server_name: None,
    };
    let s = Session::connect_opts(builtin_for_url(&url), &url, false, false, tls)
        .await
        .expect("verify-full with the lab CA");
    let info = s.tls_info().await.unwrap().expect("encrypted channel");
    assert!(
        info.protocol.as_deref().unwrap_or("").starts_with("TLSv1."),
        "{info:?}"
    );
    assert!(
        info.cipher.as_deref().is_some_and(|c| !c.is_empty()),
        "{info:?}"
    );
    assert!(info.verified, "{info:?}");
    let cert = info
        .server_cert
        .expect("validity from the status variables");
    assert_eq!((&cert.subject, &cert.issuer), (&None, &None), "{cert:?}");
    assert!(cert.not_before.unwrap() < unix_now(), "{cert:?}");
    assert!(cert.not_after.unwrap() > unix_now(), "{cert:?}");
}

/// A server that accepts and never speaks (no database needed): the connect
/// gives up after the configured timeout (§16) with a Transport error, on
/// both engines, instead of hanging on the handshake.
#[tokio::test]
async fn connect_gives_up_on_a_silent_server() {
    use alamo_session::driver::Drivers;
    use alamo_session::NetOptions;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&held);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            keep.lock().unwrap().push(sock);
        }
    });
    let drivers = Drivers::builtin();
    for (id, url) in [
        (
            "postgres",
            format!("host=127.0.0.1 port={port} user=u dbname=d"),
        ),
        ("mysql", format!("mysql://u:p@127.0.0.1:{port}/d")),
    ] {
        let started = std::time::Instant::now();
        let err = Session::connect_with(
            drivers.get(id).unwrap(),
            &url,
            false,
            false,
            alamo_core::tls::TlsOptions::plain(),
            NetOptions {
                connect_timeout: Duration::from_secs(1),
                ..Default::default()
            },
        )
        .await
        .err()
        .expect(id);
        assert_eq!(
            err.category,
            alamo_core::error::ErrorCategory::Transport,
            "{id}: {err:?}"
        );
        assert_eq!(err.message, "No answer from the server within 1 s", "{id}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{id}: {:?}",
            started.elapsed()
        );
    }
}

// ---------------------------------------------------------------- result_source (§4.3.1)

/// Runs `sql` on the sticky and gives the id of its result set; a failed
/// statement names itself.
async fn select_result(session: &Session, sql: &str) -> u64 {
    let (c, emit) = collector();
    session
        .execute(sql.into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let failed = c.events.lock().unwrap().iter().find_map(|e| match e {
        ExecEvent::StatementFinished {
            outcome: OutcomeOut::Failed { error },
            ..
        } => Some(error.message.clone()),
        _ => None,
    });
    if let Some(msg) = failed {
        panic!("{sql}: {msg}");
    }
    table_result(&c).0
}

/// Every reason a result set stays read-only, and the one shape that is
/// editable, on the same three throwaway tables for both engines.
async fn result_source_cases(session: &Session, schema: &str, now: &str) {
    use alamo_session::NotEditable;
    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE IF EXISTS m4_src; DROP TABLE IF EXISTS m4_src2; DROP TABLE IF EXISTS m4_nopk;
             CREATE TABLE m4_src (id INT PRIMARY KEY, nome VARCHAR(50), n INT DEFAULT 7);
             CREATE TABLE m4_src2 (k1 INT, k2 INT, v INT, PRIMARY KEY (k1, k2));
             CREATE TABLE m4_nopk (a INT);
             INSERT INTO m4_src VALUES (1, 'uno', 1), (2, 'due', 2)"
                .into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
    let table = format!("{schema}.m4_src");
    let source = |sql: &str| {
        let sql = sql.to_string();
        async move {
            let rid = select_result(session, &sql).await;
            session.result_source(rid).await.expect("result_source")
        }
    };

    let out = source("SELECT * FROM m4_src").await;
    assert!(out.editable, "{:?}", out.reason);
    assert_eq!(out.schema.as_deref(), Some(schema));
    assert_eq!(out.table.as_deref(), Some("m4_src"));
    assert_eq!(out.key.len(), 1);
    assert_eq!((out.key[0].col, out.key[0].name.as_str()), (0, "id"));
    assert_eq!(
        out.columns,
        vec![Some("id".into()), Some("nome".into()), Some("n".into())]
    );
    assert_eq!(out.nullable, vec![Some(false), Some(true), Some(true)]);
    assert!(out.defaults[2].as_deref().is_some_and(|d| d.contains('7')));

    let out = source("SELECT id, id, nome AS alias, n * 2 AS twice FROM m4_src").await;
    assert!(out.editable, "{:?}", out.reason);
    assert_eq!(
        out.columns,
        vec![Some("id".into()), None, Some("nome".into()), None]
    );

    let out = source("SELECT id AS k, nome FROM m4_src").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::KeyRenamed {
            table: table.clone(),
            columns: vec!["id".into()]
        })
    );
    let out = source("SELECT nome, n FROM m4_src").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::KeyMissing {
            table: table.clone(),
            columns: vec!["id".into()]
        })
    );
    let out = source("SELECT nome AS id, id FROM m4_src").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::KeyShadowed {
            table: table.clone(),
            columns: vec!["id".into()]
        })
    );
    let out = source("SELECT k1, v FROM m4_src2").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::KeyMissing {
            table: format!("{schema}.m4_src2"),
            columns: vec!["k2".into()]
        })
    );
    let out = source("SELECT * FROM m4_nopk").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::NoPrimaryKey {
            table: format!("{schema}.m4_nopk")
        })
    );
    let out = source("SELECT a.id, b.v FROM m4_src a JOIN m4_src2 b ON b.k1 = a.id").await;
    assert_eq!(
        out.reason,
        Some(NotEditable::SeveralTables {
            tables: vec![table.clone(), format!("{schema}.m4_src2")]
        })
    );
    let out = source(&format!("SELECT 1 + 1 AS two, {now}")).await;
    assert_eq!(out.reason, Some(NotEditable::NoSource));
    assert_eq!(out.columns, vec![None, None]);

    // Gone from the catalog after the result was read: no source, not a crash.
    let rid = select_result(session, "SELECT a FROM m4_nopk").await;
    let (c, emit) = collector();
    session
        .execute("DROP TABLE m4_nopk".into(), FetchOptions::default(), emit)
        .expect("execute");
    wait_done(&c).await;
    let out = session.result_source(rid).await.expect("result_source");
    assert!(!out.editable);

    let (c, emit) = collector();
    session
        .execute(
            "DROP TABLE m4_src; DROP TABLE m4_src2".into(),
            FetchOptions::default(),
            emit,
        )
        .expect("execute");
    wait_done(&c).await;
}

#[tokio::test]
async fn result_source_pg() {
    let url = require_url!();
    let session = Session::connect(&url).await.expect("connection");
    result_source_cases(&session, "public", "now()").await;
}

#[tokio::test]
async fn result_source_mysql() {
    let url = require_mysql!();
    let session = Session::connect(&url).await.expect("connection");
    result_source_cases(&session, "alamo_bench", "NOW()").await;
    // A derived table has no base table behind it: read-only, not the
    // current database's homonym.
    let rid = select_result(&session, "SELECT d.id FROM (SELECT 1 AS id) d").await;
    let out = session.result_source(rid).await.expect("result_source");
    assert_eq!(out.reason, Some(alamo_session::NotEditable::NoSource));
}
