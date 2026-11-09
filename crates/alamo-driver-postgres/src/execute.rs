//! Execution and construction of the columnar result set.
//!
//! This is the path M0 measures: socket -> decoding -> columnar chunks,
//! **without the webview** (§15, phase 1).

use std::sync::Arc;
use std::time::Instant;

use futures_util::{pin_mut, TryStreamExt};
use tokio_postgres::types::{FromSql, Type};
use tokio_postgres::Client;

use alamo_core::chunk::{ChunkBuilder, ChunkLimits};
use alamo_core::column::ColumnKind;
use alamo_core::dialect::Dialect;
use alamo_core::meta::{ColumnMeta, ColumnSource, NativeType};
use alamo_core::result_set::{ResultSet, ResultSetBuilder, ResultSetState};

/// The types of the streaming contract moved up to `alamo-core` with M3
/// (§2: the session talks to any driver); these `pub use` keep the
/// historical path `alamo_driver_postgres::execute::*` stable.
pub use alamo_core::stream::{
    ExecutionStats, FetchOptions, StopReason, StreamControl, StreamEvent, StreamOutcome,
};

use crate::decode;
use crate::dialect::PgDialect;
use crate::error::PgError;
use crate::oid;

/// Raw bytes of a cell, without any interpretation.
///
/// `tokio-postgres` normally decodes into Rust types; here the opposite is
/// needed — reaching the protocol bytes and writing them **directly** into
/// the columnar buffer, without materialising a value per cell. It is the
/// whole point of the §4.2 layout.
#[derive(Debug)]
struct RawBytes<'a>(Option<&'a [u8]>);

impl<'a> FromSql<'a> for RawBytes<'a> {
    fn from_sql(
        _ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(RawBytes(Some(raw)))
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(RawBytes(None))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Builds the schema from the metadata of the prepared statement.
///
/// §4.3: this metadata arrives in the `RowDescription` **at execution
/// time**. Afterwards it cannot be rebuilt.
fn build_schema(stmt_columns: &[tokio_postgres::Column], dialect: &PgDialect) -> Vec<ColumnMeta> {
    stmt_columns
        .iter()
        .map(|c| {
            let native = NativeType::Postgres {
                oid: c.type_().oid(),
                type_modifier: c.type_modifier(),
            };
            let mut meta = ColumnMeta::new(c.name(), native.clone(), dialect.type_display(&native));
            meta.semantic_class = dialect.semantic_class(&native);
            meta.numeric_text = dialect.is_numeric_text(&native);
            meta.undecoded_wire_format = !is_decodable(c.type_().oid());
            // §4.3.1: the table and the attribute the column comes from, as
            // the `RowDescription` says them. Only columns read straight from
            // a relation have them (oid 0 = computed); resolving them to
            // names is the session's job, on the catalog lane.
            meta.source = c
                .table_oid()
                .zip(c.column_id())
                .filter(|(oid, _)| *oid != 0)
                .map(|(table_oid, column_attnum)| ColumnSource::Postgres {
                    table_oid,
                    column_attnum,
                });
            meta
        })
        .collect()
}

fn is_decodable(o: u32) -> bool {
    matches!(
        o,
        oid::BOOL
            | oid::INT2
            | oid::INT4
            | oid::INT8
            | oid::OID
            | oid::FLOAT4
            | oid::FLOAT8
            | oid::NUMERIC
            | oid::UUID
            | oid::TEXT
            | oid::VARCHAR
            | oid::BPCHAR
            | oid::NAME
            | oid::CHAR
            | oid::XML
            | oid::JSON
            | oid::JSONB
            | oid::BYTEA
            | oid::DATE
            | oid::TIME
            | oid::TIMESTAMP
            | oid::TIMESTAMPTZ
            | oid::INET
            | oid::CIDR
            | oid::MACADDR
            | oid::MACADDR8
    ) || oid::is_array(o)
}

/// Estimate of the affected rows via textual `EXPLAIN` (§9.7): the maximum
/// of the plan's `rows=N`. The root node of an UPDATE/DELETE (ModifyTable)
/// reports `rows=0` since PG 14 — the useful estimate is in the child node,
/// hence the maximum and not the first line. Text format and not JSON: no
/// serde in the driver and the same parsing on every version. `None` = no
/// estimate in the plan.
///
/// Extended protocol (`query`, implicit prepare), never `simple_query`: the
/// prepare refuses batches, so an `x; DELETE …` that escaped the splitter
/// becomes an error instead of an executed DELETE. The caller guarantees
/// that `sql` starts with a DML keyword (`classify::explainable`), so no
/// `(ANALYZE)` options after `EXPLAIN`.
///
/// Runs inside a READ ONLY transaction rolled back once the estimate is
/// done, with the session's default `search_path` (`SET LOCAL … TO
/// DEFAULT`): the user's names resolve as in their editor even on the
/// catalog lane, which outside of here stays pinned to `pg_catalog`.
pub async fn explain_rows_estimate(client: &Client, sql: &str) -> Result<Option<u64>, PgError> {
    client
        .batch_execute("BEGIN READ ONLY; SET LOCAL search_path TO DEFAULT")
        .await
        .map_err(PgError::from)?;
    let explained = client.query(&format!("EXPLAIN {sql}"), &[]).await;
    let rolled_back = client.batch_execute("ROLLBACK").await;
    let rows = explained.map_err(PgError::from)?;
    rolled_back.map_err(PgError::from)?;
    let mut max: Option<u64> = None;
    for row in rows {
        let Ok(line) = row.try_get::<_, &str>(0) else {
            continue;
        };
        if let Some(n) = plan_line_rows(line) {
            max = Some(max.map_or(n, |m| m.max(n)));
        }
    }
    Ok(max)
}

/// Maximum of the `rows=N` of ONE line of the textual plan. Only node lines
/// carry the estimate, and they are recognised by `(cost=`: the accessory
/// lines (`Filter:`, `Index Cond:`) echo the WHERE literals — a value
/// `'rows=999'` written by the user must NOT become the estimate. Reading
/// starts from `(cost=` for the same reason: before it there are only the
/// node name and the relation.
fn plan_line_rows(line: &str) -> Option<u64> {
    let cost = line.find("(cost=")?;
    let mut rest = &line[cost..];
    let mut max: Option<u64> = None;
    while let Some(pos) = rest.find("rows=") {
        rest = &rest[pos + 5..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            max = Some(max.map_or(n, |m| m.max(n)));
        }
    }
    max
}

/// Executes and consumes the stream to the end (or up to the first limit).
///
/// For M0 the collection is complete because the throughput must be
/// measured; the §5 soft-budget pause is implemented here as `Paused` — at
/// M1 it becomes a real pause with banner and resume, not a termination.
pub async fn execute_collect(
    client: &Client,
    sql: &str,
    opts: &FetchOptions,
) -> Result<(ResultSet, ExecutionStats), PgError> {
    let dialect = PgDialect::default();
    let started = Instant::now();

    let stmt = client.prepare(sql).await.map_err(PgError::from)?;
    let schema_vec = build_schema(stmt.columns(), &dialect);
    let kinds: Vec<ColumnKind> = schema_vec
        .iter()
        .map(|m| dialect.column_kind(&m.native_type))
        .collect();
    let oids: Vec<u32> = stmt.columns().iter().map(|c| c.type_().oid()).collect();

    let mut stats = ExecutionStats {
        undecoded_columns: schema_vec
            .iter()
            .filter(|m| m.undecoded_wire_format)
            .map(|m| format!("{} ({})", m.name, m.type_display))
            .collect(),
        ..Default::default()
    };

    let schema: Arc<[ColumnMeta]> = schema_vec.into();
    let mut builder = ResultSetBuilder::new(Arc::clone(&schema));
    let mut chunk = ChunkBuilder::new(&kinds, opts.chunk_limits);
    let mut state = ResultSetState::Complete;

    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    let stream = client
        .query_raw(&stmt, params)
        .await
        .map_err(PgError::from)?;
    pin_mut!(stream);

    while let Some(row) = stream.try_next().await.map_err(PgError::from)? {
        if stats.time_to_first_row.is_none() {
            stats.time_to_first_row = Some(started.elapsed());
        }

        for col in 0..kinds.len() {
            let raw: RawBytes<'_> = row.try_get(col).map_err(PgError::from)?;
            let Some(bytes) = raw.0 else {
                chunk
                    .column_mut(col)
                    .expect("column index from the schema")
                    .push_null();
                continue;
            };
            stats.wire_bytes += bytes.len() as u64;
            push_value(&mut chunk, col, kinds[col], oids[col], bytes)?;
        }
        chunk.end_row();
        stats.rows += 1;

        if chunk.should_close() {
            let (finished, chunk_stats) = chunk.finish();
            builder.push_chunk(finished, chunk_stats);
            stats.chunks += 1;
            chunk = ChunkBuilder::new(&kinds, opts.chunk_limits);

            // Budget checks happen **at chunk close**: doing them per row
            // would cost a sum over all columns on every row, and the
            // granularity of a chunk is anyway the one at which we pause or
            // spill (§5).
            if let Some(budget) = opts.soft_budget_bytes {
                if builder.byte_size() >= budget {
                    state = ResultSetState::Paused;
                    break;
                }
            }
        }
        if let Some(max) = opts.max_rows {
            if stats.rows >= max {
                state = ResultSetState::Truncated;
                break;
            }
        }
    }

    if chunk.rows() > 0 {
        let (finished, chunk_stats) = chunk.finish();
        builder.push_chunk(finished, chunk_stats);
        stats.chunks += 1;
    }

    stats.total = started.elapsed();
    builder.add_wire_bytes(stats.wire_bytes);
    Ok((builder.finish(state), stats))
}

/// Rows of the first chunk: small on purpose, so `fetch_window` has
/// something to show within a few ms even if steady-state chunks are 64k
/// (§15, "first row on screen").
const FIRST_CHUNK_ROWS: u32 = 1024;

/// Executes in streaming: closed chunks delivered via `on_event`, pause on
/// budget, cooperative stop via `control`.
pub async fn execute_streaming(
    client: &Client,
    sql: &str,
    opts: &FetchOptions,
    control: &mut tokio::sync::mpsc::Receiver<StreamControl>,
    mut on_event: impl FnMut(StreamEvent) + Send,
) -> Result<StreamOutcome, PgError> {
    let dialect = PgDialect::default();
    let started = Instant::now();

    let stmt = client.prepare(sql).await.map_err(PgError::from)?;
    let schema_vec = build_schema(stmt.columns(), &dialect);
    let kinds: Vec<ColumnKind> = schema_vec
        .iter()
        .map(|m| dialect.column_kind(&m.native_type))
        .collect();
    let oids: Vec<u32> = stmt.columns().iter().map(|c| c.type_().oid()).collect();

    let mut stats = ExecutionStats {
        undecoded_columns: schema_vec
            .iter()
            .filter(|m| m.undecoded_wire_format)
            .map(|m| format!("{} ({})", m.name, m.type_display))
            .collect(),
        ..Default::default()
    };
    let schema: Arc<[ColumnMeta]> = schema_vec.into();
    on_event(StreamEvent::Schema {
        schema: Arc::clone(&schema),
        undecoded_columns: stats.undecoded_columns.clone(),
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

    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    let stream = client
        .query_raw(&stmt, params)
        .await
        .map_err(PgError::from)?;
    pin_mut!(stream);

    'outer: loop {
        let row = tokio::select! {
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
            row = stream.try_next() => match row {
                Ok(Some(r)) => r,
                Ok(None) => break 'outer,
                Err(e) => {
                    let db = translate_stream_error(e);
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
        for col in 0..kinds.len() {
            let raw: RawBytes<'_> = row.try_get(col).map_err(PgError::from)?;
            let Some(bytes) = raw.0 else {
                chunk
                    .column_mut(col)
                    .expect("column index from the schema")
                    .push_null();
                continue;
            };
            stats.wire_bytes += bytes.len() as u64;
            push_value(&mut chunk, col, kinds[col], oids[col], bytes)?;
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

    let rows_affected = stream.rows_affected();
    stats.total = started.elapsed();
    Ok(StreamOutcome {
        state,
        stats,
        error,
        rows_affected,
        // tokio-postgres discards the ReadyForQuery status: the session keeps
        // the transaction state from the control statements alone.
        in_transaction: None,
    })
}

/// An error mid-stream must not throw away the chunks already built: it is
/// translated and reported in the outcome, never as `Err` (§7.3).
fn translate_stream_error(e: tokio_postgres::Error) -> alamo_core::error::DbError {
    crate::error::translate(&e)
}

/// Consumes the stream **without building anything**.
///
/// §15, method 2: "measure the ceiling first". It says how much socket and
/// server can deliver on that machine. If a local PG delivers 250 MB/s, the
/// decoder will never go beyond — and **the real metric is the distance
/// from the ceiling, not the absolute value**.
pub async fn measure_ceiling(client: &Client, sql: &str) -> Result<ExecutionStats, PgError> {
    let started = Instant::now();
    let stmt = client.prepare(sql).await.map_err(PgError::from)?;
    let ncols = stmt.columns().len();
    let mut stats = ExecutionStats::default();

    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    let stream = client
        .query_raw(&stmt, params)
        .await
        .map_err(PgError::from)?;
    pin_mut!(stream);

    while let Some(row) = stream.try_next().await.map_err(PgError::from)? {
        if stats.time_to_first_row.is_none() {
            stats.time_to_first_row = Some(started.elapsed());
        }
        for col in 0..ncols {
            let raw: RawBytes<'_> = row.try_get(col).map_err(PgError::from)?;
            if let Some(b) = raw.0 {
                stats.wire_bytes += b.len() as u64;
            }
        }
        stats.rows += 1;
    }
    stats.total = started.elapsed();
    Ok(stats)
}

/// Writes a value into the column, degrading when the type cannot hold it
/// (§4.6).
///
/// The cell whose binary does not respect the documented format
/// **degrades** (explicit `\x…` rendering) and the stream continues: same
/// choice as the MySQL driver and same contract as §7.3 — the hard error
/// remains only for what prevents going on (framing, column index outside
/// the schema).
fn push_value(
    chunk: &mut ChunkBuilder,
    col: usize,
    kind: ColumnKind,
    oid_value: u32,
    bytes: &[u8],
) -> Result<(), PgError> {
    match kind {
        ColumnKind::Bool => {
            let v = bytes.first().copied().unwrap_or(0) != 0;
            column(chunk, col)?.push_bool(v)?;
        }
        ColumnKind::I16 => match decode::read_i16(bytes) {
            Some(v) => column(chunk, col)?.push_i16(v)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::I32 => match decode::read_i32(bytes) {
            Some(v) => column(chunk, col)?.push_i32(v)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::I64 => {
            let v = if oid_value == oid::OID {
                decode::read_u32(bytes).map(i64::from)
            } else {
                decode::read_i64(bytes)
            };
            match v {
                Some(v) => column(chunk, col)?.push_i64(v)?,
                None => push_malformed(chunk, col, bytes)?,
            }
        }
        ColumnKind::U64 => match decode::read_i64(bytes) {
            Some(v) => column(chunk, col)?.push_u64(v as u64)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::F32 => match decode::read_f32(bytes) {
            Some(v) => column(chunk, col)?.push_f32(v)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::F64 => match decode::read_f64(bytes) {
            Some(v) => column(chunk, col)?.push_f64(v)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::Date32 => match decode::read_i32(bytes) {
            Some(d) => match decode::date_special(d) {
                // `infinity` fits in no integer: the column degrades and
                // stays that way (§4.6).
                Some(text) => chunk.push_degraded_text(col, text)?,
                None => column(chunk, col)?.push_date32(d + oid::PG_EPOCH_DAYS)?,
            },
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::Time64Micros => match decode::read_i64(bytes) {
            Some(t) => column(chunk, col)?.push_time64_micros(t)?,
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::TimestampMicros { .. } => match decode::read_i64(bytes) {
            Some(t) => match decode::timestamp_special(t) {
                Some(text) => chunk.push_degraded_text(col, text)?,
                None => column(chunk, col)?.push_timestamp_micros(t + oid::PG_EPOCH_MICROS)?,
            },
            None => push_malformed(chunk, col, bytes)?,
        },
        ColumnKind::Binary => {
            chunk.push_bytes(col, bytes)?;
        }
        ColumnKind::Utf8 => match oid_value {
            // Hot path: for text the binary format **is** the UTF-8 bytes.
            // No conversion, no allocation: they are copied into the
            // contiguous buffer and that is it.
            oid::TEXT
            | oid::VARCHAR
            | oid::BPCHAR
            | oid::NAME
            | oid::CHAR
            | oid::XML
            | oid::JSON => {
                chunk.push_text_bytes(col, bytes)?;
            }
            oid::JSONB => {
                chunk.push_text_bytes(col, decode::jsonb_text(bytes))?;
            }
            _ => {
                let text = decode::decode_scalar_to_text(oid_value, bytes);
                match text {
                    Some(t) => chunk.push_text(col, &t)?,
                    // Unknown OID: the binary format cannot be interpreted.
                    // Explicit and ugly rendering instead of silently wrong.
                    None => {
                        let hex = alamo_core::value::encode_hex_escape(bytes);
                        chunk.push_text(col, &hex)?;
                    }
                }
            }
        },
    }
    Ok(())
}

fn column(
    chunk: &mut ChunkBuilder,
    col: usize,
) -> Result<&mut alamo_core::column::ColumnBuilder, PgError> {
    chunk
        .column_mut(col)
        .ok_or(PgError::Core(alamo_core::CoreError::ColumnOutOfRange {
            index: col,
        }))
}

/// Malformed cell (§7.3): explicit and ugly `\x…` rendering instead of an
/// aborted stream — the chunks already delivered stay valid and the other
/// cells keep arriving. The misalignment stays visible in the grid, as for
/// unknown OIDs.
fn push_malformed(chunk: &mut ChunkBuilder, col: usize, bytes: &[u8]) -> Result<(), PgError> {
    let hex = alamo_core::value::encode_hex_escape(bytes);
    chunk.push_degraded_text(col, &hex).map_err(PgError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary that does not respect the type's format must no longer
    /// produce `Err` (which would abort the stream): it degrades the cell.
    #[test]
    fn malformed_cell_degrades_instead_of_erroring() {
        let kinds = [ColumnKind::I32];
        let mut chunk = ChunkBuilder::new(&kinds, ChunkLimits::default());
        // 2-byte int4: framing of the value is broken, not of the protocol.
        push_value(&mut chunk, 0, ColumnKind::I32, oid::INT4, &[0x01, 0x02])
            .expect("the malformed cell degrades, it does not abort");
        chunk.end_row();
        assert_eq!(chunk.rows(), 1);
    }

    /// An index outside the schema remains a hard error: it is our own bug,
    /// not data from the server.
    #[test]
    fn column_out_of_range_is_still_a_hard_error() {
        let kinds = [ColumnKind::I32];
        let mut chunk = ChunkBuilder::new(&kinds, ChunkLimits::default());
        let res = push_value(
            &mut chunk,
            5,
            ColumnKind::I32,
            oid::INT4,
            &1i32.to_be_bytes(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn plan_line_rows_reads_only_node_lines() {
        // Classic node line.
        assert_eq!(
            plan_line_rows("Seq Scan on t  (cost=0.00..35.50 rows=2550 width=4)"),
            Some(2550)
        );
        // ModifyTable node of PG >= 14: rows=0 is a valid value.
        assert_eq!(
            plan_line_rows("Update on t  (cost=0.00..8.02 rows=0 width=0)"),
            Some(0)
        );
        // Filter line echoing a user literal: it is NOT an estimate.
        assert_eq!(
            plan_line_rows("  Filter: (note = 'rows=999999'::text)"),
            None
        );
        assert_eq!(
            plan_line_rows("  Index Cond: (k = 'rows=777777'::text)"),
            None
        );
        // Literal on a node line: reading starts from (cost= onwards, and
        // the real rows= is there anyway.
        assert_eq!(
            plan_line_rows("Seq Scan on \"rows=1 tbl\"  (cost=0.00..35.50 rows=42 width=4)"),
            Some(42)
        );
        assert_eq!(plan_line_rows("Planning Time: 0.2 ms"), None);
    }
}
