//! Chunk — §5.
//!
//! `Vec<Chunk>` with chunks of ~64k rows. It pays off on five fronts: no giant
//! reallocations, evict/spill per single chunk, cancellation mid-fetch that
//! keeps the complete chunks, `fetch_window` that becomes "find the chunk +
//! offset", 1:1 mapping onto `RecordBatch` for Arrow.
//!
//! Closing a chunk is triggered by **three** conditions (§5):
//!  1. row count;
//!  2. byte threshold — the budget is in BYTES, never in rows: 50 TEXT columns
//!     and 3 INT columns have the same row count and two orders of magnitude
//!     of difference in RAM;
//!  3. risk of overflowing the `i32` offsets.

use crate::column::{Column, ColumnBuilder, ColumnKind};
use crate::lob::LobArena;
use crate::meta::StatsAccumulator;
use crate::value::ValueRef;
use crate::CoreError;

/// Rows per chunk. `[VERIFY M0]` — open decision 7 of §14.
pub const DEFAULT_CHUNK_ROWS: u32 = 64 * 1024;
/// Bytes per chunk before closing. `[VERIFY M0]`
pub const DEFAULT_CHUNK_BYTES: usize = 32 * 1024 * 1024;
/// Cap on varlen data alone. Well below `i32::MAX` on purpose: the close must
/// happen *before* a single push could overflow.
pub const DEFAULT_CHUNK_VARLEN_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLimits {
    pub max_rows: u32,
    pub max_bytes: usize,
    pub max_varlen_bytes: usize,
}

impl Default for ChunkLimits {
    fn default() -> Self {
        ChunkLimits {
            max_rows: DEFAULT_CHUNK_ROWS,
            max_bytes: DEFAULT_CHUNK_BYTES,
            max_varlen_bytes: DEFAULT_CHUNK_VARLEN_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    rows: u32,
    columns: Vec<Column>,
    lobs: LobArena,
}

impl Chunk {
    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Access **by position**. There is no access by name, and there must not
    /// be one: §4.3.
    pub fn column(&self, index: usize) -> Option<&Column> {
        self.columns.get(index)
    }

    pub fn lobs(&self) -> &LobArena {
        &self.lobs
    }

    pub fn value_at(&self, row: usize, col: usize) -> Option<ValueRef<'_>> {
        if row >= self.rows as usize {
            return None;
        }
        Some(self.columns.get(col)?.value_at(row, &self.lobs))
    }

    /// §4.8: the chunk sums columns + arena. A property computed from the
    /// layout, never a counter updated by hand — hand-kept counters always
    /// drift.
    pub fn byte_size(&self) -> usize {
        self.columns.iter().map(|c| c.byte_size()).sum::<usize>() + self.lobs.byte_size()
    }
}

/// Builds a chunk row by row. The driver:
///  - calls the `push_*` of every column in schema order;
///  - calls `end_row()`;
///  - checks `should_close()` and closes if needed.
#[derive(Debug)]
pub struct ChunkBuilder {
    rows: u32,
    columns: Vec<ColumnBuilder>,
    lobs: LobArena,
    limits: ChunkLimits,
}

impl ChunkBuilder {
    pub fn new(kinds: &[ColumnKind], limits: ChunkLimits) -> Self {
        let hint = limits.max_rows.min(8192) as usize;
        ChunkBuilder {
            rows: 0,
            columns: kinds
                .iter()
                .map(|k| ColumnBuilder::with_capacity(*k, hint))
                .collect(),
            lobs: LobArena::new(),
            limits,
        }
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn column_mut(&mut self, index: usize) -> Option<&mut ColumnBuilder> {
        self.columns.get_mut(index)
    }

    /// Closes the current row. Must be called **after** pushing a value into
    /// every column.
    pub fn end_row(&mut self) {
        self.rows += 1;
        debug_assert!(
            self.columns.iter().all(|c| c.rows() == self.rows),
            "a column skipped a row: the columnar layout does not tolerate it"
        );
    }

    pub fn byte_size(&self) -> usize {
        self.columns.iter().map(|c| c.byte_size()).sum::<usize>() + self.lobs.byte_size()
    }

    pub fn varlen_bytes(&self) -> usize {
        self.columns.iter().map(|c| c.varlen_bytes()).sum()
    }

    /// The three triggers of §5, in one place.
    pub fn should_close(&self) -> bool {
        self.rows >= self.limits.max_rows
            || self.byte_size() >= self.limits.max_bytes
            || self.varlen_bytes() >= self.limits.max_varlen_bytes
    }

    // Text pushes go through here because they must be able to write into the
    // LOB arena, which belongs to the chunk and not to the column (§4.7). The
    // destructuring avoids the double borrow of `self`.
    pub fn push_text(&mut self, col: usize, s: &str) -> Result<(), CoreError> {
        let ChunkBuilder { columns, lobs, .. } = self;
        columns
            .get_mut(col)
            .ok_or(CoreError::ColumnOutOfRange { index: col })?
            .push_text(s, lobs)
    }

    pub fn push_bytes(&mut self, col: usize, b: &[u8]) -> Result<(), CoreError> {
        let ChunkBuilder { columns, lobs, .. } = self;
        columns
            .get_mut(col)
            .ok_or(CoreError::ColumnOutOfRange { index: col })?
            .push_bytes(b, lobs)
    }

    /// See `ColumnBuilder::push_text_bytes`: text already validated by the server.
    pub fn push_text_bytes(&mut self, col: usize, b: &[u8]) -> Result<(), CoreError> {
        let ChunkBuilder { columns, lobs, .. } = self;
        columns
            .get_mut(col)
            .ok_or(CoreError::ColumnOutOfRange { index: col })?
            .push_text_bytes(b, lobs)
    }

    /// The value is not representable in the chosen type: degrade the column
    /// and keep the server's text (§4.6).
    pub fn push_degraded_text(&mut self, col: usize, s: &str) -> Result<(), CoreError> {
        let ChunkBuilder { columns, lobs, .. } = self;
        columns
            .get_mut(col)
            .ok_or(CoreError::ColumnOutOfRange { index: col })?
            .push_degraded_text(s, lobs)
    }

    pub fn finish(self) -> (Chunk, Vec<StatsAccumulator>) {
        let mut columns = Vec::with_capacity(self.columns.len());
        let mut stats = Vec::with_capacity(self.columns.len());
        for cb in self.columns {
            let (col, acc) = cb.finish();
            columns.push(col);
            stats.push(acc);
        }
        (
            Chunk {
                rows: self.rows,
                columns,
                lobs: self.lobs,
            },
            stats,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_on_row_count() {
        let limits = ChunkLimits {
            max_rows: 4,
            ..Default::default()
        };
        let mut b = ChunkBuilder::new(&[ColumnKind::I32], limits);
        for i in 0..4 {
            b.column_mut(0).unwrap().push_i32(i).unwrap();
            b.end_row();
        }
        assert!(b.should_close());
        let (chunk, _) = b.finish();
        assert_eq!(chunk.rows(), 4);
    }

    #[test]
    fn closes_on_varlen_bytes_before_i32_overflow() {
        let limits = ChunkLimits {
            max_rows: u32::MAX,
            max_bytes: usize::MAX,
            max_varlen_bytes: 1024,
        };
        let mut b = ChunkBuilder::new(&[ColumnKind::Utf8], limits);
        let filler = "y".repeat(200);
        let mut pushed = 0;
        while !b.should_close() {
            b.push_text(0, &filler).unwrap();
            b.end_row();
            pushed += 1;
            assert!(pushed < 100, "should_close never fired");
        }
        assert!(b.varlen_bytes() >= 1024);
    }
}
