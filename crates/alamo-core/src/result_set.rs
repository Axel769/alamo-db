//! Result set and execution outcome — §4.1, §4.8, §5.
//!
//! An execution does **not** necessarily produce a result set, and it can
//! produce **more than one** (`CALL` of a MySQL stored procedure): that is why
//! the outcome is a `Vec<ExecutionOutcome>` from the start, not an `Option`.

use std::sync::Arc;

use crate::chunk::Chunk;
use crate::error::DbError;
use crate::meta::{ColumnMeta, StatsAccumulator};
use crate::value::ValueRef;

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    /// Boxed: a `ResultSet` is much larger than the other variants, and
    /// without the box every `Affected`/`Void` would pay the size of the
    /// worst case.
    RowSet(Box<ResultSet>),
    /// DML.
    Affected {
        rows: u64,
        last_insert_id: Option<u64>,
    },
    /// DDL, `SET`, …
    Void,
}

/// State of a stream. It is not cosmetic: §7.3 requires the incompleteness
/// flag to propagate to **local sort, filter and export**, otherwise false
/// results are produced that look like real ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultSetState {
    Streaming,
    /// Soft budget reached: the fetch is **paused, not interrupted** (§5).
    /// The user decides: Continue / Stop / Export to file.
    Paused,
    Complete,
    /// Stopped by a limit, not by the user.
    Truncated,
    /// Cancelled midway: the partial data is kept and the result set is
    /// marked incomplete (§7.3).
    PartialCancelled,
}

impl ResultSetState {
    /// True when the content does **not** represent the whole result of the
    /// query. Sort, filter and export must tell the user.
    pub fn is_incomplete(&self) -> bool {
        !matches!(self, ResultSetState::Complete)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet {
    /// Captured **once only, never per chunk** (§4.1).
    schema: Arc<[ColumnMeta]>,
    chunks: Vec<Chunk>,
    /// Prefix sum: `row_starts[i]` is the first global row of chunk *i*.
    /// Length `chunks.len() + 1`, the last element is the total row count.
    row_starts: Vec<u64>,
    state: ResultSetState,
    /// Error that closed the stream, if there was one.
    error: Option<DbError>,
}

impl ResultSet {
    pub fn new(schema: Arc<[ColumnMeta]>) -> Self {
        ResultSet {
            schema,
            chunks: Vec::new(),
            row_starts: vec![0],
            state: ResultSetState::Streaming,
            error: None,
        }
    }

    pub fn schema(&self) -> &[ColumnMeta] {
        &self.schema
    }

    pub fn schema_arc(&self) -> Arc<[ColumnMeta]> {
        Arc::clone(&self.schema)
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    pub fn state(&self) -> ResultSetState {
        self.state
    }

    pub fn set_state(&mut self, state: ResultSetState) {
        self.state = state;
    }

    pub fn error(&self) -> Option<&DbError> {
        self.error.as_ref()
    }

    pub fn set_error(&mut self, error: DbError) {
        self.error = Some(error);
    }

    pub fn push_chunk(&mut self, chunk: Chunk) {
        let total = self.rows() + chunk.rows() as u64;
        self.chunks.push(chunk);
        self.row_starts.push(total);
    }

    /// Rows **loaded**, which is not the query total.
    ///
    /// §5.2: no automatic `SELECT COUNT(*)`. The grid must live with an
    /// unknown total — Glide is given this number and "1,000,000+" is shown
    /// while the stream is open.
    pub fn rows(&self) -> u64 {
        *self.row_starts.last().unwrap_or(&0)
    }

    /// True if the row count shown must be rendered as "N+" and not as a total.
    pub fn row_count_is_partial(&self) -> bool {
        self.state.is_incomplete()
    }

    /// Global row -> (chunk index, offset in the chunk). Binary search on the
    /// prefix sum (§4.8): **once per window, not per cell**.
    pub fn locate(&self, row: u64) -> Option<(usize, usize)> {
        if row >= self.rows() {
            return None;
        }
        // partition_point returns the first index with start > row.
        let idx = self.row_starts.partition_point(|&start| start <= row) - 1;
        Some((idx, (row - self.row_starts[idx]) as usize))
    }

    pub fn value_at(&self, row: u64, col: usize) -> Option<ValueRef<'_>> {
        let (chunk_idx, offset) = self.locate(row)?;
        self.chunks[chunk_idx].value_at(offset, col)
    }

    /// §4.8: the result set sums the chunks. Computed from the layout.
    pub fn byte_size(&self) -> usize {
        self.chunks.iter().map(|c| c.byte_size()).sum::<usize>()
            + self.row_starts.capacity() * std::mem::size_of::<u64>()
    }

    fn schema_mut(&mut self) -> &mut [ColumnMeta] {
        // `Arc<[T]>` has no `make_mut`. The schema is touched very few times
        // (degradation, statistics) and always with the chunk closed, so the
        // occasional clone is irrelevant.
        if Arc::get_mut(&mut self.schema).is_none() {
            self.schema = self.schema.iter().cloned().collect::<Vec<_>>().into();
        }
        Arc::get_mut(&mut self.schema).expect("just made unique")
    }
}

/// Assembles a `ResultSet` while the stream flows.
///
/// Keeps out of the `ResultSet` two things needed only during construction:
/// the per-column statistics accumulators (§4.3) and the count of raw bytes
/// arrived from the wire, which feeds the soft budget of §5.
#[derive(Debug)]
pub struct ResultSetBuilder {
    result: ResultSet,
    stats: Vec<StatsAccumulator>,
    wire_bytes: u64,
}

impl ResultSetBuilder {
    pub fn new(schema: Arc<[ColumnMeta]>) -> Self {
        let stats = (0..schema.len())
            .map(|_| StatsAccumulator::new(true))
            .collect();
        ResultSetBuilder {
            result: ResultSet::new(schema),
            stats,
            wire_bytes: 0,
        }
    }

    /// Protocol bytes consumed. It is the denominator of the
    /// "RAM / logical size" metric of §15.
    pub fn add_wire_bytes(&mut self, n: u64) {
        self.wire_bytes += n;
    }

    pub fn wire_bytes(&self) -> u64 {
        self.wire_bytes
    }

    pub fn rows(&self) -> u64 {
        self.result.rows()
    }

    pub fn byte_size(&self) -> usize {
        self.result.byte_size()
    }

    /// Adds a closed chunk, merging its statistics and degradations into the
    /// schema.
    pub fn push_chunk(&mut self, chunk: Chunk, chunk_stats: Vec<StatsAccumulator>) {
        for (i, acc) in chunk_stats.iter().enumerate() {
            if let Some(dst) = self.stats.get_mut(i) {
                dst.merge(acc);
            }
        }
        {
            let schema = self.result.schema_mut();
            for (i, col) in chunk.columns().iter().enumerate() {
                if col.degraded() {
                    if let Some(meta) = schema.get_mut(i) {
                        meta.degraded = true;
                    }
                }
            }
        }
        let finished: Vec<_> = self.stats.iter().map(|a| a.finish()).collect();
        let schema = self.result.schema_mut();
        for (i, meta) in schema.iter_mut().enumerate() {
            if let Some(s) = finished.get(i) {
                meta.stats = *s;
            }
        }
        self.result.push_chunk(chunk);
    }

    /// View of the result set under construction: it is what lets
    /// `fetch_window` run **while the stream is still open** (§8).
    pub fn result(&self) -> &ResultSet {
        &self.result
    }

    /// State visible during construction (`Streaming` ↔ `Paused`, §5).
    pub fn set_state(&mut self, state: ResultSetState) {
        self.result.set_state(state);
    }

    pub fn finish(mut self, state: ResultSetState) -> ResultSet {
        self.result.set_state(state);
        self.result
    }

    pub fn finish_with_error(mut self, state: ResultSetState, error: DbError) -> ResultSet {
        self.result.set_state(state);
        self.result.set_error(error);
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkBuilder, ChunkLimits};
    use crate::column::ColumnKind;
    use crate::meta::{ColumnMeta, NativeType};

    fn schema() -> Arc<[ColumnMeta]> {
        vec![ColumnMeta::new(
            "n",
            NativeType::Postgres {
                oid: 20,
                type_modifier: -1,
            },
            "int8",
        )]
        .into()
    }

    fn chunk_of(range: std::ops::Range<i64>) -> Chunk {
        let mut b = ChunkBuilder::new(&[ColumnKind::I64], ChunkLimits::default());
        for i in range {
            b.column_mut(0).unwrap().push_i64(i).unwrap();
            b.end_row();
        }
        b.finish().0
    }

    #[test]
    fn locate_spans_chunks() {
        let mut rs = ResultSet::new(schema());
        rs.push_chunk(chunk_of(0..10));
        rs.push_chunk(chunk_of(10..25));
        rs.push_chunk(chunk_of(25..30));
        assert_eq!(rs.rows(), 30);
        assert_eq!(rs.locate(0), Some((0, 0)));
        assert_eq!(rs.locate(9), Some((0, 9)));
        assert_eq!(rs.locate(10), Some((1, 0)));
        assert_eq!(rs.locate(24), Some((1, 14)));
        assert_eq!(rs.locate(25), Some((2, 0)));
        assert_eq!(rs.locate(29), Some((2, 4)));
        assert_eq!(rs.locate(30), None);
        assert_eq!(rs.value_at(24, 0), Some(ValueRef::I64(24)));
    }

    #[test]
    fn empty_chunks_do_not_break_addressing() {
        let mut rs = ResultSet::new(schema());
        rs.push_chunk(chunk_of(0..0));
        rs.push_chunk(chunk_of(0..3));
        assert_eq!(rs.rows(), 3);
        assert_eq!(rs.value_at(0, 0), Some(ValueRef::I64(0)));
    }
}
