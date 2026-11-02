//! Columns — §4.2, §4.4, §4.6.
//!
//! The three layout rules, which together make the conversion to Arrow a
//! `memcpy` and not a rewrite:
//!
//! 1. validity bitmap separate from the data (`bitmap.rs`);
//! 2. varlen as offsets + contiguous buffer (`varbuf.rs`);
//! 3. **no enum per cell**: the enum lives here, at column level.

use crate::bitmap::Bitmap;
use crate::fmt;
use crate::lob::{LobArena, LobHandle, LOB_INLINE_CAP, LOB_PREFIX_LEN};
use crate::meta::StatsAccumulator;
use crate::value::ValueRef;
use crate::varbuf::VarBuf;
use crate::CoreError;

/// The variants of §4.4. Keeping them few is a decision, not laziness: every
/// variant is decoding, comparison, formatting and export code.
///
/// Everything else — interval, range, array, geometries, domains, bit/varbit,
/// custom types — goes into `Utf8` with the text **exactly as the server sends
/// it**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    Bool,
    I16,
    I32,
    I64,
    /// MySQL's `BIGINT UNSIGNED`.
    U64,
    F32,
    F64,
    /// Text, JSON/JSONB, UUID, enum/set, **and the whole fallback**.
    Utf8,
    /// `bytea`, BLOB. Same shape as `Utf8`.
    Binary,
    /// Days since the epoch.
    Date32,
    /// Time of day in microseconds.
    Time64Micros,
    TimestampMicros {
        tz: bool,
    },
}

impl ColumnKind {
    /// Typical display width, for fixed-width types. Serves column sizing
    /// (§9.6.2) without looking at the data.
    pub fn nominal_display_width(&self) -> Option<u32> {
        Some(match self {
            ColumnKind::Bool => 5,
            ColumnKind::I16 => 6,
            ColumnKind::I32 => 11,
            ColumnKind::I64 | ColumnKind::U64 => 20,
            ColumnKind::F32 | ColumnKind::F64 => 16,
            ColumnKind::Date32 => 10,
            ColumnKind::Time64Micros => 15,
            ColumnKind::TimestampMicros { tz } => {
                if *tz {
                    29
                } else {
                    26
                }
            }
            ColumnKind::Utf8 | ColumnKind::Binary => return None,
        })
    }

    pub fn is_varlen(&self) -> bool {
        matches!(self, ColumnKind::Utf8 | ColumnKind::Binary)
    }
}

/// A column's buffers. One enum per column, not one per cell.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnData {
    Bool(Bitmap),
    I16(Vec<i16>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    U64(Vec<u64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Utf8(VarBuf),
    Binary(VarBuf),
    Date32(Vec<i32>),
    Time64Micros(Vec<i64>),
    TimestampMicros { values: Vec<i64>, tz: bool },
}

impl ColumnData {
    pub fn empty(kind: ColumnKind) -> Self {
        match kind {
            ColumnKind::Bool => ColumnData::Bool(Bitmap::new()),
            ColumnKind::I16 => ColumnData::I16(Vec::new()),
            ColumnKind::I32 => ColumnData::I32(Vec::new()),
            ColumnKind::I64 => ColumnData::I64(Vec::new()),
            ColumnKind::U64 => ColumnData::U64(Vec::new()),
            ColumnKind::F32 => ColumnData::F32(Vec::new()),
            ColumnKind::F64 => ColumnData::F64(Vec::new()),
            ColumnKind::Utf8 => ColumnData::Utf8(VarBuf::new()),
            ColumnKind::Binary => ColumnData::Binary(VarBuf::new()),
            ColumnKind::Date32 => ColumnData::Date32(Vec::new()),
            ColumnKind::Time64Micros => ColumnData::Time64Micros(Vec::new()),
            ColumnKind::TimestampMicros { tz } => ColumnData::TimestampMicros {
                values: Vec::new(),
                tz,
            },
        }
    }

    pub fn kind(&self) -> ColumnKind {
        match self {
            ColumnData::Bool(_) => ColumnKind::Bool,
            ColumnData::I16(_) => ColumnKind::I16,
            ColumnData::I32(_) => ColumnKind::I32,
            ColumnData::I64(_) => ColumnKind::I64,
            ColumnData::U64(_) => ColumnKind::U64,
            ColumnData::F32(_) => ColumnKind::F32,
            ColumnData::F64(_) => ColumnKind::F64,
            ColumnData::Utf8(_) => ColumnKind::Utf8,
            ColumnData::Binary(_) => ColumnKind::Binary,
            ColumnData::Date32(_) => ColumnKind::Date32,
            ColumnData::Time64Micros(_) => ColumnKind::Time64Micros,
            ColumnData::TimestampMicros { tz, .. } => ColumnKind::TimestampMicros { tz: *tz },
        }
    }

    pub fn len(&self) -> usize {
        match self {
            ColumnData::Bool(b) => b.len(),
            ColumnData::I16(v) => v.len(),
            ColumnData::I32(v) | ColumnData::Date32(v) => v.len(),
            ColumnData::I64(v) | ColumnData::Time64Micros(v) => v.len(),
            ColumnData::TimestampMicros { values, .. } => values.len(),
            ColumnData::U64(v) => v.len(),
            ColumnData::F32(v) => v.len(),
            ColumnData::F64(v) => v.len(),
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes used by the data buffers. §4.8: a property of the layout.
    pub fn byte_size(&self) -> usize {
        match self {
            ColumnData::Bool(b) => b.byte_size(),
            ColumnData::I16(v) => v.capacity() * 2,
            ColumnData::I32(v) | ColumnData::Date32(v) => v.capacity() * 4,
            ColumnData::I64(v) | ColumnData::Time64Micros(v) => v.capacity() * 8,
            ColumnData::TimestampMicros { values, .. } => values.capacity() * 8,
            ColumnData::U64(v) => v.capacity() * 8,
            ColumnData::F32(v) => v.capacity() * 4,
            ColumnData::F64(v) => v.capacity() * 8,
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.byte_size(),
        }
    }

    /// Gives back the excess capacity left by the growth of the `Vec`s.
    /// See `VarBuf::shrink`: called only once the chunk is closed, hence immutable.
    pub fn shrink(&mut self) {
        match self {
            ColumnData::Bool(_) => {}
            ColumnData::I16(v) => v.shrink_to_fit(),
            ColumnData::I32(v) | ColumnData::Date32(v) => v.shrink_to_fit(),
            ColumnData::I64(v) | ColumnData::Time64Micros(v) => v.shrink_to_fit(),
            ColumnData::TimestampMicros { values, .. } => values.shrink_to_fit(),
            ColumnData::U64(v) => v.shrink_to_fit(),
            ColumnData::F32(v) => v.shrink_to_fit(),
            ColumnData::F64(v) => v.shrink_to_fit(),
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.shrink(),
        }
    }

    /// Bytes of the varlen data alone — feeds the third chunk-close trigger
    /// (risk of overflowing the `i32` offsets, §5).
    pub fn varlen_bytes(&self) -> usize {
        match self {
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.data_len(),
            _ => 0,
        }
    }
}

/// A complete column inside a chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    validity: Bitmap,
    null_count: u32,
    data: ColumnData,
    /// Rows whose value ended up in the chunk's `LobArena` (§4.7).
    /// Sorted by row: binary search, and in the common (empty) case zero cost.
    lob_refs: Vec<(u32, LobHandle)>,
    /// The column has been degraded to `Utf8` (§4.6).
    degraded: bool,
}

impl Column {
    pub fn validity(&self) -> &Bitmap {
        &self.validity
    }

    /// Explicit: allows **skipping the validity check entirely** on columns
    /// without NULLs, which are the majority (§4.2).
    pub fn null_count(&self) -> u32 {
        self.null_count
    }

    pub fn has_nulls(&self) -> bool {
        self.null_count > 0
    }

    pub fn data(&self) -> &ColumnData {
        &self.data
    }

    pub fn kind(&self) -> ColumnKind {
        self.data.kind()
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn byte_size(&self) -> usize {
        self.validity.byte_size()
            + self.data.byte_size()
            + self.lob_refs.capacity() * std::mem::size_of::<(u32, LobHandle)>()
    }

    fn lob_at(&self, row: usize) -> Option<LobHandle> {
        if self.lob_refs.is_empty() {
            return None;
        }
        self.lob_refs
            .binary_search_by_key(&(row as u32), |&(r, _)| r)
            .ok()
            .map(|i| self.lob_refs[i].1)
    }

    /// Temporary view of the cell. Does not allocate.
    pub fn value_at<'a>(&'a self, row: usize, arena: &'a LobArena) -> ValueRef<'a> {
        if self.null_count > 0 && !self.validity.get(row) {
            return ValueRef::Null;
        }
        if let Some(handle) = self.lob_at(row) {
            let total_len = arena.get(handle).map(|b| b.len() as u64).unwrap_or(0);
            let prefix = match &self.data {
                ColumnData::Utf8(v) | ColumnData::Binary(v) => v.get(row),
                _ => &[],
            };
            return ValueRef::Lob {
                prefix,
                handle,
                total_len,
            };
        }
        match &self.data {
            ColumnData::Bool(b) => ValueRef::Bool(b.get(row)),
            ColumnData::I16(v) => ValueRef::I16(v[row]),
            ColumnData::I32(v) => ValueRef::I32(v[row]),
            ColumnData::I64(v) => ValueRef::I64(v[row]),
            ColumnData::U64(v) => ValueRef::U64(v[row]),
            ColumnData::F32(v) => ValueRef::F32(v[row]),
            ColumnData::F64(v) => ValueRef::F64(v[row]),
            // `Utf8` buffers contain only valid text: the drivers write into
            // them through `push_text`, which takes `&str`.
            ColumnData::Utf8(v) => match std::str::from_utf8(v.get(row)) {
                Ok(s) => ValueRef::Utf8(s),
                Err(_) => ValueRef::Binary(v.get(row)),
            },
            ColumnData::Binary(v) => ValueRef::Binary(v.get(row)),
            ColumnData::Date32(v) => ValueRef::Date32(v[row]),
            ColumnData::Time64Micros(v) => ValueRef::Time64Micros(v[row]),
            ColumnData::TimestampMicros { values, tz } => ValueRef::TimestampMicros {
                micros: values[row],
                tz: *tz,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// Accumulates a column row by row.
///
/// Every typed `push_*` keeps working even **after** the column has been
/// degraded to `Utf8` (§4.6): in that case the value is formatted in its
/// canonical form. The driver does not need to know the degradation state.
#[derive(Debug)]
pub struct ColumnBuilder {
    validity: Bitmap,
    null_count: u32,
    data: ColumnData,
    stats: StatsAccumulator,
    lob_refs: Vec<(u32, LobHandle)>,
    degraded: bool,
    rows: u32,
}

impl ColumnBuilder {
    pub fn new(kind: ColumnKind) -> Self {
        let stats = match kind.nominal_display_width() {
            Some(w) => StatsAccumulator::fixed_width(w),
            None => StatsAccumulator::new(true),
        };
        ColumnBuilder {
            validity: Bitmap::new(),
            null_count: 0,
            data: ColumnData::empty(kind),
            stats,
            lob_refs: Vec::new(),
            degraded: false,
            rows: 0,
        }
    }

    pub fn with_capacity(kind: ColumnKind, rows: usize) -> Self {
        let mut b = Self::new(kind);
        b.validity = Bitmap::with_capacity(rows);
        b.data = match kind {
            ColumnKind::Utf8 => ColumnData::Utf8(VarBuf::with_capacity(rows, rows * 16)),
            ColumnKind::Binary => ColumnData::Binary(VarBuf::with_capacity(rows, rows * 16)),
            ColumnKind::I16 => ColumnData::I16(Vec::with_capacity(rows)),
            ColumnKind::I32 => ColumnData::I32(Vec::with_capacity(rows)),
            ColumnKind::I64 => ColumnData::I64(Vec::with_capacity(rows)),
            ColumnKind::U64 => ColumnData::U64(Vec::with_capacity(rows)),
            ColumnKind::F32 => ColumnData::F32(Vec::with_capacity(rows)),
            ColumnKind::F64 => ColumnData::F64(Vec::with_capacity(rows)),
            ColumnKind::Date32 => ColumnData::Date32(Vec::with_capacity(rows)),
            ColumnKind::Time64Micros => ColumnData::Time64Micros(Vec::with_capacity(rows)),
            ColumnKind::TimestampMicros { tz } => ColumnData::TimestampMicros {
                values: Vec::with_capacity(rows),
                tz,
            },
            ColumnKind::Bool => ColumnData::Bool(Bitmap::with_capacity(rows)),
        };
        b
    }

    pub fn kind(&self) -> ColumnKind {
        self.data.kind()
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn byte_size(&self) -> usize {
        self.validity.byte_size() + self.data.byte_size()
    }

    pub fn varlen_bytes(&self) -> usize {
        self.data.varlen_bytes()
    }

    // -- push ---------------------------------------------------------------

    pub fn push_null(&mut self) {
        self.validity.push(false);
        self.null_count += 1;
        self.rows += 1;
        match &mut self.data {
            ColumnData::Bool(b) => b.push(false),
            ColumnData::I16(v) => v.push(0),
            ColumnData::I32(v) | ColumnData::Date32(v) => v.push(0),
            ColumnData::I64(v) | ColumnData::Time64Micros(v) => v.push(0),
            ColumnData::TimestampMicros { values, .. } => values.push(0),
            ColumnData::U64(v) => v.push(0),
            ColumnData::F32(v) => v.push(0.0),
            ColumnData::F64(v) => v.push(0.0),
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.push_empty(),
        }
    }

    fn mark_valid(&mut self) {
        self.validity.push(true);
        self.rows += 1;
    }

    /// Common path for text values. Applies the LOB cap of §4.7.
    pub fn push_text(&mut self, s: &str, arena: &mut LobArena) -> Result<(), CoreError> {
        self.push_var(s.as_bytes(), arena, true)
    }

    pub fn push_bytes(&mut self, b: &[u8], arena: &mut LobArena) -> Result<(), CoreError> {
        self.push_var(b, arena, false)
    }

    /// Text the driver already knows is valid UTF-8 because it comes from the
    /// server. Avoids the re-validation of `str::from_utf8` on every cell — on
    /// a TEXT column of a million rows that is not a detail — but still treats
    /// the value as text when cutting the LOB prefix.
    pub fn push_text_bytes(&mut self, b: &[u8], arena: &mut LobArena) -> Result<(), CoreError> {
        self.push_var(b, arena, true)
    }

    fn push_var(
        &mut self,
        bytes: &[u8],
        arena: &mut LobArena,
        text: bool,
    ) -> Result<(), CoreError> {
        if !self.data.kind().is_varlen() {
            // A text value on a typed column means the driver met something
            // not representable: degrade.
            self.degrade_to_utf8()?;
        }
        let row = self.rows;
        let store: &[u8] = if bytes.len() > LOB_INLINE_CAP {
            let handle = arena.push(bytes);
            self.lob_refs.push((row, handle));
            &bytes[..prefix_boundary(bytes, text)]
        } else {
            bytes
        };
        match &mut self.data {
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.push(store)?,
            _ => unreachable!("degrade_to_utf8 guarantees a varlen variant"),
        }
        self.stats.observe_bytes(bytes);
        self.mark_valid();
        Ok(())
    }

    pub fn push_bool(&mut self, value: bool) -> Result<(), CoreError> {
        if let ColumnData::Bool(b) = &mut self.data {
            b.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(if value { "true" } else { "false" })
    }

    pub fn push_i16(&mut self, value: i16) -> Result<(), CoreError> {
        if let ColumnData::I16(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_i32(&mut self, value: i32) -> Result<(), CoreError> {
        if let ColumnData::I32(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_i64(&mut self, value: i64) -> Result<(), CoreError> {
        if let ColumnData::I64(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_u64(&mut self, value: u64) -> Result<(), CoreError> {
        if let ColumnData::U64(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_f32(&mut self, value: f32) -> Result<(), CoreError> {
        if let ColumnData::F32(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_f64(&mut self, value: f64) -> Result<(), CoreError> {
        if let ColumnData::F64(v) = &mut self.data {
            v.push(value);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&value.to_string())
    }

    pub fn push_date32(&mut self, days: i32) -> Result<(), CoreError> {
        if let ColumnData::Date32(v) = &mut self.data {
            v.push(days);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&fmt::format_date32(days))
    }

    pub fn push_time64_micros(&mut self, micros: i64) -> Result<(), CoreError> {
        if let ColumnData::Time64Micros(v) = &mut self.data {
            v.push(micros);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&fmt::format_time64_micros(micros))
    }

    pub fn push_timestamp_micros(&mut self, micros: i64) -> Result<(), CoreError> {
        let tz = match &self.data {
            ColumnData::TimestampMicros { tz, .. } => *tz,
            _ => false,
        };
        if let ColumnData::TimestampMicros { values, .. } = &mut self.data {
            values.push(micros);
            self.mark_valid();
            return Ok(());
        }
        self.push_degraded_scalar(&fmt::format_timestamp_micros(micros, tz))
    }

    fn push_degraded_scalar(&mut self, text: &str) -> Result<(), CoreError> {
        if !self.data.kind().is_varlen() {
            return Err(CoreError::KindMismatch {
                expected: self.data.kind(),
            });
        }
        match &mut self.data {
            ColumnData::Utf8(v) | ColumnData::Binary(v) => v.push(text.as_bytes())?,
            _ => unreachable!(),
        }
        self.stats.observe_bytes(text.as_bytes());
        self.mark_valid();
        Ok(())
    }

    // -- degradation --------------------------------------------------------

    /// §4.6: at the first non-representable value the column degrades to
    /// `Utf8` and **stays there**. It costs a rewrite of the current chunk,
    /// and protects forever.
    ///
    /// Cases that trigger it: MySQL's zero-date `'0000-00-00'`, MySQL's `TIME`
    /// (which is a duration from -838:59:59 to 838:59:59, not a time of day),
    /// PostgreSQL's `infinity`/`-infinity` on dates and timestamps. Ignoring
    /// them produces a crash or, worse, a wrong value that looks like a right
    /// one.
    pub fn degrade_to_utf8(&mut self) -> Result<(), CoreError> {
        if self.data.kind() == ColumnKind::Utf8 {
            return Ok(());
        }
        if self.data.kind() == ColumnKind::Binary {
            // Binary has no text rendering more faithful than itself.
            return Ok(());
        }
        let rows = self.data.len();
        let mut out = VarBuf::with_capacity(rows, rows * 12);
        let mut stats = StatsAccumulator::new(true);
        for row in 0..rows {
            if self.null_count > 0 && !self.validity.get(row) {
                out.push_empty();
                continue;
            }
            let text = match &self.data {
                ColumnData::Bool(b) => {
                    if b.get(row) {
                        "true".to_owned()
                    } else {
                        "false".to_owned()
                    }
                }
                ColumnData::I16(v) => v[row].to_string(),
                ColumnData::I32(v) => v[row].to_string(),
                ColumnData::I64(v) => v[row].to_string(),
                ColumnData::U64(v) => v[row].to_string(),
                ColumnData::F32(v) => v[row].to_string(),
                ColumnData::F64(v) => v[row].to_string(),
                ColumnData::Date32(v) => fmt::format_date32(v[row]),
                ColumnData::Time64Micros(v) => fmt::format_time64_micros(v[row]),
                ColumnData::TimestampMicros { values, tz } => {
                    fmt::format_timestamp_micros(values[row], *tz)
                }
                ColumnData::Utf8(_) | ColumnData::Binary(_) => unreachable!(),
            };
            out.push(text.as_bytes())?;
            stats.observe_bytes(text.as_bytes());
        }
        self.data = ColumnData::Utf8(out);
        self.stats = stats;
        self.degraded = true;
        Ok(())
    }

    /// Forces the server's raw text onto a column, degrading it if needed.
    /// It is the path the drivers use when they meet a value the chosen type
    /// cannot hold.
    pub fn push_degraded_text(
        &mut self,
        text: &str,
        arena: &mut LobArena,
    ) -> Result<(), CoreError> {
        self.degrade_to_utf8()?;
        self.push_text(text, arena)
    }

    pub fn finish(mut self) -> (Column, StatsAccumulator) {
        // The chunk is closed and no longer grows: the excess capacity left
        // by the doubling of the `Vec`s is RAM used for nothing, and on a TEXT
        // column it is up to 40% of the total. Giving it back here is the only
        // point where it is safe to do so.
        self.data.shrink();
        self.lob_refs.shrink_to_fit();
        (
            Column {
                validity: self.validity,
                null_count: self.null_count,
                data: self.data,
                lob_refs: self.lob_refs,
                degraded: self.degraded,
            },
            self.stats,
        )
    }
}

/// Finds a cut point that does not split a UTF-8 character in half.
fn prefix_boundary(bytes: &[u8], text: bool) -> usize {
    let mut end = LOB_PREFIX_LEN.min(bytes.len());
    if end == bytes.len() {
        return end;
    }
    if text {
        while end > 0 && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_column_roundtrip() {
        let arena = LobArena::new();
        let mut b = ColumnBuilder::new(ColumnKind::I64);
        b.push_i64(1).unwrap();
        b.push_null();
        b.push_i64(-3).unwrap();
        let (col, _) = b.finish();
        assert_eq!(col.null_count(), 1);
        assert_eq!(col.value_at(0, &arena), ValueRef::I64(1));
        assert_eq!(col.value_at(1, &arena), ValueRef::Null);
        assert_eq!(col.value_at(2, &arena), ValueRef::I64(-3));
    }

    #[test]
    fn degradation_rewrites_previous_values_and_keeps_nulls() {
        let mut arena = LobArena::new();
        let mut b = ColumnBuilder::new(ColumnKind::Date32);
        b.push_date32(0).unwrap();
        b.push_null();
        b.push_date32(10_957).unwrap();
        // `infinity` arrives: it does not fit in Date32 (§4.6).
        b.push_degraded_text("infinity", &mut arena).unwrap();
        b.push_date32(1).unwrap(); // keeps working, formatted
        let (col, _) = b.finish();
        assert_eq!(col.kind(), ColumnKind::Utf8);
        assert!(col.degraded());
        assert_eq!(col.value_at(0, &arena), ValueRef::Utf8("1970-01-01"));
        assert_eq!(col.value_at(1, &arena), ValueRef::Null);
        assert_eq!(col.value_at(2, &arena), ValueRef::Utf8("2000-01-01"));
        assert_eq!(col.value_at(3, &arena), ValueRef::Utf8("infinity"));
        assert_eq!(col.value_at(4, &arena), ValueRef::Utf8("1970-01-02"));
    }

    #[test]
    fn empty_string_and_null_stay_distinct() {
        let mut arena = LobArena::new();
        let mut b = ColumnBuilder::new(ColumnKind::Utf8);
        b.push_text("", &mut arena).unwrap();
        b.push_null();
        b.push_text("NULL", &mut arena).unwrap();
        let (col, _) = b.finish();
        assert_eq!(col.value_at(0, &arena), ValueRef::Utf8(""));
        assert_eq!(col.value_at(1, &arena), ValueRef::Null);
        assert_eq!(col.value_at(2, &arena), ValueRef::Utf8("NULL"));
    }

    #[test]
    fn large_value_goes_to_the_arena() {
        let mut arena = LobArena::new();
        let mut b = ColumnBuilder::new(ColumnKind::Utf8);
        let big = "x".repeat(LOB_INLINE_CAP + 10);
        b.push_text(&big, &mut arena).unwrap();
        let (col, _) = b.finish();
        match col.value_at(0, &arena) {
            ValueRef::Lob {
                prefix, total_len, ..
            } => {
                assert_eq!(prefix.len(), LOB_PREFIX_LEN);
                assert_eq!(total_len as usize, big.len());
            }
            other => panic!("expected Lob, found {other:?}"),
        }
    }
}
