//! Integration tests against a real PostgreSQL (§15.1).
//!
//! They need `ALAMO_TEST_URL`; without it they are skipped — so `cargo test`
//! stays green in CI without services.
//!
//! ```sh
//! ALAMO_TEST_URL='postgres://postgres:alamo@localhost:5432/alamo_bench' \
//!   cargo test -p alamo-driver-postgres --test integration
//! ```

use alamo_core::meta::{ColumnSource, NativeType};
use alamo_driver_postgres::{execute_collect, FetchOptions, PgSession};

const PG_VAR: &str = "ALAMO_TEST_URL";

/// Noisy skip or hard gate (same deal as the session suite): if
/// `ALAMO_REQUIRE_TEST_DBS` lists the variable, its absence is a FAILURE,
/// not a skip — the matrix always sets it, per engine.
fn skip_or_fail(var: &str) {
    let required = std::env::var("ALAMO_REQUIRE_TEST_DBS").unwrap_or_default();
    if required.split(',').any(|v| v.trim() == var) {
        panic!("{var} not set but required (ALAMO_REQUIRE_TEST_DBS={required})");
    }
    eprintln!("skipped: {var} not set");
}

macro_rules! require_url {
    ($var:expr) => {
        match std::env::var($var).ok() {
            Some(u) => u,
            None => {
                skip_or_fail($var);
                return;
            }
        }
    };
}

/// §4.3.1: every column read straight from a relation says which table and
/// which attribute it comes from, aliases included; a computed column says
/// nothing. The type modifier arrives too (`numeric(10,2)`).
#[tokio::test]
async fn result_columns_carry_their_table_and_attribute() {
    let url = require_url!(PG_VAR);
    let s = PgSession::connect_plain(&url)
        .await
        .expect("PostgreSQL connection");
    s.client
        .batch_execute(
            "DROP TABLE IF EXISTS drv_source;
             CREATE TABLE drv_source (id INT PRIMARY KEY, nome VARCHAR(50), n NUMERIC(10,2))",
        )
        .await
        .expect("throwaway table");
    let oid: u32 = s
        .client
        .query_one("SELECT 'drv_source'::regclass::oid", &[])
        .await
        .expect("table oid")
        .get(0);

    let (rs, _) = execute_collect(
        &s.client,
        "SELECT id, nome AS label, 1 + 1 AS two, n FROM drv_source",
        &FetchOptions::default(),
    )
    .await
    .expect("execute");
    let schema = rs.schema();
    let from = |attnum: i16| {
        Some(ColumnSource::Postgres {
            table_oid: oid,
            column_attnum: attnum,
        })
    };
    assert_eq!(schema[0].source, from(1), "id");
    assert_eq!(schema[1].source, from(2), "nome AS label keeps its origin");
    assert_eq!(schema[1].name, "label");
    assert_eq!(schema[2].source, None, "1 + 1 is computed");
    assert_eq!(schema[3].source, from(3), "n");
    match schema[3].native_type {
        NativeType::Postgres { type_modifier, .. } => {
            assert!(type_modifier > 0, "numeric(10,2) has a type modifier");
        }
        _ => panic!("PostgreSQL native type expected"),
    }

    s.client
        .batch_execute("DROP TABLE drv_source")
        .await
        .expect("drop");
}
