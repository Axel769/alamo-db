//! Streaming contract between driver and session (§5, §7).
//!
//! These types were born in the PostgreSQL driver and moved up here with M3:
//! the session must talk to **any** driver without knowing its protocol, and
//! the contract is made only of `core` types (chunks, statistics, errors) —
//! no external dependencies, as §2 requires.

use std::sync::Arc;
use std::time::Duration;

use crate::chunk::{Chunk, ChunkLimits};
use crate::error::DbError;
use crate::meta::{ColumnMeta, StatsAccumulator};
use crate::result_set::ResultSetState;

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub chunk_limits: ChunkLimits,
    /// Soft budget in **bytes**, not in rows (§5): 50 TEXT columns and 3 INT
    /// columns have the same row count and two orders of magnitude of
    /// difference in RAM. When reached, the fetch is **paused**, not
    /// interrupted.
    ///
    /// `None` disables the budget: needed for the pure throughput measurement
    /// of M0, where T2 at 1M rows exceeds it **on purpose**.
    pub soft_budget_bytes: Option<usize>,
    /// Row limit applied by **stopping the stream**, never by rewriting the
    /// user's SQL (§5.2: rewriting breaks edge cases and shifts the error
    /// positions).
    pub max_rows: Option<u64>,
    /// Pause at N rows (SQL tab): same mechanism as the soft budget — fetch
    /// stopped, the user decides — but counted in rows. `ResumeRows` grants
    /// another N rows, `Resume` removes it (the byte budget stays). The SQL
    /// is not touched.
    pub pause_after_rows: Option<u64>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        FetchOptions {
            chunk_limits: ChunkLimits::default(),
            soft_budget_bytes: Some(384 * 1024 * 1024),
            max_rows: None,
            pause_after_rows: None,
        }
    }
}

/// Raw measurements of a single execution. Feeds the thresholds of §15.
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    /// From the call to the first **decoded** row. It is the "first row on
    /// screen" metric minus the UI cost, which at M0 phase 1 does not exist.
    pub time_to_first_row: Option<Duration>,
    /// From the call to the end of the stream.
    pub total: Duration,
    /// Useful bytes arrived from the protocol. It is the denominator of
    /// "RAM / logical size".
    pub wire_bytes: u64,
    pub rows: u64,
    pub chunks: usize,
    /// Columns whose binary format the driver could not interpret (see the
    /// comment at the top of the PG driver's `decode.rs`).
    pub undecoded_columns: Vec<String>,
}

pub enum StreamEvent {
    Schema {
        schema: Arc<[ColumnMeta]>,
        undecoded_columns: Vec<String>,
    },
    Chunk {
        chunk: Chunk,
        stats: Vec<StatsAccumulator>,
        rows_total: u64,
        /// Accumulated bytes of the columnar layout: it is the quantity the
        /// soft budget reasons about (§5), not the bytes on the wire.
        layout_bytes_total: u64,
        wire_bytes_total: u64,
    },
    /// Soft budget reached: the driver is stopped waiting for
    /// `StreamControl::Resume` or `Stop`. The pause is real backpressure — the
    /// socket is no longer read and TCP slows the server down (§5).
    /// `by_rows` = the pause at N rows fired (`pause_after_rows`), not the
    /// byte budget.
    BudgetReached {
        rows: u64,
        layout_bytes: u64,
        by_rows: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// "Stop" of the §5 banner: partial data kept, state `Truncated`.
    UserStopped,
    /// §7: the server-side cancel has already been sent; here we stop consuming.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamControl {
    /// "Continue" / "Load all": the row pause goes away, the byte budget
    /// grants another increment.
    Resume,
    /// Scroll to the bottom of the grid: another `pause_after_rows` rows, then
    /// paused again (pages on demand, like DBeaver).
    ResumeRows,
    Stop(StopReason),
}

#[derive(Debug)]
pub struct StreamOutcome {
    pub state: ResultSetState,
    pub stats: ExecutionStats,
    /// SQL error arrived while streaming (the partial data stays, §7.3) or
    /// the translated cancellation (`57014` → `Cancelled`, §6.2).
    pub error: Option<DbError>,
    /// Rows touched according to the command tag: it is what distinguishes
    /// `Affected` from `Void` in outcomes without columns (§4.1).
    pub rows_affected: Option<u64>,
    /// §9.7: transaction open on the server AFTER this statement, when the
    /// driver knows it from the protocol (MySQL: `SERVER_STATUS_IN_TRANS` from
    /// the OK packet). `None` = the protocol does not expose it (tokio-postgres
    /// discards the ReadyForQuery status) or it was lost (MySQL error): the
    /// session's client-side tracking decides.
    pub in_transaction: Option<bool>,
}
