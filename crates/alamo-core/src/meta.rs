//! Column metadata — §4.3.
//!
//! ⚠️ **The identity of a column is its POSITION, never its name.**
//! `SELECT a.id, b.id FROM …` produces two `id` columns. Any `name -> column`
//! map in the UI or in the IPC is already a bug. That is why there is no
//! lookup by name here, and there must not be one.
//!
//! This metadata arrives in the `RowDescription` (PG) / column definition
//! (MySQL) **at execution time**. Afterwards it cannot be reconstructed.

/// The type as the server declares it, before any mapping.
///
/// Kept even when not needed: it is what will allow promoting MySQL's
/// `DECIMAL` to `Decimal128` in the future (§4.5) without re-running anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeType {
    Postgres {
        oid: u32,
        /// `atttypmod`: carries precision/scale of `numeric`, length of
        /// `varchar`. -1 when not specified.
        type_modifier: i32,
    },
    MySql {
        code: u8,
        /// `UNSIGNED`, `ZEROFILL`, `BINARY`, `NOT NULL`, …
        flags: u16,
        /// Distinguishes `BINARY`/`BLOB` from text (charset 63 = binary).
        charset: u16,
    },
}

/// Where the column really comes from. **Unlocks grid editing** (§4.3): it
/// costs nothing to capture, and without it editing cannot be implemented.
///
/// PG populates it only for columns that come directly from a table: on
/// `SELECT a+b` it is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnSource {
    Postgres {
        table_oid: u32,
        column_attnum: i16,
    },
    MySql {
        db: String,
        org_table: String,
        org_name: String,
    },
}

/// The five classes of §9.6, deliberately few. The frontend maps class ->
/// text tint; the colour is resolved **once per column**, never per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticClass {
    Numeric,
    Temporal,
    /// Default, no tint.
    Text,
    Boolean,
    /// JSON, array, binary.
    Structured,
}

/// Per-column statistics computed **from the offsets already present** while
/// building the chunk (§4.3). They enable automatic sizing without
/// `measureText` (§9.6.2): `width = len_p95 * advance + padding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColumnStats {
    /// 95th percentile of the length, **not** the maximum: a single
    /// 5000-character cell would produce an absurd column.
    pub len_p95: Option<u32>,
    /// If false, `len_p95` is in bytes and not in characters (multibyte UTF-8,
    /// CJK, emoji): the width must be sampled with `measureText` instead of
    /// computed (§9.6.2).
    pub is_ascii: bool,
}

/// Metadata of one column of a result set.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnMeta {
    /// **Output** name (after any alias). It is not a unique identifier: see
    /// the warning at the top of the module.
    pub name: String,
    pub native_type: NativeType,
    /// Name shown to the user.
    pub type_display: String,
    /// MySQL always says it; PG does not for computed columns.
    pub nullable: Option<bool>,
    pub source: Option<ColumnSource>,
    pub semantic_class: SemanticClass,
    pub stats: ColumnStats,
    /// True for types that stay text but must be right-aligned and sorted
    /// numerically: PG's `NUMERIC` stays `Utf8` permanently (§4.5) because
    /// precision and scale may be unspecified and the values exceed any
    /// `Decimal128`.
    pub numeric_text: bool,
    /// True if the column was degraded to `Utf8` from a typed kind because it
    /// met a non-representable value (§4.6). Useful in diagnostics: it
    /// explains why a `date` column behaves like text.
    pub degraded: bool,
    /// ⚠️ True if the driver could not decode the wire format and fell back
    /// to a raw rendering. See open decision 3 of §14 (hybrid binary/text
    /// format in PG).
    pub undecoded_wire_format: bool,
}

impl ColumnMeta {
    /// Minimal constructor, used by the drivers, which then fill in the rest.
    pub fn new(
        name: impl Into<String>,
        native_type: NativeType,
        type_display: impl Into<String>,
    ) -> Self {
        ColumnMeta {
            name: name.into(),
            native_type,
            type_display: type_display.into(),
            nullable: None,
            source: None,
            semantic_class: SemanticClass::Text,
            stats: ColumnStats::default(),
            numeric_text: false,
            degraded: false,
            undecoded_wire_format: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Statistics accumulator
// ---------------------------------------------------------------------------

/// Number of width-1 buckets (lengths 0..=255, the dominant case:
/// identifiers, codes, UUIDs, timestamps).
const LINEAR_BUCKETS: usize = 256;
/// Logarithmic buckets above 255.
const LOG_BUCKETS: usize = 40;
const TOTAL_BUCKETS: usize = LINEAR_BUCKETS + LOG_BUCKETS;

/// Length histogram, updated at O(1) cost per cell.
///
/// The alternative — keeping all the lengths and sorting them — costs memory
/// proportional to the rows for every column. Here it is 1.2 KB per column,
/// constant.
#[derive(Debug, Clone)]
pub struct StatsAccumulator {
    hist: Box<[u32; TOTAL_BUCKETS]>,
    count: u64,
    is_ascii: bool,
    /// `false` for fixed-width columns, where the display width does not
    /// depend on the data.
    tracks_length: bool,
}

impl Default for StatsAccumulator {
    fn default() -> Self {
        Self::new(true)
    }
}

impl StatsAccumulator {
    pub fn new(tracks_length: bool) -> Self {
        StatsAccumulator {
            hist: Box::new([0u32; TOTAL_BUCKETS]),
            count: 0,
            is_ascii: true,
            tracks_length,
        }
    }

    #[inline]
    fn bucket(len: u32) -> usize {
        if (len as usize) < LINEAR_BUCKETS {
            len as usize
        } else {
            // Above 255 the bit length is at least 9: the first log bucket
            // (index LINEAR_BUCKETS) covers 256..511, i.e. k = 9.
            let k = (u32::BITS - len.leading_zeros()) as usize; // ceil-ish log2
            LINEAR_BUCKETS + (k - 9).min(LOG_BUCKETS - 1)
        }
    }

    /// To be called for every **non-NULL** value. `len` in bytes.
    #[inline]
    pub fn observe(&mut self, len: u32) {
        if !self.tracks_length {
            return;
        }
        self.hist[Self::bucket(len)] += 1;
        self.count += 1;
    }

    /// To be called on text values. Stops scanning as soon as it knows the
    /// answer.
    #[inline]
    pub fn observe_bytes(&mut self, bytes: &[u8]) {
        self.observe(bytes.len() as u32);
        if self.is_ascii && !bytes.is_ascii() {
            self.is_ascii = false;
        }
    }

    /// Fixed width known a priori (integers, timestamps, bool): declared once
    /// instead of measured per row.
    pub fn fixed_width(width: u32) -> Self {
        let mut acc = StatsAccumulator::new(false);
        acc.hist[Self::bucket(width)] = 1;
        acc.count = 1;
        acc
    }

    pub fn finish(&self) -> ColumnStats {
        if self.count == 0 {
            return ColumnStats {
                len_p95: None,
                is_ascii: self.is_ascii,
            };
        }
        let target = ((self.count as f64) * 0.95).ceil() as u64;
        let mut cum: u64 = 0;
        for (i, &n) in self.hist.iter().enumerate() {
            cum += n as u64;
            if cum >= target {
                let len = if i < LINEAR_BUCKETS {
                    i as u32
                } else {
                    // Upper bound of the logarithmic bucket: deliberately
                    // overestimates, it is a column width. In u64 because for
                    // lengths >= 2^31 the shift reaches 32 and in u32 it would
                    // panic in debug; the result saturates at u32::MAX.
                    let shift = (i - LINEAR_BUCKETS) as u32 + 9;
                    (1u64 << shift).min(u32::MAX as u64) as u32
                };
                return ColumnStats {
                    len_p95: Some(len),
                    is_ascii: self.is_ascii,
                };
            }
        }
        ColumnStats {
            len_p95: None,
            is_ascii: self.is_ascii,
        }
    }

    /// Merges two accumulators — needed because statistics are computed per
    /// chunk but the column width is a property of the result set.
    pub fn merge(&mut self, other: &StatsAccumulator) {
        for i in 0..TOTAL_BUCKETS {
            self.hist[i] = self.hist[i].saturating_add(other.hist[i]);
        }
        self.count += other.count;
        self.is_ascii &= other.is_ascii;
        self.tracks_length |= other.tracks_length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p95_ignores_the_single_huge_cell() {
        let mut acc = StatsAccumulator::new(true);
        for _ in 0..999 {
            acc.observe(12);
        }
        acc.observe(5000);
        // The maximum would be 5000 and would produce an absurd column.
        assert_eq!(acc.finish().len_p95, Some(12));
    }

    #[test]
    fn huge_lengths_saturate_instead_of_overflowing_the_shift() {
        // Lengths >= 2^31: the shift of the upper bound would reach 32 bits
        // and panic in debug. It must saturate at u32::MAX.
        let mut acc = StatsAccumulator::new(true);
        acc.observe(u32::MAX);
        assert_eq!(acc.finish().len_p95, Some(u32::MAX));
    }

    #[test]
    fn first_log_bucket_reports_its_upper_bound() {
        // 256 lands in the first log bucket (256..511): the p95 reports the
        // upper bound 512, not a bucket off by one.
        let mut acc = StatsAccumulator::new(true);
        acc.observe(256);
        assert_eq!(acc.finish().len_p95, Some(512));
    }

    #[test]
    fn non_ascii_is_sticky() {
        let mut acc = StatsAccumulator::new(true);
        acc.observe_bytes(b"plain");
        assert!(acc.finish().is_ascii);
        acc.observe_bytes("caffè".as_bytes());
        acc.observe_bytes(b"plain");
        assert!(!acc.finish().is_ascii);
    }
}
