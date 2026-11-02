//! Row window — the shape of `fetch_window` (§8, §8.1).
//!
//! Principle: **the complete result set stays in Rust, indexed by an id. The
//! frontend asks only for the visible window.**
//!
//! Two details that are discovered late:
//!  1. **the window is columnar on the wire too** — one array per column, not
//!     an array of objects. Less JSON, and Glide works by coordinates anyway;
//!  2. **large values must be truncated in the window**, with fetch on demand.
//!     Otherwise a 1 MB TEXT column over 200 visible rows = 200 MB of JSON
//!     for a single scroll.
//!
//! There is no serialization here: the conversion to JSON lives in the `app`
//! crate, because `core` knows nothing about the IPC.

use std::ops::Range;

use crate::result_set::ResultSet;
use crate::value::ValueRef;

/// Above this threshold the value is truncated in the window and can be
/// fetched with `get_cell_full`.
pub const WINDOW_CELL_TRUNCATE: usize = 4 * 1024;

/// A window column.
///
/// NULLs live in a separate bitmap and **not** in a sentinel string: they
/// must stay distinguishable from the string `"NULL"` and from the empty
/// string, otherwise the grid shows three different things the same way (§8.2).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowColumn {
    /// **Positional** index in the schema. Never the name (§4.3).
    pub index: usize,
    pub values: Vec<String>,
    /// `true` = NULL. A `Vec<bool>` instead of a bitmap because a window is
    /// ~200 rows: compressing it does not pay for the complexity.
    pub nulls: Vec<bool>,
    /// `true` = the value was truncated and the full one must be requested
    /// with `get_cell_full`.
    pub truncated: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub rows: Range<u64>,
    pub columns: Vec<WindowColumn>,
}

impl ResultSet {
    /// Extracts a window. Cost: one binary search per boundary row, then
    /// direct access (§4.8).
    pub fn window(&self, rows: Range<u64>, cols: Range<usize>) -> Window {
        let end = rows.end.min(self.rows());
        let start = rows.start.min(end);
        self.window_mapped(start..end, cols, |r| r)
    }

    /// Read path shared between the direct window and the permuted one (§4.8):
    /// `map` translates the view row into the physical row. The range must
    /// already be clamped by the caller to the view space.
    pub fn window_mapped(
        &self,
        rows: Range<u64>,
        cols: Range<usize>,
        map: impl Fn(u64) -> u64,
    ) -> Window {
        let (start, end) = (rows.start, rows.end);
        let n = (end - start) as usize;
        let col_end = cols.end.min(self.schema().len());
        let col_start = cols.start.min(col_end);

        // Resolve chunk/offset ONCE per row, not per cell: with C columns we
        // go from R·C binary searches (`locate`) to at most R — and for
        // contiguous physical rows (the non-permuted path, i.e. scrolling) we
        // advance on the known offset without any new search.
        // `None` = row out of range: rendered as NULL, as `value_at` did.
        let mut locs: Vec<Option<(usize, usize)>> = Vec::with_capacity(n);
        let mut prev: Option<(u64, usize, usize)> = None;
        for row in start..end {
            let phys = map(row);
            let loc = match prev {
                Some((p, ci, off)) if phys == p + 1 => self.advance(ci, off),
                _ => self.locate(phys),
            };
            prev = loc.map(|(ci, off)| (phys, ci, off));
            locs.push(loc);
        }

        let mut columns = Vec::with_capacity(col_end - col_start);
        for col in col_start..col_end {
            let mut values = Vec::with_capacity(n);
            let mut nulls = Vec::with_capacity(n);
            let mut truncated = Vec::with_capacity(n);
            for &loc in &locs {
                let value = loc.and_then(|(ci, off)| self.chunks()[ci].value_at(off, col));
                match value {
                    None | Some(ValueRef::Null) => {
                        values.push(String::new());
                        nulls.push(true);
                        truncated.push(false);
                    }
                    Some(ValueRef::Lob { prefix, .. }) => {
                        values.push(String::from_utf8_lossy(prefix).into_owned());
                        nulls.push(false);
                        truncated.push(true);
                    }
                    Some(v) => {
                        let s = v.to_canonical_string().unwrap_or_default();
                        if s.len() > WINDOW_CELL_TRUNCATE {
                            let cut = floor_char_boundary(&s, WINDOW_CELL_TRUNCATE);
                            values.push(s[..cut].to_owned());
                            truncated.push(true);
                        } else {
                            values.push(s);
                            truncated.push(false);
                        }
                        nulls.push(false);
                    }
                }
            }
            columns.push(WindowColumn {
                index: col,
                values,
                nulls,
                truncated,
            });
        }
        Window {
            rows: start..end,
            columns,
        }
    }

    /// (chunk, offset) of the next physical row **without binary search**:
    /// advances the offset or jumps to the next non-empty chunk.
    /// `None` = past the last loaded row.
    fn advance(&self, ci: usize, off: usize) -> Option<(usize, usize)> {
        let chunks = self.chunks();
        if off + 1 < chunks[ci].rows() as usize {
            return Some((ci, off + 1));
        }
        chunks
            .iter()
            .enumerate()
            .skip(ci + 1)
            .find(|(_, c)| c.rows() > 0)
            .map(|(i, _)| (i, 0))
    }
}

impl ResultSet {
    /// Full value of a cell for the inspection panel (`get_cell_full`, §8):
    /// resolves LOBs from the chunk's arena (§4.7).
    /// `None` = NULL or out of range.
    pub fn cell_full(&self, row: u64, col: usize) -> Option<String> {
        let (ci, off) = self.locate(row)?;
        let chunk = self.chunks().get(ci)?;
        match chunk.value_at(off, col)? {
            ValueRef::Lob { prefix, handle, .. } => {
                let full = chunk.lobs().get(handle).unwrap_or(prefix);
                Some(String::from_utf8_lossy(full).into_owned())
            }
            v => v.to_canonical_string(),
        }
    }
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use crate::chunk::{ChunkBuilder, ChunkLimits};
    use crate::column::ColumnKind;
    use crate::meta::{ColumnMeta, NativeType};
    use crate::result_set::ResultSet;
    use std::sync::Arc;

    #[test]
    fn null_empty_and_literal_null_are_three_things() {
        let schema: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "t",
            NativeType::Postgres {
                oid: 25,
                type_modifier: -1,
            },
            "text",
        )]
        .into();
        let mut b = ChunkBuilder::new(&[ColumnKind::Utf8], ChunkLimits::default());
        b.push_text(0, "").unwrap();
        b.end_row();
        b.column_mut(0).unwrap().push_null();
        b.end_row();
        b.push_text(0, "NULL").unwrap();
        b.end_row();
        let mut rs = ResultSet::new(schema);
        rs.push_chunk(b.finish().0);

        let w = rs.window(0..3, 0..1);
        let c = &w.columns[0];
        assert_eq!(c.nulls, vec![false, true, false]);
        assert_eq!(
            c.values,
            vec!["".to_string(), "".to_string(), "NULL".to_string()]
        );
    }

    #[test]
    fn window_clamps_out_of_range_requests() {
        let schema: Arc<[ColumnMeta]> = vec![].into();
        let rs = ResultSet::new(schema);
        let w = rs.window(100..200, 0..5);
        assert_eq!(w.rows, 0..0);
        assert!(w.columns.is_empty());
    }
}
