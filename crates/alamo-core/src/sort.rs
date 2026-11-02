//! Local sort via permutation (§4.8, decision 10).
//!
//! **The sort does not move data**: it produces a `Vec<u32>` of global indices
//! through which the window is read. Zero buffer copies, and the future move
//! to DataFusion requires no redesign. The same permutation will carry the
//! local filter (M3+) without changing the read path.
//!
//! §7.3: on an incomplete result set the permutation covers only the rows
//! downloaded — telling the user is the UI's job (badge), not this module's.

use std::cmp::Ordering;
use std::ops::Range;

use crate::result_set::ResultSet;
use crate::value::ValueRef;
use crate::window::Window;

/// Total order for sorting within ONE column (types are per column, §4.2
/// rule 3, so mixed comparisons are the exception, not the rule).
///
/// PostgreSQL semantics: NULL is the largest value — at the end in ASC, at
/// the start in DESC (the DESC permutation is the reverse of the comparison).
pub fn cmp_values(a: &ValueRef<'_>, b: &ValueRef<'_>) -> Ordering {
    use ValueRef::*;
    match (a, b) {
        (Null, Null) => Ordering::Equal,
        (Null, _) => Ordering::Greater,
        (_, Null) => Ordering::Less,
        (Bool(x), Bool(y)) => x.cmp(y),
        (I16(x), I16(y)) => x.cmp(y),
        (I32(x), I32(y)) => x.cmp(y),
        (I64(x), I64(y)) => x.cmp(y),
        (U64(x), U64(y)) => x.cmp(y),
        // total_cmp: NaNs go to the end instead of breaking transitivity
        // (sort_by with a non-total comparator = undefined order).
        (F32(x), F32(y)) => x.total_cmp(y),
        (F64(x), F64(y)) => x.total_cmp(y),
        (Utf8(x), Utf8(y)) => x.cmp(y),
        (Binary(x), Binary(y)) => x.cmp(y),
        (Date32(x), Date32(y)) => x.cmp(y),
        (Time64Micros(x), Time64Micros(y)) => x.cmp(y),
        (TimestampMicros { micros: x, .. }, TimestampMicros { micros: y, .. }) => x.cmp(y),
        (Lob { prefix: x, .. }, Lob { prefix: y, .. }) => x.cmp(y),
        // Mixed variant (column degraded midway, Lob/Utf8 comparison):
        // canonical form, allocating but rare by construction. The truncated
        // prefix of Lobs is acceptable here: it is a sort, not an export
        // (`canonical_prefix` states so, §4.7).
        _ => a
            .canonical_prefix()
            .unwrap_or_default()
            .cmp(&b.canonical_prefix().unwrap_or_default()),
    }
}

/// Like `cmp_values`, but text is compared **numerically**: for columns with
/// `ColumnMeta::numeric_text` (PG's `NUMERIC` stays `Utf8` permanently, §4.5)
/// lexicographic order would say `"10" < "9"`.
pub fn cmp_values_numeric_text(a: &ValueRef<'_>, b: &ValueRef<'_>) -> Ordering {
    match (a, b) {
        (ValueRef::Utf8(x), ValueRef::Utf8(y)) => cmp_numeric_text(x, y),
        _ => cmp_values(a, b),
    }
}

/// Ordering classes of numeric text. The order is PostgreSQL's for `NUMERIC`:
/// `-Infinity < finite numbers < Infinity < NaN`.
/// Text that is none of the four ends up with `NaN` at the end (maximum
/// class) and within it is ordered lexicographically: deterministic, and
/// NULLs still stay beyond (handled in `cmp_values*`).
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum NumClass {
    NegInfinity,
    Finite,
    PosInfinity,
    NotANumber,
}

/// Parts of a decimal number in text, already normalized for comparison:
/// integer part without leading zeros, fractional part without trailing zeros.
struct NumParts<'a> {
    neg: bool,
    int: &'a str,
    frac: &'a str,
}

/// Parses `[+|-]digits[.digits]` (at least one digit). No exponential
/// notation: PG does not emit it for `NUMERIC`; whatever does not parse ends
/// up in the `NotANumber` class, never in a wrong order.
fn parse_numeric_text(s: &str) -> Option<NumParts<'_>> {
    let rest = s.strip_prefix(['-', '+']).unwrap_or(s);
    let neg = s.starts_with('-');
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, f),
        None => (rest, ""),
    };
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(NumParts {
        neg,
        int: int.trim_start_matches('0'),
        frac: frac.trim_end_matches('0'),
    })
}

fn numeric_class(s: &str) -> NumClass {
    if s.eq_ignore_ascii_case("-infinity") || s.eq_ignore_ascii_case("-inf") {
        NumClass::NegInfinity
    } else if s.eq_ignore_ascii_case("infinity")
        || s.eq_ignore_ascii_case("inf")
        || s.eq_ignore_ascii_case("+infinity")
        || s.eq_ignore_ascii_case("+inf")
    {
        NumClass::PosInfinity
    } else {
        NumClass::NotANumber
    }
}

/// Numeric comparison of two numbers in text **without parsing them into a
/// numeric type**: precision and scale of PG's `NUMERIC` exceed any
/// `Decimal128` (§4.5), so the digits are compared.
///
///  - sign first of all (`-0` counts as `0`);
///  - integer part without leading zeros: first by length, then
///    lexicographic;
///  - on a tie, fractional part without trailing zeros, lexicographic (the
///    shorter prefix is smaller: `0.5 < 0.55`).
pub fn cmp_numeric_text(a: &str, b: &str) -> Ordering {
    match (parse_numeric_text(a), parse_numeric_text(b)) {
        (Some(x), Some(y)) => cmp_finite(&x, &y),
        (Some(_), None) => NumClass::Finite.cmp(&numeric_class(b)),
        (None, Some(_)) => numeric_class(a).cmp(&NumClass::Finite),
        (None, None) => {
            let (ca, cb) = (numeric_class(a), numeric_class(b));
            // Within the residual class, lexicographic order keeps the
            // comparator total and deterministic ("NaN" == "NaN").
            ca.cmp(&cb).then_with(|| {
                if ca == NumClass::NotANumber {
                    a.cmp(b)
                } else {
                    Ordering::Equal
                }
            })
        }
    }
}

fn cmp_finite(a: &NumParts<'_>, b: &NumParts<'_>) -> Ordering {
    let a_neg = a.neg && !(a.int.is_empty() && a.frac.is_empty());
    let b_neg = b.neg && !(b.int.is_empty() && b.frac.is_empty());
    match (a_neg, b_neg) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let magnitude = a
        .int
        .len()
        .cmp(&b.int.len())
        .then_with(|| a.int.cmp(b.int))
        .then_with(|| a.frac.cmp(b.frac));
    if a_neg {
        magnitude.reverse()
    } else {
        magnitude
    }
}

impl ResultSet {
    /// Sort permutation on column `col` (§4.8). Covers the rows **present
    /// now**: on a still-open stream it must be recomputed when the count
    /// grows.
    ///
    /// Stable: on equal values the arrival order is kept, so successive sorts
    /// on different columns compose as the user expects.
    ///
    /// Columns with `ColumnMeta::numeric_text` (PG's `NUMERIC` stays text,
    /// §4.5) are sorted numerically, not lexicographically.
    pub fn sort_permutation(&self, col: usize, desc: bool) -> Vec<u32> {
        let n = self.rows().min(u32::MAX as u64) as u32;
        // Materialize the views ONCE: immutable borrows from the chunks, zero
        // data copies. The alternative — `value_at` inside the comparator —
        // costs ~2·n·log n binary searches (`locate`) plus rebuilding the
        // ValueRef for EVERY comparison. The ValueRefs borrow from the chunks
        // and stay in scope until the end of the function.
        let mut values: Vec<ValueRef<'_>> = Vec::with_capacity(n as usize);
        for chunk in self.chunks() {
            for offset in 0..chunk.rows() as usize {
                if values.len() == n as usize {
                    break;
                }
                values.push(chunk.value_at(offset, col).unwrap_or(ValueRef::Null));
            }
        }
        let cmp: fn(&ValueRef<'_>, &ValueRef<'_>) -> Ordering =
            if self.schema().get(col).is_some_and(|m| m.numeric_text) {
                cmp_values_numeric_text
            } else {
                cmp_values
            };
        let mut perm: Vec<u32> = (0..n).collect();
        perm.sort_by(|&i, &j| {
            let ord = cmp(&values[i as usize], &values[j as usize]);
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        perm
    }

    /// Window read through a permutation: `rows` is in view space, the
    /// physical indices come out of `perm`. Clamps to the length of the
    /// permutation, not to the total row count: rows that arrived AFTER the
    /// sort have no position in the view until it is recomputed.
    pub fn window_permuted(&self, rows: Range<u64>, cols: Range<usize>, perm: &[u32]) -> Window {
        let end = rows.end.min(perm.len() as u64);
        let start = rows.start.min(end);
        self.window_mapped(start..end, cols, |r| perm[r as usize] as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkBuilder, ChunkLimits};
    use crate::column::ColumnKind;
    use crate::meta::{ColumnMeta, NativeType};
    use std::sync::Arc;

    fn rs_i64(values: &[Option<i64>]) -> ResultSet {
        let schema: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "n",
            NativeType::Postgres {
                oid: 20,
                type_modifier: -1,
            },
            "int8",
        )]
        .into();
        let mut b = ChunkBuilder::new(&[ColumnKind::I64], ChunkLimits::default());
        for v in values {
            match v {
                Some(x) => b.column_mut(0).unwrap().push_i64(*x).unwrap(),
                None => b.column_mut(0).unwrap().push_null(),
            }
            b.end_row();
        }
        let mut rs = ResultSet::new(schema);
        rs.push_chunk(b.finish().0);
        rs
    }

    /// Text column with `numeric_text` (PG's NUMERIC, oid 1700).
    fn rs_numeric_text(values: &[Option<&str>]) -> ResultSet {
        let mut meta = ColumnMeta::new(
            "n",
            NativeType::Postgres {
                oid: 1700,
                type_modifier: -1,
            },
            "numeric",
        );
        meta.numeric_text = true;
        let schema: Arc<[ColumnMeta]> = vec![meta].into();
        let mut b = ChunkBuilder::new(&[ColumnKind::Utf8], ChunkLimits::default());
        for v in values {
            match v {
                Some(s) => b.push_text(0, s).unwrap(),
                None => b.column_mut(0).unwrap().push_null(),
            }
            b.end_row();
        }
        let mut rs = ResultSet::new(schema);
        rs.push_chunk(b.finish().0);
        rs
    }

    #[test]
    fn numeric_text_sorts_numerically_not_lexicographically() {
        // Lexicographic would say "-2" < "0.05" < "0.5" < "10" < "9" < "NaN"…
        // almost right by chance; the killer case is "10" < "9".
        let rs = rs_numeric_text(&[
            Some("10"),
            Some("9"),
            Some("-2"),
            Some("0.5"),
            Some("0.05"),
            Some("NaN"),
        ]);
        // Expected: -2, 0.05, 0.5, 9, 10, NaN.
        assert_eq!(rs.sort_permutation(0, false), vec![2, 4, 3, 1, 0, 5]);
        assert_eq!(rs.sort_permutation(0, true), vec![5, 0, 1, 3, 4, 2]);
    }

    #[test]
    fn numeric_text_nulls_still_sort_last_asc() {
        let rs = rs_numeric_text(&[Some("2"), None, Some("10")]);
        assert_eq!(rs.sort_permutation(0, false), vec![0, 2, 1]);
    }

    #[test]
    fn cmp_numeric_text_cases() {
        use Ordering::*;
        // The case that breaks lexicographic order.
        assert_eq!(cmp_numeric_text("10", "9"), Greater);
        assert_eq!(cmp_numeric_text("9", "10"), Less);
        // Leading zeros and sign.
        assert_eq!(cmp_numeric_text("007", "7"), Equal);
        assert_eq!(cmp_numeric_text("-0", "0"), Equal);
        assert_eq!(cmp_numeric_text("+3", "3"), Equal);
        assert_eq!(cmp_numeric_text("-10", "-9"), Less);
        assert_eq!(cmp_numeric_text("-1", "1"), Less);
        // Decimal part: trailing zeros ignored, shorter prefix is smaller.
        assert_eq!(cmp_numeric_text("0.5", "0.50"), Equal);
        assert_eq!(cmp_numeric_text("0.5", "0.55"), Less);
        assert_eq!(cmp_numeric_text("0.5", "0.45"), Greater);
        assert_eq!(cmp_numeric_text("2", "2.1"), Less);
        // PG's special classes: -Infinity < finite < Infinity < NaN.
        assert_eq!(cmp_numeric_text("-Infinity", "-999999"), Less);
        assert_eq!(cmp_numeric_text("Infinity", "999999"), Greater);
        assert_eq!(cmp_numeric_text("NaN", "Infinity"), Greater);
        assert_eq!(cmp_numeric_text("NaN", "NaN"), Equal);
        // Non-numeric text: at the end with NaN, ordered lexicographically
        // (deterministic, never a panic nor a non-total order).
        assert_eq!(cmp_numeric_text("abc", "5"), Greater);
        assert_eq!(cmp_numeric_text("abc", "abd"), Less);
    }

    #[test]
    fn sort_asc_nulls_last_desc_nulls_first() {
        let rs = rs_i64(&[Some(3), None, Some(1), Some(2)]);
        assert_eq!(rs.sort_permutation(0, false), vec![2, 3, 0, 1]);
        assert_eq!(rs.sort_permutation(0, true), vec![1, 0, 3, 2]);
    }

    #[test]
    fn window_permuted_reads_view_space() {
        let rs = rs_i64(&[Some(3), Some(1), Some(2)]);
        let perm = rs.sort_permutation(0, false);
        let w = rs.window_permuted(0..3, 0..1, &perm);
        assert_eq!(w.rows, 0..3);
        assert_eq!(w.columns[0].values, vec!["1", "2", "3"]);
    }

    #[test]
    fn window_permuted_clamps_to_perm_not_rows() {
        // Stream grown after the sort: the view stays as long as the
        // permutation.
        let mut rs = rs_i64(&[Some(2), Some(1)]);
        let perm = rs.sort_permutation(0, false);
        let more = rs_i64(&[Some(0)]);
        rs.push_chunk(more.chunks()[0].clone());
        assert_eq!(rs.rows(), 3);
        let w = rs.window_permuted(0..3, 0..1, &perm);
        assert_eq!(w.rows, 0..2);
        assert_eq!(w.columns[0].values, vec!["1", "2"]);
    }
}
