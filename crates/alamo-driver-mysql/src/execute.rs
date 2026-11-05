//! Execution and construction of the columnar result set (MySQL/MariaDB).
//!
//! **Text** protocol (`query_iter`): every value arrives as bytes and the
//! decoding goes by `ColumnKind`. The binary format only exists with
//! prepared statements (§11) and remains an optimisation to be evaluated
//! with measurements — the loop structure is identical to that of the PG
//! driver, which M0 has already measured.

use std::sync::Arc;
use std::time::Instant;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Row, Value};

use alamo_core::chunk::{ChunkBuilder, ChunkLimits};
use alamo_core::column::ColumnKind;
use alamo_core::dialect::Dialect;
use alamo_core::error::{DbError, Severity};
use alamo_core::meta::{ColumnMeta, ColumnSource, NativeType};
use alamo_core::result_set::ResultSetState;
use alamo_core::schema_info::ConnectionBudget;
use alamo_core::stream::{
    ExecutionStats, FetchOptions, StopReason, StreamControl, StreamEvent, StreamOutcome,
};

use crate::dialect::MyDialect;
use crate::error::{first_as, rows_as, MyError};
use crate::typecode as tc;

/// Rows of the first chunk: small on purpose, so `fetch_window` has
/// something to show within a few ms even if steady-state chunks are 64k
/// (§15).
const FIRST_CHUNK_ROWS: u32 = 1024;

/// Builds the schema from the result set metadata (§4.3: they arrive at
/// execution time, afterwards they cannot be rebuilt).
fn build_schema(cols: &[mysql_async::Column], dialect: &MyDialect) -> Vec<ColumnMeta> {
    cols.iter()
        .map(|c| {
            let native = NativeType::MySql {
                code: c.column_type() as u8,
                flags: c.flags().bits(),
                charset: c.character_set(),
            };
            let mut meta = ColumnMeta::new(
                c.name_str().as_ref(),
                native.clone(),
                dialect.type_display(&native),
            );
            meta.semantic_class = dialect.semantic_class(&native);
            meta.numeric_text = dialect.is_numeric_text(&native);
            // Text protocol: everything arrives as bytes, there is no binary
            // format we could fail to decode.
            meta.undecoded_wire_format = false;
            // Source table (§4.3): today it names the INSERT export of a
            // query; grid editing (M4) will start from here. Empty for
            // computed columns (`NOW()`, `a+b`).
            let org_table = c.org_table_str();
            meta.source = (!org_table.is_empty()).then(|| ColumnSource::MySql {
                db: c.schema_str().into_owned(),
                org_table: org_table.into_owned(),
                org_name: c.org_name_str().into_owned(),
            });
            meta
        })
        .collect()
}

/// `ER_UNSUPPORTED_PS`: the statement is a single one, but cannot be prepared.
const ER_UNSUPPORTED_PS: u16 = 1295;

/// `mysql_async` opens the connection with `CLIENT_MULTI_STATEMENTS` and
/// does not allow removing it: a `query_iter` containing `x; DELETE …`
/// executes both. The splitter can get it wrong (server `sql_mode` such as
/// `NO_BACKSLASH_ESCAPES` or `ANSI_QUOTES`, executable comments) and then
/// the DELETE would escape the classifier, confirmations and read-only mode.
/// The prepare accepts ONE statement only: if it refuses, nothing is
/// executed and the user sees the server error.
///
/// Only if the text contains a `;`: without one, the server cannot see two
/// statements in it, and we save a network roundtrip per statement.
async fn ensure_single_statement(conn: &mut Conn, sql: &str) -> Result<(), MyError> {
    if !sql.contains(';') {
        return Ok(());
    }
    match conn.prep(sql).await {
        Ok(stmt) => conn.close(stmt).await.map_err(MyError::from),
        Err(mysql_async::Error::Server(e)) if e.code == ER_UNSUPPORTED_PS => Ok(()),
        Err(e) => Err(MyError::from(e)),
    }
}

/// Executes in streaming: closed chunks delivered via `on_event`, pause on
/// budget, cooperative stop via `control`. Same shape as the PG driver.
/// `kill_query`: the `KILL QUERY` of this connection (see [`kill`]), awaited
/// only if the stream stops before the end.
pub async fn execute_streaming(
    conn: &mut Conn,
    sql: &str,
    dialect: &MyDialect,
    opts: &FetchOptions,
    control: &mut tokio::sync::mpsc::Receiver<StreamControl>,
    kill_query: impl std::future::Future<Output = ()> + Send,
    mut on_event: impl FnMut(StreamEvent) + Send,
) -> Result<StreamOutcome, MyError> {
    let started = Instant::now();
    ensure_single_statement(conn, sql).await?;
    let mut qr = conn.query_iter(sql).await.map_err(MyError::from)?;

    let mut stats = ExecutionStats::default();

    // On DML/DDL `columns()` can return `Some` but empty: the real
    // discriminant is the absence of columns. The command tag tells Affected
    // from Void (§4.1).
    let columns = qr.columns().filter(|c| !c.is_empty());
    let Some(columns) = columns else {
        let rows_affected = qr.affected_rows();
        drop(qr);
        stats.total = started.elapsed();
        return Ok(StreamOutcome {
            state: ResultSetState::Complete,
            stats,
            error: None,
            rows_affected: Some(rows_affected),
            in_transaction: server_in_txn(conn),
        });
    };

    let schema_vec = build_schema(&columns, dialect);
    let kinds: Vec<ColumnKind> = schema_vec
        .iter()
        .map(|m| dialect.column_kind(&m.native_type))
        .collect();
    // BIT columns: same kind as BIGINT UNSIGNED, different wire encoding.
    let bits: Vec<bool> = schema_vec
        .iter()
        .map(|m| matches!(m.native_type, NativeType::MySql { code, .. } if code == tc::BIT))
        .collect();
    let schema: Arc<[ColumnMeta]> = schema_vec.into();
    on_event(StreamEvent::Schema {
        schema: Arc::clone(&schema),
        undecoded_columns: Vec::new(),
    });

    let first_limits = ChunkLimits {
        max_rows: FIRST_CHUNK_ROWS.min(opts.chunk_limits.max_rows),
        ..opts.chunk_limits
    };
    let mut chunk = ChunkBuilder::new(&kinds, first_limits);
    let mut layout_bytes_total: u64 = 0;
    let mut state = ResultSetState::Complete;
    let mut error = None;
    // Moving threshold: every "Continue" grants another budget increment.
    let mut budget_threshold = opts.soft_budget_bytes;
    let mut row_pause = opts.pause_after_rows;

    'outer: loop {
        let row: Row = tokio::select! {
            biased;
            ctrl = control.recv() => {
                match ctrl {
                    Some(StreamControl::Stop(StopReason::UserStopped)) | None => {
                        state = ResultSetState::Truncated;
                        break 'outer;
                    }
                    Some(StreamControl::Stop(StopReason::Cancelled)) => {
                        state = ResultSetState::PartialCancelled;
                        break 'outer;
                    }
                    // Resume outside a pause: harmless race, ignored.
                    // "Load all" while a page is arriving: it also applies
                    // outside a pause, no pause at the next page.
                    Some(StreamControl::Resume) => {
                        row_pause = None;
                        continue 'outer;
                    }
                    // More rows outside a pause: harmless race, ignored.
                    Some(StreamControl::ResumeRows) => continue 'outer,
                }
            }
            row = qr.next() => match row {
                Ok(Some(r)) => r,
                Ok(None) => break 'outer,
                Err(e) => {
                    let db = MyError::from(e).into_db_error();
                    state = if db.is_cancellation() {
                        ResultSetState::PartialCancelled
                    } else {
                        ResultSetState::Truncated
                    };
                    error = Some(db);
                    break 'outer;
                }
            }
        };

        if stats.time_to_first_row.is_none() {
            stats.time_to_first_row = Some(started.elapsed());
        }
        for (col, kind) in kinds.iter().enumerate() {
            // Index outside the row = our own indexing bug, never a silent
            // NULL: explicit error as in the PG driver (`ColumnOutOfRange`),
            // not a false value in the grid (§8.2).
            let v = row.as_ref(col).ok_or_else(|| {
                MyError::from(mysql_async::Error::Other(Box::new(
                    alamo_core::CoreError::ColumnOutOfRange { index: col },
                )))
            })?;
            if matches!(v, Value::NULL) {
                chunk
                    .column_mut(col)
                    .expect("column index from the schema")
                    .push_null();
                continue;
            }
            stats.wire_bytes += wire_size(v) as u64;
            push_value(&mut chunk, col, *kind, bits[col], v)?;
        }
        chunk.end_row();
        stats.rows += 1;

        // Pause at N rows: the chunk closes there, even if not full, so the
        // grid has exactly the rows announced by the banner.
        let rows_due = row_pause.is_some_and(|n| stats.rows >= n);
        if chunk.should_close() || rows_due {
            let byte_size = chunk.byte_size() as u64;
            let (finished, chunk_stats) = chunk.finish();
            chunk = ChunkBuilder::new(&kinds, opts.chunk_limits);
            layout_bytes_total += byte_size;
            stats.chunks += 1;
            on_event(StreamEvent::Chunk {
                chunk: finished,
                stats: chunk_stats,
                rows_total: stats.rows,
                layout_bytes_total,
                wire_bytes_total: stats.wire_bytes,
            });

            let bytes_due = budget_threshold.is_some_and(|b| layout_bytes_total >= b as u64);
            if rows_due || bytes_due {
                // ⚠️ §11: unlike the PG portal, a paused MySQL stream
                // OCCUPIES the connection — the §5.1 watchdog on the session
                // side is the only defence against a forgotten pause.
                on_event(StreamEvent::BudgetReached {
                    rows: stats.rows,
                    layout_bytes: layout_bytes_total,
                    by_rows: rows_due,
                });
                match control.recv().await {
                    Some(c @ (StreamControl::Resume | StreamControl::ResumeRows)) => {
                        // "Load all" removes the row pause, scrolling grants
                        // another page of it; the byte one grants another
                        // increment.
                        if rows_due {
                            row_pause = match c {
                                StreamControl::ResumeRows => {
                                    opts.pause_after_rows.map(|n| stats.rows + n)
                                }
                                _ => None,
                            };
                        }
                        if let (true, Some(budget)) = (bytes_due, budget_threshold) {
                            budget_threshold =
                                Some(budget + opts.soft_budget_bytes.unwrap_or(budget));
                        }
                    }
                    Some(StreamControl::Stop(StopReason::Cancelled)) => {
                        state = ResultSetState::PartialCancelled;
                        break 'outer;
                    }
                    Some(StreamControl::Stop(StopReason::UserStopped)) | None => {
                        state = ResultSetState::Truncated;
                        break 'outer;
                    }
                }
            }
        }
        if let Some(max) = opts.max_rows {
            if stats.rows >= max {
                state = ResultSetState::Truncated;
                break 'outer;
            }
        }
    }

    if chunk.rows() > 0 {
        let byte_size = chunk.byte_size() as u64;
        let (finished, chunk_stats) = chunk.finish();
        layout_bytes_total += byte_size;
        stats.chunks += 1;
        on_event(StreamEvent::Chunk {
            chunk: finished,
            stats: chunk_stats,
            rows_total: stats.rows,
            layout_bytes_total,
            wire_bytes_total: stats.wire_bytes,
        });
    }

    // Stop mid-stream: the connection stays occupied by the unconsumed
    // result set (§11) and the server would go on sending the rest of it —
    // minutes on a large table behind SSH. `KILL QUERY` first, then the
    // drain reads only the rows already in flight and the interruption
    // error. A cancel has already sent it (§7.1).
    if state != ResultSetState::Complete && error.is_none() {
        if state != ResultSetState::PartialCancelled {
            kill_query.await;
        }
        while let Ok(Some(_)) = qr.next().await {}
    }
    drop(qr);

    stats.total = started.elapsed();
    let in_transaction = server_in_txn(conn);
    Ok(StreamOutcome {
        state,
        stats,
        error,
        rows_affected: None,
        in_transaction,
    })
}

/// §9.7: REAL transaction state from the server, from the last OK packet
/// (`SERVER_STATUS_IN_TRANS`). `None` after an error: mysql_async clears the
/// last OK packet (`handle_err`), but the server may still have the
/// transaction open — the session tracking decides.
fn server_in_txn(conn: &Conn) -> Option<bool> {
    use mysql_async::consts::StatusFlags;
    conn.last_ok_packet().map(|ok| {
        ok.status_flags()
            .contains(StatusFlags::SERVER_STATUS_IN_TRANS)
    })
}

/// `USE` with backtick quoting: changes the default database of unqualified
/// statements on the connection.
pub async fn use_database(conn: &mut Conn, db: &str) -> Result<(), MyError> {
    let sql = format!("USE {}", MyDialect::default().quote_identifier(db));
    conn.query_drop(sql).await.map_err(MyError::from)
}

/// Current database of the connection (`SELECT DATABASE()`): reflects the
/// last `USE` and may be none. For the context shown in the UI.
pub async fn current_database(conn: &mut Conn) -> Result<Option<String>, MyError> {
    let v: Option<Option<String>> = conn
        .query_first::<Row, _>("SELECT DATABASE()")
        .await
        .map_err(MyError::from)
        .and_then(first_as)?;
    Ok(v.flatten())
}

/// Estimate of the affected rows via tabular `EXPLAIN` (§9.7): the `rows`
/// column exists with the same name on MySQL and MariaDB — the JSON format
/// instead diverges between the two. With several plan rows (join) the
/// maximum is taken: an honest estimate of the largest scan, never a count.
/// `None` = the plan exposes no estimates (e.g. empty table, `rows` NULL).
///
/// Prepared protocol (`exec`), never `query`: the connection has
/// multi-statements enabled and the text protocol would execute an
/// `x; DELETE …` that escaped the splitter (the MySQL lexer is not PG's);
/// `COM_STMT_PREPARE` refuses it. On top of that a transaction closed by
/// `ROLLBACK`: if something still got executed, InnoDB undoes it.
/// Not `READ ONLY`: MySQL and MariaDB refuse `EXPLAIN UPDATE/DELETE` in a
/// read-only transaction, and the estimate is needed exactly there.
pub async fn explain_rows_estimate(conn: &mut Conn, sql: &str) -> Result<Option<u64>, MyError> {
    conn.query_drop("START TRANSACTION")
        .await
        .map_err(MyError::from)?;
    let explained = conn.exec::<Row, _, _>(format!("EXPLAIN {sql}"), ()).await;
    let rolled_back = conn.query_drop("ROLLBACK").await;
    let rows = explained.map_err(MyError::from)?;
    rolled_back.map_err(MyError::from)?;
    let mut max: Option<u64> = None;
    for row in rows {
        let idx = row
            .columns_ref()
            .iter()
            .position(|c| c.name_str().eq_ignore_ascii_case("rows"));
        let Some(idx) = idx else { continue };
        if let Some(Some(n)) = row.get_opt::<Option<u64>, usize>(idx).and_then(Result::ok) {
            max = Some(max.map_or(n, |m| m.max(n)));
        }
    }
    Ok(max)
}

/// Server-side read-only (§9.7 point 4): the session's transactions are
/// born read-only and the server refuses writes — a real safety net, not
/// just the client classifier. The variable name diverges: MySQL >= 5.7.20
/// uses `transaction_read_only`, MariaDB stayed on `tx_read_only`.
pub async fn set_session_read_only(conn: &mut Conn, mariadb: bool) -> Result<(), MyError> {
    let var = if mariadb {
        "tx_read_only"
    } else {
        "transaction_read_only"
    };
    conn.query_drop(format!("SET SESSION {var} = 1"))
        .await
        .map_err(MyError::from)
}

/// `idle_transaction_timeout` (MariaDB 10.3+ only, in seconds): the extra
/// safety net of §9.7.2 against transactions left open by mistake. MySQL
/// has no per-session equivalent: no-op.
pub async fn set_idle_txn_timeout(
    conn: &mut Conn,
    mariadb: bool,
    secs: u32,
) -> Result<(), MyError> {
    if !mariadb {
        return Ok(());
    }
    conn.query_drop(format!("SET SESSION idle_transaction_timeout = {secs}"))
        .await
        .map_err(MyError::from)
}

/// Per-statement server-side limit (§16), given in milliseconds. MySQL
/// `max_execution_time` is in milliseconds but bounds only read-only
/// SELECTs (5.7.8+); MariaDB `max_statement_time` is in seconds (a double)
/// and bounds every statement. 0 disables it on both.
pub async fn set_statement_timeout(conn: &mut Conn, mariadb: bool, ms: u32) -> Result<(), MyError> {
    let sql = if mariadb {
        format!(
            "SET SESSION max_statement_time = {}",
            f64::from(ms) / 1000.0
        )
    } else {
        format!("SET SESSION max_execution_time = {ms}")
    };
    conn.query_drop(sql).await.map_err(MyError::from)
}

/// The user name of the connection as `PROCESSLIST.USER` shows it: cut at
/// the LAST `@` of `CURRENT_USER()` (the name may contain one, the host
/// cannot).
pub(crate) const OWN_USER_SQL: &str = "LEFT(CURRENT_USER(), CHAR_LENGTH(CURRENT_USER()) \
     - CHAR_LENGTH(SUBSTRING_INDEX(CURRENT_USER(), '@', -1)) - 1)";

/// `None` = no limit. The **session** value of `@@max_user_connections` is
/// already the effective one — the account limit if non-zero, otherwise the
/// global one — on MySQL 8.4 and MariaDB 11.8, and it can be read without
/// privileges (`mysql.user` would require them). The count comes from
/// `PROCESSLIST`, which without `PROCESS` still shows one's own threads. It
/// filters on the name only: the limit is per account (`name@host`), so
/// with the same name on several hosts the count is an overestimate — the
/// safe side (see `OWN_USER_SQL`).
pub async fn connection_budget(conn: &mut Conn) -> Result<Option<ConnectionBudget>, MyError> {
    // MariaDB treats the variable as signed: read as i64, <= 0 = no limit.
    let row: Option<(i64, i64)> = conn
        .query_first::<Row, _>(format!(
            "SELECT @@session.max_user_connections, \
                    (SELECT COUNT(*) FROM information_schema.PROCESSLIST \
                     WHERE USER = {OWN_USER_SQL})"
        ))
        .await
        .map_err(MyError::from)
        .and_then(first_as)?;
    let Some((limit, in_use)) = row else {
        return Ok(None);
    };
    if limit <= 0 {
        return Ok(None);
    }
    Ok(Some(ConnectionBudget {
        limit: u32::try_from(limit).unwrap_or(u32::MAX),
        in_use: u32::try_from(in_use).unwrap_or(0),
    }))
}

/// Explicit `SHOW WARNINGS` (§11): MySQL has no asynchronous notices. To be
/// called once the statement has completed, on the **same** connection.
pub async fn fetch_warnings(conn: &mut Conn) -> Result<Vec<DbError>, MyError> {
    let rows: Vec<(String, u16, String)> = conn
        .query::<Row, _>("SHOW WARNINGS")
        .await
        .map_err(MyError::from)
        .and_then(rows_as)?;
    Ok(rows
        .into_iter()
        .map(|(level, code, message)| {
            let mut db = DbError::internal(message);
            db.severity = match level.as_str() {
                "Error" => Severity::Error,
                "Note" => Severity::Notice,
                _ => Severity::Warning,
            };
            db.code = alamo_core::error::ErrorCode::MySql {
                errno: code,
                sqlstate: None,
            };
            db.category = alamo_core::error::ErrorCategory::Sql;
            db
        })
        .collect())
}

// ------------------------------------------------------------------ decoding

/// Bytes "on the wire" attributed to a value, for the §15 statistic.
fn wire_size(v: &Value) -> usize {
    match v {
        Value::Bytes(b) => b.len(),
        Value::NULL => 0,
        _ => 8,
    }
}

/// Decodes a text-protocol value into the columnar buffer. Degradation
/// (§4.4) fires on the value that does not respect the format: the column
/// becomes text, never an error that throws away the stream. `bit`: the
/// column is a BIT, its bytes are the number itself, not its decimal text.
fn push_value(
    chunk: &mut ChunkBuilder,
    col: usize,
    kind: ColumnKind,
    bit: bool,
    v: &Value,
) -> Result<(), MyError> {
    let res = match kind {
        ColumnKind::Utf8 => match v {
            Value::Bytes(b) => chunk.push_text_bytes(col, b),
            other => chunk.push_text(col, &text_of(other)),
        },
        ColumnKind::Binary => match v {
            Value::Bytes(b) => chunk.push_bytes(col, b),
            other => chunk.push_bytes(col, text_of(other).as_bytes()),
        },
        ColumnKind::I32 => match int_of(v) {
            Some(n) if i32::try_from(n).is_ok() => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_i32(n as i32),
            _ => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::I64 => match int_of(v) {
            Some(n) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_i64(n),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::U64 => match if bit { bit_of(v) } else { uint_of(v) } {
            Some(n) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_u64(n),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::F32 => match float_of(v) {
            Some(f) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_f32(f as f32),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::F64 => match float_of(v) {
            Some(f) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_f64(f),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::Date32 => match date_days(v) {
            Some(d) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_date32(d),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        ColumnKind::TimestampMicros { .. } => match timestamp_micros(v) {
            Some(us) => chunk
                .column_mut(col)
                .expect("column from the schema")
                .push_timestamp_micros(us),
            None => chunk.push_degraded_text(col, &text_of(v)),
        },
        // TIME stays Utf8 (duration, not time of day) and Bool is not mapped
        // by this dialect: text fallback by construction.
        ColumnKind::Bool | ColumnKind::I16 | ColumnKind::Time64Micros => {
            chunk.push_degraded_text(col, &text_of(v))
        }
    };
    res.map_err(|e| MyError::from(mysql_async::Error::Other(Box::new(e))))
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Date(y, mo, d, h, mi, s, us) => {
            // Fraction only if present: the server never writes ".000000",
            // and this rendering is also the one that degrades in the grid
            // (§4.4) — it must stay canonical.
            if *h == 0 && *mi == 0 && *s == 0 && *us == 0 {
                format!("{y:04}-{mo:02}-{d:02}")
            } else if *us == 0 {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
            }
        }
        Value::Time(neg, d, h, mi, s, us) => {
            let sign = if *neg { "-" } else { "" };
            let hours = *d as u64 * 24 + *h as u64;
            if *us == 0 {
                format!("{sign}{hours}:{mi:02}:{s:02}")
            } else {
                format!("{sign}{hours}:{mi:02}:{s:02}.{us:06}")
            }
        }
        Value::NULL => String::new(),
    }
}

fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::UInt(u) => i64::try_from(*u).ok(),
        Value::Bytes(b) => std::str::from_utf8(b).ok()?.trim().parse().ok(),
        _ => None,
    }
}

fn uint_of(v: &Value) -> Option<u64> {
    match v {
        Value::UInt(u) => Some(*u),
        Value::Int(i) => u64::try_from(*i).ok(),
        Value::Bytes(b) => std::str::from_utf8(b).ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// BIT(n) in the text protocol: a big-endian byte string of ceil(n/8)
/// bytes (`b'1'` arrives as `\x01`, not as "1"). Up to 64 bits.
fn bit_of(v: &Value) -> Option<u64> {
    match v {
        Value::Bytes(b) if b.len() <= 8 => {
            Some(b.iter().fold(0u64, |acc, x| (acc << 8) | *x as u64))
        }
        Value::Bytes(_) => None,
        other => uint_of(other),
    }
}

fn float_of(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f as f64),
        Value::Double(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        Value::UInt(u) => Some(*u as f64),
        Value::Bytes(b) => std::str::from_utf8(b).ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// Days since the Unix epoch for a civil date (days_from_civil algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn date_days(v: &Value) -> Option<i32> {
    match v {
        Value::Date(y, m, d, _, _, _, _) => {
            i32::try_from(days_from_civil(*y as i64, *m as u32, *d as u32)).ok()
        }
        Value::Bytes(b) => {
            let s = std::str::from_utf8(b).ok()?;
            let (y, m, d) = parse_ymd(s)?;
            i32::try_from(days_from_civil(y, m, d)).ok()
        }
        _ => None,
    }
}

fn timestamp_micros(v: &Value) -> Option<i64> {
    match v {
        Value::Date(y, mo, d, h, mi, s, us) => Some(
            days_from_civil(*y as i64, *mo as u32, *d as u32) * 86_400_000_000
                + (*h as i64 * 3600 + *mi as i64 * 60 + *s as i64) * 1_000_000
                + *us as i64,
        ),
        Value::Bytes(b) => {
            let txt = std::str::from_utf8(b).ok()?;
            let (date, time) = txt.split_once([' ', 'T']).unwrap_or((txt, ""));
            let (y, mo, d) = parse_ymd(date)?;
            let mut micros = days_from_civil(y, mo, d) * 86_400_000_000;
            if !time.is_empty() {
                let mut it = time.splitn(3, ':');
                let h: i64 = it.next()?.parse().ok()?;
                let mi: i64 = it.next()?.parse().ok()?;
                let rest = it.next().unwrap_or("0");
                let (s, frac) = rest.split_once('.').unwrap_or((rest, ""));
                let s: i64 = s.parse().ok()?;
                // "25:99:99" is not a time: out of range it degrades to text
                // as it arrived (§4.4), no silent arithmetic normalisation.
                // 60 and 61 remain allowed as leap seconds.
                if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..62).contains(&s) {
                    return None;
                }
                let us: i64 = if frac.is_empty() {
                    0
                } else {
                    let padded = format!("{frac:0<6}");
                    padded.get(..6)?.parse().ok()?
                };
                micros += (h * 3600 + mi * 60 + s) * 1_000_000 + us;
            }
            Some(micros)
        }
        _ => None,
    }
}

fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.trim().splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if m == 0 || d == 0 {
        // "0000-00-00": MySQL's zero-date is not a real date.
        return None;
    }
    // With ALLOW_INVALID_DATES the server really sends "2024-13-01":
    // normalising with the calendar would produce a wrong value that looks
    // like a right one. The raw range is enough (month 1-12, day 1-31): out
    // of range the cell degrades to text as it arrived (§4.4).
    if m > 12 || d > 31 {
        return None;
    }
    Some((y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_bytes_are_big_endian_numbers() {
        assert_eq!(bit_of(&Value::Bytes(vec![0])), Some(0));
        assert_eq!(bit_of(&Value::Bytes(vec![1])), Some(1));
        assert_eq!(bit_of(&Value::Bytes(vec![1, 0])), Some(256));
        assert_eq!(bit_of(&Value::Bytes(vec![0xff; 8])), Some(u64::MAX));
        assert_eq!(bit_of(&Value::Bytes(vec![0; 9])), None);
        assert_eq!(bit_of(&Value::UInt(5)), Some(5));
    }

    #[test]
    fn civil_days_match_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }

    #[test]
    fn timestamp_text_with_fraction() {
        let v = Value::Bytes(b"2024-01-01 00:00:01.5".to_vec());
        assert_eq!(timestamp_micros(&v), Some(1_704_067_201_500_000));
    }

    #[test]
    fn zero_date_degrades_not_panics() {
        assert_eq!(date_days(&Value::Bytes(b"0000-00-00".to_vec())), None);
    }

    #[test]
    fn out_of_range_date_degrades_instead_of_normalizing() {
        // ALLOW_INVALID_DATES: month or day outside the raw range must not
        // be normalised (2024-13-01 must not become January 2025).
        let ts = |s: &[u8]| timestamp_micros(&Value::Bytes(s.to_vec()));
        assert_eq!(ts(b"2024-13-01 00:00:00"), None);
        assert_eq!(ts(b"2024-01-32 00:00:00"), None);
        assert_eq!(date_days(&Value::Bytes(b"2024-13-01".to_vec())), None);
        assert_eq!(date_days(&Value::Bytes(b"2024-01-32".to_vec())), None);
        // The range is raw, not the exact calendar: 2024-04-31 passes.
        assert!(ts(b"2024-04-31 00:00:00").is_some());
    }

    #[test]
    fn out_of_range_time_degrades() {
        let ts = |s: &[u8]| timestamp_micros(&Value::Bytes(s.to_vec()));
        assert_eq!(ts(b"2024-01-01 25:00:00"), None);
        assert_eq!(ts(b"2024-01-01 10:99:00"), None);
        assert_eq!(ts(b"2024-01-01 10:00:99"), None);
        assert_eq!(ts(b"2024-01-01 25:99:99"), None);
        // Leap seconds (60, 61) remain allowed.
        assert!(ts(b"2016-12-31 23:59:60").is_some());
    }

    #[test]
    fn text_of_omits_zero_fraction() {
        assert_eq!(
            text_of(&Value::Date(2024, 1, 2, 3, 4, 5, 0)),
            "2024-01-02 03:04:05"
        );
        assert_eq!(
            text_of(&Value::Date(2024, 1, 2, 3, 4, 5, 60_000)),
            "2024-01-02 03:04:05.060000"
        );
        assert_eq!(text_of(&Value::Time(false, 0, 1, 2, 3, 0)), "1:02:03");
        assert_eq!(
            text_of(&Value::Time(true, 1, 1, 2, 3, 7)),
            "-25:02:03.000007"
        );
    }
}
