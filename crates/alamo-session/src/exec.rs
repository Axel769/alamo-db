//! Execution of a batch: split, per-statement streaming, outcomes (§6.4, §7).

use std::sync::Arc;
use std::time::{Duration, Instant};

use alamo_core::error::{DbError, ErrorCategory, Severity};
use alamo_core::result_set::ResultSetState;
use alamo_core::splitter::split_statements;
use alamo_core::stream::{FetchOptions, StreamEvent};
use tokio::sync::mpsc;

use crate::driver::DbConn;
use crate::events::{state_str, ColumnOut, ErrorOut, ExecEvent, OutcomeOut};
use crate::registry::{ExecState, LiveResult, Registry};
use crate::txn::TxnTracker;

/// Event emission callback: the `app` crate implements it on the tauri
/// channel; tests implement it on a `Vec`.
pub type Emit = Arc<dyn Fn(ExecEvent) + Send + Sync>;

/// Transaction context (§9.7): present only for user SQL on the sticky
/// connection — the auxiliary lanes stay in auto-commit.
pub struct TxnCtx {
    pub tracker: Arc<TxnTracker>,
    /// Manual commit (prod, or the toolbar switch): with no transaction
    /// open, implicit `BEGIN` before the first statement that writes or
    /// locks rows (§9.7 smart commit, `TxnTracker::needs_begin`). A separate
    /// statement of the app — user SQL is never rewritten (§5.2).
    pub manual: bool,
}

/// Settings of an execution: fetch options, pause watchdog and the optional
/// transaction context (§9.7, sticky only).
pub struct ExecConfig {
    pub opts: FetchOptions,
    pub pause_timeout: Duration,
    pub txn: Option<TxnCtx>,
    /// Read-only on PG: the batch runs inside the app's `BEGIN READ ONLY` …
    /// `COMMIT`. The session variable `default_transaction_read_only` can be
    /// switched off by any `set_config` inside a SELECT; the READ ONLY of an
    /// already started transaction cannot.
    pub read_only_txn: bool,
}

/// RAII guard on the state machine of §7.2: if the `run_execution` task dies
/// without a terminal outcome (panic in the emit callback, driver bug, task
/// dropped by `Registry::abort_all`), the entry would stay non-terminal and
/// the lane busy forever. On Drop, an execution still open ends `Cancelled`
/// if a cancel was asked, `Failed` otherwise, and the UI gets `Finished`
/// (not during a panic: the emit itself may be what panicked). With a
/// terminal outcome already written it is a no-op.
struct ExecGuard {
    registry: Arc<Registry>,
    exec_id: u64,
    emit: Emit,
}

impl Drop for ExecGuard {
    fn drop(&mut self) {
        let state = match self.registry.exec_state(self.exec_id) {
            Some(s) if s.is_terminal() => return,
            Some(ExecState::Cancelling) => ExecState::Cancelled,
            _ => ExecState::Failed,
        };
        self.registry.set_exec_state(self.exec_id, state);
        if !std::thread::panicking() {
            (self.emit)(ExecEvent::Finished {
                exec_id: self.exec_id,
                state: state.as_str(),
            });
        }
    }
}

/// Starts `run_execution` in its own task, which the registry can drop
/// (`Registry::abort_all`).
pub fn spawn_execution(
    conn: DbConn,
    registry: Arc<Registry>,
    exec_id: u64,
    sql: String,
    cfg: ExecConfig,
    emit: Emit,
) {
    let task = tokio::spawn(run_execution(
        conn,
        Arc::clone(&registry),
        exec_id,
        sql,
        cfg,
        emit,
    ));
    registry.set_task(exec_id, task.abort_handle());
}

/// The implicit `BEGIN` of the manual-commit regime (§9.7). `false` = it
/// failed, and the error has reached the UI as a notice of statement `index`.
async fn implicit_begin(
    conn: &DbConn,
    opts: &FetchOptions,
    tracker: &TxnTracker,
    emit: &Emit,
    exec_id: u64,
    index: u32,
) -> bool {
    let (_ctl, mut ctl_rx) = mpsc::channel(1);
    let error = match conn
        .execute_streaming("BEGIN", opts, &mut ctl_rx, &mut |_| {})
        .await
    {
        Ok(out) => match out.error {
            None => {
                tracker.apply_statement("BEGIN", true, out.in_transaction, None);
                return true;
            }
            Some(e) => e,
        },
        Err(e) => *e,
    };
    emit(ExecEvent::Notice {
        exec_id,
        index,
        notice: ErrorOut::from(&error),
    });
    false
}

/// Runs the whole batch. To be launched in a dedicated task; the exec_id has
/// already been handed to the caller (§8: `execute` returns immediately).
async fn run_execution(
    conn: DbConn,
    registry: Arc<Registry>,
    exec_id: u64,
    sql: String,
    cfg: ExecConfig,
    emit: Emit,
) {
    let _guard = ExecGuard {
        registry: Arc::clone(&registry),
        exec_id,
        emit: Arc::clone(&emit),
    };
    let ExecConfig {
        opts,
        pause_timeout,
        txn,
        read_only_txn,
    } = cfg;
    let statements = split_statements(&sql, conn.dialect().capabilities().backslash_escapes);
    emit(ExecEvent::Started {
        exec_id,
        statement_count: statements.len() as u32,
    });
    registry.set_exec_state(exec_id, ExecState::Running);

    let mut batch_failed = false;
    // Cancellation also observed locally: the registry may prune the
    // terminal entry as soon as another execution starts, and this task
    // must not resume executing because `exec_state` answers `None`.
    let mut batch_cancelled = false;
    let mut skip_from: Option<usize> = None;

    // If the read-only transaction does not start, nothing starts: never
    // execute outside a safety net the user believes is in place.
    let mut read_only_open = false;
    if read_only_txn && !batch_failed {
        match conn.begin_read_only_txn().await {
            Ok(()) => read_only_open = true,
            Err(e) => {
                emit(ExecEvent::Notice {
                    exec_id,
                    index: 0,
                    notice: ErrorOut::from(&*e),
                });
                batch_failed = true;
            }
        }
    }

    for (i, stmt) in statements.iter().enumerate() {
        let index = i as u32;
        // Stop at the first error (§6.4) or cancellation: the rest is Skipped,
        // each statement with its own outcome, never an aggregate.
        let cancelling = matches!(
            registry.exec_state(exec_id),
            Some(ExecState::Cancelling) | Some(ExecState::Cancelled)
        );
        if batch_failed || batch_cancelled || cancelling {
            skip_from = Some(i);
            break;
        }
        // §9.7 smart commit: the implicit BEGIN goes right before the first
        // statement that needs it, so the SELECTs before it see current data.
        // If it fails (dead connection), this statement and the rest are
        // Skipped: never run outside a transaction what the user expects to
        // be protected.
        if let Some(ctx) = txn.as_ref().filter(|c| c.manual) {
            if ctx.tracker.needs_begin(&stmt.sql)
                && !implicit_begin(&conn, &opts, &ctx.tracker, &emit, exec_id, index).await
            {
                batch_failed = true;
                skip_from = Some(i);
                break;
            }
        }

        emit(ExecEvent::StatementStarted {
            exec_id,
            index,
            char_offset: stmt.origin.char_offset,
            byte_offset: stmt.origin.byte_offset,
        });
        let started = Instant::now();

        let (control_tx, mut control_rx) = mpsc::channel(4);
        registry.set_control(exec_id, control_tx);

        // State shared between the driver loop (sync callback) and this task.
        let mut live: Option<(u64, LiveResult)> = None;
        // Last wire total seen: the driver reports the TOTAL of the
        // statement, the result set gets the per-chunk delta.
        let mut wire_seen: u64 = 0;
        let registry_cb = Arc::clone(&registry);
        let emit_cb = Arc::clone(&emit);
        let conn_cb = conn.clone();

        let dialect = conn.dialect();
        let outcome = conn
            .execute_streaming(&stmt.sql, &opts, &mut control_rx, &mut |ev| match ev {
                StreamEvent::Schema { schema, .. } => {
                    if !schema.is_empty() {
                        let l = LiveResult::new(Arc::clone(&schema));
                        let result_id = registry_cb.new_result(l.clone());
                        registry_cb.set_live(exec_id, l.clone());
                        emit_cb(ExecEvent::Schema {
                            exec_id,
                            index,
                            result_id,
                            columns: schema
                                .iter()
                                .map(|m| ColumnOut::new(m, dialect.column_kind(&m.native_type)))
                                .collect(),
                        });
                        live = Some((result_id, l));
                    }
                }
                StreamEvent::Chunk {
                    chunk,
                    stats,
                    rows_total,
                    layout_bytes_total,
                    wire_bytes_total,
                } => {
                    if let Some((result_id, l)) = &live {
                        // Saturating: robust to a total restarting from
                        // zero, never an inflated delta.
                        let delta = wire_bytes_total.saturating_sub(wire_seen);
                        wire_seen = wire_bytes_total;
                        l.push_chunk(chunk, stats, delta);
                        emit_cb(ExecEvent::Rows {
                            exec_id,
                            result_id: *result_id,
                            rows: rows_total,
                            layout_bytes: layout_bytes_total,
                        });
                    }
                    drain_notices(&conn_cb, &emit_cb, exec_id, index);
                }
                StreamEvent::BudgetReached {
                    rows,
                    layout_bytes,
                    by_rows,
                } => {
                    if let Some((result_id, l)) = &live {
                        l.set_state(ResultSetState::Paused);
                        emit_cb(ExecEvent::BudgetPaused {
                            exec_id,
                            result_id: *result_id,
                            rows,
                            layout_bytes,
                            by_rows,
                        });
                        // §5.1: a paused fetch keeps the portal and its
                        // implicit transaction open, which blocks vacuum on
                        // the server. If the user does not answer the banner
                        // within the timeout, the fetch stops by itself
                        // (= "Stop": partial rows kept, truncated state).
                        let gen = registry_cb.mark_paused(exec_id);
                        let reg = Arc::clone(&registry_cb);
                        let em = Arc::clone(&emit_cb);
                        tokio::spawn(async move {
                            tokio::time::sleep(pause_timeout).await;
                            if reg.is_still_paused(exec_id, gen) {
                                em(ExecEvent::Notice {
                                    exec_id,
                                    index,
                                    notice: ErrorOut::from(&pause_timeout_notice(pause_timeout)),
                                });
                                reg.budget_stop(exec_id);
                            }
                        });
                    }
                }
            })
            .await
            .map_err(|e| *e);

        drain_notices(&conn, &emit, exec_id, index);
        // §11: in MySQL warnings are requested with SHOW WARNINGS, after a
        // successful statement (after an error the queue holds the error
        // itself; after a stop, the `KILL QUERY` that freed the connection).
        if outcome
            .as_ref()
            .map(|o| o.error.is_none() && o.state == ResultSetState::Complete)
            .unwrap_or(false)
        {
            for w in conn.post_statement_warnings().await {
                emit(ExecEvent::Notice {
                    exec_id,
                    index,
                    notice: ErrorOut::from(&w),
                });
            }
        }
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        // §9.7: the transaction state is updated at every statement — truth
        // from the protocol when available (MySQL), txn control tracking
        // otherwise (PG). Cancelled or failed statement = not succeeded.
        if let Some(ctx) = &txn {
            let (ok, srv, rows) = match &outcome {
                Ok(o) => (o.error.is_none(), o.in_transaction, o.rows_affected),
                Err(_) => (false, None, None),
            };
            ctx.tracker.apply_statement(&stmt.sql, ok, srv, rows);
        }

        match outcome {
            Err(e) => {
                // If the `Schema` had already arrived, the `LiveResult` exists
                // and must be closed HERE: otherwise it would stay `Streaming`
                // forever in the registry. Partial rows are kept, marked (§7.3).
                let mut db = e;
                db.statement = Some(stmt.origin.clone());
                let outcome = if cancelled_via_transport(&registry, exec_id, &db) {
                    let (result_id, rows) = match &live {
                        Some((id, l)) => {
                            l.finish(ResultSetState::PartialCancelled, Some(db));
                            (Some(*id), registry.result(*id).map_or(0, |r| r.rows()))
                        }
                        None => (None, 0),
                    };
                    registry.set_exec_state(exec_id, ExecState::Cancelled);
                    batch_cancelled = true;
                    OutcomeOut::Cancelled { result_id, rows }
                } else {
                    // `Truncated`: the same state the drivers give to a
                    // stream closed by a non-cancellation error (§7.3).
                    if let Some((_, l)) = &live {
                        l.finish(ResultSetState::Truncated, Some(db.clone()));
                    }
                    batch_failed = true;
                    OutcomeOut::Failed {
                        error: ErrorOut::from(&db),
                    }
                };
                emit(ExecEvent::StatementFinished {
                    exec_id,
                    index,
                    outcome,
                    elapsed_ms,
                });
            }
            Ok(out) => {
                let outcome_out = match (out.error, &live) {
                    (Some(mut db), l) => {
                        db.statement = Some(stmt.origin.clone());
                        if db.is_cancellation() || cancelled_via_transport(&registry, exec_id, &db)
                        {
                            let (result_id, rows) = match l {
                                Some((id, l)) => {
                                    l.finish(ResultSetState::PartialCancelled, Some(db));
                                    (Some(*id), registry.result(*id).map_or(0, |r| r.rows()))
                                }
                                None => (None, 0),
                            };
                            registry.set_exec_state(exec_id, ExecState::Cancelled);
                            batch_cancelled = true;
                            OutcomeOut::Cancelled { result_id, rows }
                        } else {
                            // SQL error with the stream started: the chunks
                            // already built stay readable, marked (§7.3).
                            if let Some((_, l)) = l {
                                l.finish(out.state, Some(db.clone()));
                            }
                            batch_failed = true;
                            OutcomeOut::Failed {
                                error: ErrorOut::from(&db),
                            }
                        }
                    }
                    (None, Some((result_id, l))) => {
                        l.finish(out.state, None);
                        if out.state == ResultSetState::PartialCancelled {
                            registry.set_exec_state(exec_id, ExecState::Cancelled);
                            batch_cancelled = true;
                            OutcomeOut::Cancelled {
                                result_id: Some(*result_id),
                                rows: out.stats.rows,
                            }
                        } else {
                            OutcomeOut::RowSet {
                                result_id: *result_id,
                                rows: out.stats.rows,
                                state: state_str(out.state),
                            }
                        }
                    }
                    (None, None) => match out.rows_affected {
                        Some(n) => OutcomeOut::Affected { rows: n },
                        None => OutcomeOut::Void,
                    },
                };
                emit(ExecEvent::StatementFinished {
                    exec_id,
                    index,
                    outcome: outcome_out,
                    elapsed_ms,
                });
            }
        }
    }

    // Statements that never started, each with its own outcome (§6.4).
    if let Some(from) = skip_from {
        for i in from..statements.len() {
            emit(ExecEvent::StatementFinished {
                exec_id,
                index: i as u32,
                outcome: OutcomeOut::Skipped,
                elapsed_ms: 0.0,
            });
        }
    }

    // Closing the read-only transaction: COMMIT even after an error (on an
    // aborted transaction it is equivalent to ROLLBACK). After a
    // cancellation the connection may be dead: no noise.
    if read_only_open {
        if let Err(e) = conn.end_read_only_txn().await {
            if !batch_cancelled {
                emit(ExecEvent::Notice {
                    exec_id,
                    index: 0,
                    notice: ErrorOut::from(&*e),
                });
            }
        }
    }

    // `batch_cancelled` also covers the entry already pruned by the registry:
    // the outcome towards the UI stays "cancelled" even if `exec_state` is `None`.
    let final_state = if batch_cancelled
        || matches!(
            registry.exec_state(exec_id),
            Some(ExecState::Cancelling) | Some(ExecState::Cancelled)
        ) {
        ExecState::Cancelled
    } else if batch_failed {
        ExecState::Failed
    } else {
        ExecState::Completed
    };
    registry.set_exec_state(exec_id, final_state);
    emit(ExecEvent::Finished {
        exec_id,
        state: final_state.as_str(),
    });
}

/// §7.2, escalation: if the user has already asked for cancellation and the
/// connection dies (`pg_terminate_backend`, closed socket), the outcome is
/// "cancelled", not a red error — the death of the connection is exactly
/// what they asked for (§6.2).
fn cancelled_via_transport(registry: &Registry, exec_id: u64, db: &DbError) -> bool {
    db.category == ErrorCategory::Transport
        && matches!(registry.exec_state(exec_id), Some(ExecState::Cancelling))
}

/// Warning that accompanies the automatic stop of a paused fetch (§5.1).
fn pause_timeout_notice(timeout: Duration) -> DbError {
    let mut db = DbError::internal(format!(
        "Fetch stopped automatically after {} s paused without an answer: a suspended \
         fetch keeps a transaction open on the server and blocks vacuum (§5.1). \
         The rows already loaded stay readable.",
        timeout.as_secs()
    ));
    db.severity = Severity::Warning;
    db
}

fn drain_notices(conn: &DbConn, emit: &Emit, exec_id: u64, index: u32) {
    for n in conn.drain_async_notices() {
        emit(ExecEvent::Notice {
            exec_id,
            index,
            notice: ErrorOut::from(&n),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_canceller;

    #[test]
    fn guard_marks_failed_a_task_dead_without_outcome() {
        let registry = Arc::new(Registry::new());
        let exec_id = registry.try_new_execution(test_canceller()).unwrap();
        registry.set_exec_state(exec_id, ExecState::Running);
        // Simulates the task panic: the guard drops in a non-terminal state.
        drop(ExecGuard {
            registry: Arc::clone(&registry),
            exec_id,
            emit: Arc::new(|_| {}),
        });
        assert_eq!(
            registry.exec_state(exec_id),
            Some(ExecState::Failed),
            "the lane must not stay busy forever"
        );
        assert!(!registry.has_active_execution());
    }

    #[test]
    fn guard_does_not_touch_a_terminal_outcome() {
        let registry = Arc::new(Registry::new());
        let exec_id = registry.try_new_execution(test_canceller()).unwrap();
        registry.set_exec_state(exec_id, ExecState::Completed);
        drop(ExecGuard {
            registry: Arc::clone(&registry),
            exec_id,
            emit: Arc::new(|_| {}),
        });
        assert_eq!(registry.exec_state(exec_id), Some(ExecState::Completed));
    }

    #[tokio::test]
    async fn abort_all_ends_a_stuck_task_cancelled_and_tells_the_ui() {
        let registry = Arc::new(Registry::new());
        let exec_id = registry.try_new_execution(test_canceller()).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let guard = ExecGuard {
            registry: Arc::clone(&registry),
            exec_id,
            emit: Arc::new(move |ev| {
                let _ = tx.send(ev);
            }),
        };
        // A stream on a server that never answers: only the abort ends it.
        let task = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        registry.set_task(exec_id, task.abort_handle());
        registry.set_exec_state(exec_id, ExecState::Running);
        let live = LiveResult::new(Arc::from([]));
        registry.set_live(exec_id, live.clone());

        registry.abort_all();
        assert_eq!(
            live.state(),
            ResultSetState::PartialCancelled,
            "rows kept, marked partial"
        );
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(registry.exec_state(exec_id), Some(ExecState::Cancelled));
        assert!(!registry.has_active_execution(), "the sticky is free again");
        assert!(matches!(
            rx.recv().await,
            Some(ExecEvent::Finished {
                state: "cancelled",
                ..
            })
        ));
    }
}
