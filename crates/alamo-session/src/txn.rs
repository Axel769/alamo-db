//! Transaction state of the sticky connection — §9.7.
//!
//! Hybrid source of truth: where the protocol says so (MySQL,
//! `SERVER_STATUS_IN_TRANS` from the OK packet) the server wins; where it
//! does not (tokio-postgres discards the ReadyForQuery status) only
//! transaction CONTROL statements are tracked — classifiable with certainty
//! from the keyword, unlike read/write (§9.7.3). A failed statement does not
//! change the state: on PG the transaction stays open (aborted) until a
//! ROLLBACK closes it, and that is exactly what the tracking reflects.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use alamo_core::classify::{classify_statement, txn_control, TxnControl};

/// Snapshot of the state, serialized towards the UI.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TxnStateOut {
    /// Explicit transaction open on the sticky connection.
    pub open: bool,
    /// At least one write (or unclassifiable statement, which for the gates
    /// counts as a write) succeeded inside the transaction: lights up the
    /// Commit button (§9.7.2).
    pub has_writes: bool,
    /// How many: the number on the Commit button (the log keeps at most
    /// `LOG_MAX` of them).
    pub writes: u32,
    /// Milliseconds since it was opened, for the timer with the progressive warning.
    pub open_for_ms: Option<u64>,
    /// Milliseconds since the last statement on the sticky: at
    /// `IDLE_ROLLBACK` the transaction is rolled back (§9.7.2).
    pub idle_for_ms: Option<u64>,
    /// `IDLE_ROLLBACK` in milliseconds, so the UI counts down to it.
    pub idle_rollback_ms: u64,
}

/// Idle transaction rolled back by the client (§9.7.2): before the
/// server-side net (`IDLE_TXN_TIMEOUT_SECS`), and on MySQL, which has none.
pub const IDLE_ROLLBACK: Duration = Duration::from_secs(900);

/// Transaction log (§9.7.2): the first `LOG_MAX` writes, each cut at
/// `LOG_SQL_MAX` characters — a 10,000-INSERT script must not keep its
/// whole text in memory to be listed before Commit.
const LOG_MAX: usize = 500;
const LOG_SQL_MAX: usize = 2000;

/// A write of the open transaction, as listed before Commit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TxnWrite {
    pub sql: String,
    /// Cut at `LOG_SQL_MAX` characters.
    pub truncated: bool,
    /// Rows affected, when the server says so.
    pub rows: Option<u64>,
    /// Milliseconds since the transaction was opened.
    pub at_ms: u64,
}

/// The log of the open transaction: `total` counts the writes past `LOG_MAX` too.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TxnLogOut {
    pub writes: Vec<TxnWrite>,
    pub total: u32,
}

#[derive(Debug)]
struct Inner {
    open: bool,
    writes: Vec<TxnWrite>,
    total: u32,
    started_at: Option<Instant>,
    last_activity: Option<Instant>,
}

impl Inner {
    const CLOSED: Inner = Inner {
        open: false,
        writes: Vec::new(),
        total: 0,
        started_at: None,
        last_activity: None,
    };
}

fn millis(t: Option<Instant>) -> Option<u64> {
    t.map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
}

#[derive(Debug)]
pub struct TxnTracker {
    inner: Mutex<Inner>,
    /// Transactional DDL of the dialect: feeds the classifier.
    transactional_ddl: bool,
}

impl TxnTracker {
    pub fn new(transactional_ddl: bool) -> Self {
        TxnTracker {
            inner: Mutex::new(Inner::CLOSED),
            transactional_ddl,
        }
    }

    pub fn snapshot(&self) -> TxnStateOut {
        let g = self.inner.lock().unwrap();
        TxnStateOut {
            open: g.open,
            has_writes: g.total > 0,
            writes: g.total,
            open_for_ms: millis(g.started_at),
            idle_for_ms: millis(g.last_activity),
            idle_rollback_ms: IDLE_ROLLBACK.as_millis() as u64,
        }
    }

    pub fn log(&self) -> TxnLogOut {
        let g = self.inner.lock().unwrap();
        TxnLogOut {
            writes: g.writes.clone(),
            total: g.total,
        }
    }

    /// Open transaction with no statement for `limit`.
    pub fn idle_expired(&self, limit: Duration) -> bool {
        let g = self.inner.lock().unwrap();
        g.open && g.last_activity.is_some_and(|t| t.elapsed() >= limit)
    }

    /// Updates the state after a statement executed on the sticky connection.
    ///
    /// `server_in_txn`: truth from the protocol when available — also covers
    /// the implicit commit of MySQL DDL (§9.7.1), which client-side tracking
    /// would not see. `None` = the tracking decides. `rows`: affected, for the log.
    pub fn apply_statement(
        &self,
        sql: &str,
        succeeded: bool,
        server_in_txn: Option<bool>,
        rows: Option<u64>,
    ) {
        let mut g = self.inner.lock().unwrap();
        let was_open = g.open;

        match server_in_txn {
            Some(open) => g.open = open,
            None => {
                if succeeded {
                    match txn_control(sql) {
                        Some(TxnControl::Begin) => g.open = true,
                        Some(TxnControl::End { chain }) => g.open = chain,
                        Some(TxnControl::Savepoint) | None => {}
                    }
                }
                // Failed: state unchanged — on PG the open transaction
                // stays open (aborted), never closed by an error.
            }
        }

        // `COMMIT AND CHAIN` closes and reopens: for the timer and the writes
        // it is a NEW transaction even though `open` stays true.
        let chained = matches!(txn_control(sql), Some(TxnControl::End { chain: true }));
        if g.open && (!was_open || (succeeded && chained)) {
            g.started_at = Some(Instant::now());
            g.writes.clear();
            g.total = 0;
        }
        if !g.open {
            g.started_at = None;
            g.writes.clear();
            g.total = 0;
        }
        g.last_activity = g.open.then(Instant::now);

        if g.open
            && succeeded
            && classify_statement(sql, self.transactional_ddl)
                .class
                .counts_as_write()
        {
            g.total = g.total.saturating_add(1);
            if g.writes.len() < LOG_MAX {
                let text = sql.trim();
                let truncated = text.chars().count() > LOG_SQL_MAX;
                let at_ms = millis(g.started_at).unwrap_or(0);
                g.writes.push(TxnWrite {
                    sql: text.chars().take(LOG_SQL_MAX).collect(),
                    truncated,
                    rows,
                    at_ms,
                });
            }
        }
    }

    /// The connection died or was reopened: any transaction is lost.
    pub fn reset(&self) {
        *self.inner.lock().unwrap() = Inner::CLOSED;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_client_side_round_trip() {
        let t = TxnTracker::new(true);
        assert!(!t.snapshot().open);
        t.apply_statement("BEGIN", true, None, None);
        assert!(t.snapshot().open);
        assert!(!t.snapshot().has_writes);
        t.apply_statement("SELECT 1", true, None, None);
        assert!(!t.snapshot().has_writes, "a read does not light up Commit");
        t.apply_statement("UPDATE t SET a=1 WHERE id=2", true, None, None);
        assert!(t.snapshot().has_writes);
        t.apply_statement("COMMIT", true, None, None);
        let s = t.snapshot();
        assert!(!s.open);
        assert!(!s.has_writes);
    }

    #[test]
    fn error_does_not_close_the_transaction() {
        let t = TxnTracker::new(true);
        t.apply_statement("BEGIN", true, None, None);
        t.apply_statement("SELECT sbagliata", false, None, None);
        assert!(t.snapshot().open, "on PG it stays open (aborted)");
        t.apply_statement("ROLLBACK", true, None, None);
        assert!(!t.snapshot().open);
    }

    #[test]
    fn server_truth_wins_and_covers_mysql_ddl() {
        let t = TxnTracker::new(false);
        t.apply_statement("BEGIN", true, Some(true), None);
        t.apply_statement("INSERT INTO t VALUES (1)", true, Some(true), None);
        assert!(t.snapshot().has_writes);
        // MySQL DDL: implicit commit — the server says "outside transaction"
        // even though no COMMIT went through.
        t.apply_statement("CREATE TABLE x (id int)", true, Some(false), None);
        let s = t.snapshot();
        assert!(!s.open);
        assert!(!s.has_writes);
    }

    #[test]
    fn unknown_statement_counts_as_write() {
        let t = TxnTracker::new(true);
        t.apply_statement("BEGIN", true, None, None);
        t.apply_statement("CALL opaca()", true, None, None);
        assert!(t.snapshot().has_writes, "\u{a7}9.7.3: unknown = write");
    }

    #[test]
    fn idle_counts_from_the_last_statement() {
        let t = TxnTracker::new(true);
        assert!(
            !t.idle_expired(Duration::ZERO),
            "no transaction, nothing to expire"
        );
        t.apply_statement("BEGIN", true, None, None);
        assert!(t.idle_expired(Duration::ZERO));
        assert!(!t.idle_expired(Duration::from_secs(60)));
        assert!(t.snapshot().idle_for_ms.is_some());
        t.apply_statement("COMMIT", true, None, None);
        assert!(!t.idle_expired(Duration::ZERO));
        assert_eq!(t.snapshot().idle_for_ms, None);
    }

    #[test]
    fn log_lists_the_writes_of_the_open_transaction() {
        let t = TxnTracker::new(true);
        t.apply_statement("BEGIN", true, None, None);
        t.apply_statement("SELECT 1", true, None, None);
        t.apply_statement("  UPDATE t SET a=1 WHERE id=2 ", true, None, Some(1));
        t.apply_statement("DELETE FROM t WHERE x", false, None, None);
        let log = t.log();
        assert_eq!(log.total, 1, "reads and failed statements are not listed");
        assert_eq!(log.writes[0].sql, "UPDATE t SET a=1 WHERE id=2");
        assert_eq!(log.writes[0].rows, Some(1));
        assert_eq!(t.snapshot().writes, 1);

        let long = format!("INSERT INTO t VALUES ('{}')", "x".repeat(LOG_SQL_MAX));
        for _ in 0..LOG_MAX + 5 {
            t.apply_statement(&long, true, None, Some(1));
        }
        let log = t.log();
        assert_eq!(log.writes.len(), LOG_MAX, "the log is capped");
        assert_eq!(log.total as usize, LOG_MAX + 6, "the count is not");
        assert!(log.writes[1].truncated);
        assert_eq!(log.writes[1].sql.chars().count(), LOG_SQL_MAX);

        t.apply_statement("COMMIT", true, None, None);
        assert_eq!(t.log().total, 0);
        assert!(t.log().writes.is_empty());
    }

    #[test]
    fn reset_clears_everything() {
        // Dead connection (terminate §7.2, transport broken on COMMIT):
        // the UI must not keep showing a ghost transaction.
        let t = TxnTracker::new(true);
        t.apply_statement("BEGIN", true, None, None);
        t.apply_statement("INSERT INTO t VALUES (1)", true, None, None);
        t.reset();
        let s = t.snapshot();
        assert!(!s.open);
        assert!(!s.has_writes);
        assert!(s.open_for_ms.is_none());
    }

    #[test]
    fn and_chain_reopens_and_clears() {
        let t = TxnTracker::new(true);
        t.apply_statement("BEGIN", true, None, None);
        t.apply_statement("INSERT INTO t VALUES (1)", true, None, None);
        t.apply_statement("COMMIT AND CHAIN", true, None, None);
        let s = t.snapshot();
        assert!(s.open, "AND CHAIN reopens immediately");
        assert!(!s.has_writes, "but the new transaction is clean");
    }
}
