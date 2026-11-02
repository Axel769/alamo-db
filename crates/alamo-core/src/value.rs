//! Temporary view of a cell.
//!
//! Rule 3 of §4.2: **no enum per cell**. The enum lives at *column* level
//! (`ColumnData`). `ValueRef` exists only as a view built on the fly for the
//! UI and for export — **never as storage**.
//!
//! It is a by-reference type on purpose: if it were `Value` with a `String`
//! inside, every scroll would allocate one string per cell, which is exactly
//! the cost the columnar layout exists to avoid.

use crate::lob::LobHandle;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueRef<'a> {
    Null,
    Bool(bool),
    I16(i16),
    I32(i32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    Utf8(&'a str),
    Binary(&'a [u8]),
    /// Days since the Unix epoch.
    Date32(i32),
    /// Microseconds since midnight.
    Time64Micros(i64),
    /// Microseconds since the Unix epoch; `tz` distinguishes `timestamptz`
    /// from `timestamp`.
    TimestampMicros {
        micros: i64,
        tz: bool,
    },
    /// Value above the 64 KB inline cap (§4.7): the column holds the truncated
    /// prefix, the full value lives in the chunk's `LobArena` and is fetched
    /// with `get_cell_full`.
    Lob {
        prefix: &'a [u8],
        handle: LobHandle,
        total_len: u64,
    },
}

impl ValueRef<'_> {
    pub fn is_null(&self) -> bool {
        matches!(self, ValueRef::Null)
    }

    /// Canonical text form — the one that ends up in the clipboard and in the
    /// export (§8.2). Whoever copies a cell expects the value, not its
    /// localized rendering.
    ///
    /// ⚠️ **On `Lob`s it returns ONLY the prefix** (max `LOB_PREFIX_LEN` =
    /// 256 bytes, §4.7), looking like a full value: the complete one lives in
    /// the chunk's arena and is resolved with `ResultSet::cell_full`. If the
    /// truncation is what you want (comparisons, previews), say so with
    /// `canonical_prefix()`; for clipboard and export of potentially LOB cells
    /// go through `cell_full`.
    ///
    /// Allocates: to be used for one cell at a time, not for a whole window.
    pub fn to_canonical_string(&self) -> Option<String> {
        Some(match self {
            ValueRef::Null => return None,
            ValueRef::Bool(v) => v.to_string(),
            ValueRef::I16(v) => v.to_string(),
            ValueRef::I32(v) => v.to_string(),
            ValueRef::I64(v) => v.to_string(),
            ValueRef::U64(v) => v.to_string(),
            // `{}` on f32/f64 in Rust produces the shortest form that
            // round-trips exactly: it is what §4.5 needs.
            ValueRef::F32(v) => v.to_string(),
            ValueRef::F64(v) => v.to_string(),
            ValueRef::Utf8(s) => (*s).to_owned(),
            ValueRef::Binary(b) => encode_hex_escape(b),
            ValueRef::Date32(d) => crate::fmt::format_date32(*d),
            ValueRef::Time64Micros(t) => crate::fmt::format_time64_micros(*t),
            ValueRef::TimestampMicros { micros, tz } => {
                crate::fmt::format_timestamp_micros(*micros, *tz)
            }
            ValueRef::Lob { prefix, .. } => String::from_utf8_lossy(prefix).into_owned(),
        })
    }

    /// Like `to_canonical_string`, but **the name tells the truth**: on `Lob`s
    /// the result is the truncated prefix (§4.7), not the whole value. To be
    /// used where truncation is acceptable by construction (sort comparisons
    /// on the mixed variant, previews); never for clipboard or export.
    ///
    /// `None` = NULL, like `to_canonical_string`.
    pub fn canonical_prefix(&self) -> Option<String> {
        self.to_canonical_string()
    }

    /// `true` if the canonical rendering of this value would be a truncated
    /// prefix and not the full value (i.e. if it is a `Lob`, §4.7).
    pub fn canonical_is_prefix(&self) -> bool {
        matches!(self, ValueRef::Lob { .. })
    }
}

/// `\x…` rendering of the bytes, the same PostgreSQL uses for `bytea`.
pub fn encode_hex_escape(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("\\x");
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}
