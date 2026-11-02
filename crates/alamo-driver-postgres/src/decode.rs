//! Decoding from PostgreSQL's **binary** format.
//!
//! ⚠️ **Note that counts as an M0 result** (open decision 3 of §14):
//! `tokio-postgres` asks the server for the **binary format for all
//! columns**, with no way to choose per column. Therefore:
//!
//!  - the types below decode well and fast;
//!  - the types **not** listed arrive as opaque bytes whose format we do not
//!    know, and there is no way to request them as text without going down
//!    to `postgres-protocol` and building the `Bind` by hand.
//!
//! Until that work exists, the unknown ones are rendered as `\x…` and the
//! column is marked `undecoded_wire_format` in the schema. It is an honest
//! fallback: ugly and visible, instead of silently wrong.

use std::fmt::Write as _;

use alamo_core::fmt as wfmt;
use alamo_core::value::encode_hex_escape;

use crate::oid;

#[inline]
pub fn read_i16(b: &[u8]) -> Option<i16> {
    Some(i16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}

#[inline]
pub fn read_i32(b: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

#[inline]
pub fn read_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

#[inline]
pub fn read_i64(b: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

#[inline]
pub fn read_f32(b: &[u8]) -> Option<f32> {
    Some(f32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

#[inline]
pub fn read_f64(b: &[u8]) -> Option<f64> {
    Some(f64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

/// Binary `jsonb` = 1 version byte (currently `1`) + JSON text.
/// Binary `json` = plain JSON text. Getting this detail wrong produces a `1`
/// at the head of every JSON value, which is the kind of bug that passes
/// review.
#[inline]
pub fn jsonb_text(b: &[u8]) -> &[u8] {
    match b.first() {
        Some(1) => &b[1..],
        _ => b,
    }
}

pub fn uuid_to_string(b: &[u8]) -> Option<String> {
    if b.len() != 16 {
        return None;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// NUMERIC
// ---------------------------------------------------------------------------

const NUMERIC_POS: u16 = 0x0000;
const NUMERIC_NEG: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_PINF: u16 = 0xD000;
const NUMERIC_NINF: u16 = 0xF000;

/// Binary `NUMERIC` -> text.
///
/// §4.5: PG's `NUMERIC` **stays `Utf8` permanently**. Precision and scale
/// may be unspecified and the values exceed any `Decimal128`: converting it
/// to a numeric type would be a lossy conversion disguised as a promotion.
/// In the schema the column carries `numeric_text = true`, so the grid
/// right-aligns it and sorts it numerically while treating it as text.
///
/// Format: `i16 ndigits`, `i16 weight`, `u16 sign`, `u16 dscale`, then
/// `ndigits` `i16` groups in base 10000. Group *i* has weight `weight - i`.
pub fn numeric_to_string(b: &[u8]) -> Option<String> {
    let ndigits = read_i16(b)?;
    let weight = read_i16(b.get(2..)?)? as i32;
    let sign = read_i16(b.get(4..)?)? as u16;
    let dscale = read_i16(b.get(6..)?)?;
    // Header from the wire, never trust it: a negative field cast to usize
    // would become ~2^64 and `with_capacity` would panic — against the
    // module's "degrade, never panic" contract.
    if ndigits < 0 || dscale < 0 {
        return None;
    }
    let ndigits = ndigits as usize;
    let dscale = dscale as usize;

    match sign {
        NUMERIC_NAN => return Some("NaN".to_owned()),
        NUMERIC_PINF => return Some("Infinity".to_owned()),
        NUMERIC_NINF => return Some("-Infinity".to_owned()),
        NUMERIC_POS | NUMERIC_NEG => {}
        _ => return None,
    }

    let mut digits = Vec::with_capacity(ndigits);
    for i in 0..ndigits {
        let d = read_i16(b.get(8 + i * 2..)?)?;
        // Valid base-10000 groups are in 0..=9999: out of range the wire is
        // malformed (same caution as the header above).
        if !(0..=9999).contains(&d) {
            return None;
        }
        digits.push(d);
    }

    let mut s = String::with_capacity(ndigits * 4 + dscale + 3);
    if sign == NUMERIC_NEG {
        s.push('-');
    }

    if weight < 0 {
        s.push('0');
    } else {
        for i in 0..=weight {
            let d = digits.get(i as usize).copied().unwrap_or(0);
            if i == 0 {
                // First group without leading zeros: written directly into
                // the buffer, no temporary String on the hot path.
                let _ = write!(s, "{d}");
            } else {
                push_group4(&mut s, d, 4);
            }
        }
    }

    if dscale > 0 {
        s.push('.');
        let mut written = 0usize;
        let mut i = weight + 1;
        while written < dscale {
            let d = if i >= 0 {
                digits.get(i as usize).copied().unwrap_or(0)
            } else {
                0
            };
            let take = 4.min(dscale - written);
            push_group4(&mut s, d, take);
            written += take;
            i += 1;
        }
    }
    Some(s)
}

/// The first `take` digits (leading zeros included) of a base-10000 group
/// already validated in 0..=9999, pushed by hand into the buffer:
/// `format!("{d:04}")` was an allocation per group, on every NUMERIC value.
#[inline]
fn push_group4(s: &mut String, d: i16, take: usize) {
    let d = d as u16;
    let ascii = [
        b'0' + (d / 1000) as u8,
        b'0' + (d / 100 % 10) as u8,
        b'0' + (d / 10 % 10) as u8,
        b'0' + (d % 10) as u8,
    ];
    for &c in &ascii[..take.min(4)] {
        s.push(c as char);
    }
}

// ---------------------------------------------------------------------------
// Array
// ---------------------------------------------------------------------------

/// Binary array -> PostgreSQL-style textual rendering (`{a,b,NULL}`).
///
/// Format: `i32 ndim`, `i32 has_null`, `u32 elem_oid`, then for each
/// dimension `i32 len` + `i32 lower_bound`, then the elements in row-major
/// order, each preceded by `i32 len` (-1 = NULL).
pub fn array_to_string(b: &[u8]) -> Option<String> {
    let ndim = read_i32(b)?;
    let elem_oid = read_u32(b.get(8..)?)?;
    if ndim == 0 {
        return Some("{}".to_owned());
    }
    // PG's MAXDIM is 6: out of range the header is malformed. Same caution
    // as `numeric_to_string`: negative or inflated dimensions must not
    // drive `with_capacity`.
    if !(1..=6).contains(&ndim) {
        return None;
    }
    let ndim = ndim as usize;
    let mut pos = 12;
    let mut dims = Vec::with_capacity(ndim);
    let mut total = 1usize;
    for _ in 0..ndim {
        let len = read_i32(b.get(pos..)?)?;
        if len < 0 {
            return None;
        }
        total = total.checked_mul(len as usize)?;
        dims.push(len as usize);
        pos += 8; // len + lower_bound
    }
    // Every element takes at least 4 bytes (length header): a total beyond
    // the remaining bytes is malformed, no point allocating.
    if total > b.len().saturating_sub(pos) / 4 {
        return None;
    }

    let mut elements: Vec<Option<String>> = Vec::with_capacity(total);
    for _ in 0..total {
        let len = read_i32(b.get(pos..)?)?;
        pos += 4;
        if len < 0 {
            elements.push(None);
            continue;
        }
        let len = len as usize;
        let raw = b.get(pos..pos + len)?;
        pos += len;
        elements.push(Some(
            decode_scalar_to_text(elem_oid, raw).unwrap_or_else(|| encode_hex_escape(raw)),
        ));
    }

    let mut out = String::new();
    render_dim(&dims, 0, &mut 0, &elements, &mut out);
    Some(out)
}

fn render_dim(
    dims: &[usize],
    depth: usize,
    cursor: &mut usize,
    elements: &[Option<String>],
    out: &mut String,
) {
    out.push('{');
    if depth + 1 == dims.len() {
        for i in 0..dims[depth] {
            if i > 0 {
                out.push(',');
            }
            match elements.get(*cursor) {
                Some(Some(v)) => out.push_str(&quote_array_element(v)),
                _ => out.push_str("NULL"),
            }
            *cursor += 1;
        }
    } else {
        for i in 0..dims[depth] {
            if i > 0 {
                out.push(',');
            }
            render_dim(dims, depth + 1, cursor, elements, out);
        }
    }
    out.push('}');
}

/// PostgreSQL's quoting rules. An element must be quoted if it is empty, if
/// it contains delimiters or spaces, or if it is literally `NULL` —
/// otherwise the string `"NULL"` would be indistinguishable from a real
/// NULL, which is the same §8.2 error in another place.
fn quote_array_element(v: &str) -> String {
    let needs_quote = v.is_empty()
        || v.eq_ignore_ascii_case("NULL")
        || v.chars()
            .any(|c| matches!(c, '{' | '}' | ',' | '"' | '\\') || c.is_whitespace());
    if !needs_quote {
        return v.to_owned();
    }
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

/// Binary `inet`/`cidr` (`network_send` in `utils/adt/network.c`): family
/// (2 = IPv4, 3 = IPv6), mask bits, cidr flag, length, address. Rendered
/// like PG's text output: `inet` omits the mask if it is full, `cidr`
/// always shows it.
pub fn inet_to_string(b: &[u8], cidr: bool) -> Option<String> {
    let (&family, rest) = b.split_first()?;
    let (&bits, rest) = rest.split_first()?;
    let (_is_cidr, rest) = rest.split_first()?;
    let (&len, addr) = rest.split_first()?;
    if addr.len() != len as usize {
        return None;
    }
    let (ip, max_bits) = match (family, addr.len()) {
        (2, 4) => {
            let a: [u8; 4] = addr.try_into().ok()?;
            (std::net::IpAddr::from(a), 32)
        }
        (3, 16) => {
            let a: [u8; 16] = addr.try_into().ok()?;
            (std::net::IpAddr::from(a), 128)
        }
        _ => return None,
    };
    if bits > max_bits {
        return None;
    }
    Some(if cidr || bits != max_bits {
        format!("{ip}/{bits}")
    } else {
        ip.to_string()
    })
}

/// `macaddr` (6 bytes) and `macaddr8` (8 bytes): lowercase hexadecimal
/// separated by `:`, like PG's text output.
pub fn macaddr_to_string(b: &[u8]) -> Option<String> {
    if b.len() != 6 && b.len() != 8 {
        return None;
    }
    let mut s = String::with_capacity(b.len() * 3);
    for (i, byte) in b.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        let _ = write!(s, "{byte:02x}");
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// Scalars -> text
// ---------------------------------------------------------------------------

/// Renders a binary value of known OID as text. `None` if the OID is not
/// among the decodable ones.
pub fn decode_scalar_to_text(oid_value: u32, raw: &[u8]) -> Option<String> {
    Some(match oid_value {
        oid::BOOL => (if raw.first().copied().unwrap_or(0) != 0 {
            "true"
        } else {
            "false"
        })
        .to_owned(),
        oid::INT2 => read_i16(raw)?.to_string(),
        oid::INT4 => read_i32(raw)?.to_string(),
        oid::OID => read_u32(raw)?.to_string(),
        oid::INT8 => read_i64(raw)?.to_string(),
        oid::FLOAT4 => read_f32(raw)?.to_string(),
        oid::FLOAT8 => read_f64(raw)?.to_string(),
        oid::NUMERIC => numeric_to_string(raw)?,
        oid::UUID => uuid_to_string(raw)?,
        oid::TEXT | oid::VARCHAR | oid::BPCHAR | oid::NAME | oid::CHAR | oid::XML | oid::JSON => {
            String::from_utf8_lossy(raw).into_owned()
        }
        oid::JSONB => String::from_utf8_lossy(jsonb_text(raw)).into_owned(),
        oid::BYTEA => encode_hex_escape(raw),
        oid::INET | oid::CIDR => inet_to_string(raw, oid_value == oid::CIDR)?,
        oid::MACADDR | oid::MACADDR8 => macaddr_to_string(raw)?,
        oid::DATE => {
            let d = read_i32(raw)?;
            match date_special(d) {
                Some(s) => s.to_owned(),
                None => wfmt::format_date32(d + oid::PG_EPOCH_DAYS),
            }
        }
        oid::TIME => wfmt::format_time64_micros(read_i64(raw)?),
        oid::TIMESTAMP | oid::TIMESTAMPTZ => {
            let t = read_i64(raw)?;
            match timestamp_special(t) {
                Some(s) => s.to_owned(),
                None => wfmt::format_timestamp_micros(
                    t + oid::PG_EPOCH_MICROS,
                    oid_value == oid::TIMESTAMPTZ,
                ),
            }
        }
        _ if oid::is_array(oid_value) => array_to_string(raw)?,
        _ => return None,
    })
}

/// §4.6: `infinity` and `-infinity` on dates and timestamps **cannot be
/// represented by any integer**. Recognising them is mandatory: without it,
/// `infinity` would become a date in 5874897 AD — a wrong value that looks
/// like a right one.
#[inline]
pub fn date_special(days: i32) -> Option<&'static str> {
    match days {
        i32::MAX => Some("infinity"),
        i32::MIN => Some("-infinity"),
        _ => None,
    }
}

#[inline]
pub fn timestamp_special(micros: i64) -> Option<&'static str> {
    match micros {
        i64::MAX => Some("infinity"),
        i64::MIN => Some("-infinity"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric_bytes(ndigits: i16, weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&ndigits.to_be_bytes());
        b.extend_from_slice(&weight.to_be_bytes());
        b.extend_from_slice(&(sign as i16).to_be_bytes());
        b.extend_from_slice(&(dscale as i16).to_be_bytes());
        for d in digits {
            b.extend_from_slice(&d.to_be_bytes());
        }
        b
    }

    #[test]
    fn numeric_integer_and_fraction() {
        // 1234.56 -> base-10000 groups: [1234, 5600], weight 0, dscale 2
        let b = numeric_bytes(2, 0, NUMERIC_POS, 2, &[1234, 5600]);
        assert_eq!(numeric_to_string(&b).as_deref(), Some("1234.56"));
    }

    #[test]
    fn numeric_below_one_keeps_leading_zeros() {
        // 0.0042 -> weight -1, group [42] with weight -1 => positions 1..4
        let b = numeric_bytes(1, -1, NUMERIC_POS, 4, &[42]);
        assert_eq!(numeric_to_string(&b).as_deref(), Some("0.0042"));
    }

    #[test]
    fn numeric_negative_and_nan() {
        let b = numeric_bytes(1, 0, NUMERIC_NEG, 0, &[7]);
        assert_eq!(numeric_to_string(&b).as_deref(), Some("-7"));
        let b = numeric_bytes(0, 0, NUMERIC_NAN, 0, &[]);
        assert_eq!(numeric_to_string(&b).as_deref(), Some("NaN"));
    }

    #[test]
    fn malformed_headers_return_none_instead_of_panicking() {
        // Negative ndigits from the wire.
        let b = numeric_bytes(-1, 0, NUMERIC_POS, 0, &[]);
        assert_eq!(numeric_to_string(&b), None);
        // Negative dscale.
        let b = numeric_bytes(1, 0, NUMERIC_POS, 0xFFFF, &[7]);
        assert_eq!(numeric_to_string(&b), None);
        // Base-10000 group outside 0..=9999.
        let b = numeric_bytes(1, 0, NUMERIC_POS, 0, &[-3]);
        assert_eq!(numeric_to_string(&b), None);
        let b = numeric_bytes(1, 0, NUMERIC_POS, 0, &[10_000]);
        assert_eq!(numeric_to_string(&b), None);

        // Array with negative ndim.
        let mut a = Vec::new();
        a.extend_from_slice(&(-1i32).to_be_bytes());
        a.extend_from_slice(&0i32.to_be_bytes());
        a.extend_from_slice(&23u32.to_be_bytes());
        assert_eq!(array_to_string(&a), None);

        // Array with dimensions inflated beyond the available bytes.
        let mut a = Vec::new();
        a.extend_from_slice(&2i32.to_be_bytes());
        a.extend_from_slice(&0i32.to_be_bytes());
        a.extend_from_slice(&23u32.to_be_bytes());
        for _ in 0..2 {
            a.extend_from_slice(&1_000_000i32.to_be_bytes());
            a.extend_from_slice(&1i32.to_be_bytes());
        }
        assert_eq!(array_to_string(&a), None);
    }

    #[test]
    fn jsonb_version_byte_is_stripped() {
        assert_eq!(jsonb_text(&[1, b'{', b'}']), b"{}");
        assert_eq!(jsonb_text(b"{}"), b"{}");
    }

    #[test]
    fn uuid_rendering() {
        let bytes: Vec<u8> = (0u8..16).collect();
        assert_eq!(
            uuid_to_string(&bytes).as_deref(),
            Some("00010203-0405-0607-0809-0a0b0c0d0e0f")
        );
    }

    #[test]
    fn array_quoting_distinguishes_null_from_the_string_null() {
        assert_eq!(quote_array_element("NULL"), "\"NULL\"");
        assert_eq!(quote_array_element("plain"), "plain");
        assert_eq!(quote_array_element("two words"), "\"two words\"");
        assert_eq!(quote_array_element(""), "\"\"");
    }

    #[test]
    fn inet_and_cidr_like_pg_text_output() {
        // 172.18.0.2/32: the case seen during manual validation.
        let host = [2, 32, 0, 4, 172, 18, 0, 2];
        assert_eq!(inet_to_string(&host, false).as_deref(), Some("172.18.0.2"));
        assert_eq!(
            inet_to_string(&host, true).as_deref(),
            Some("172.18.0.2/32")
        );
        let net = [2, 24, 1, 4, 10, 0, 0, 0];
        assert_eq!(inet_to_string(&net, true).as_deref(), Some("10.0.0.0/24"));
        let mut v6 = vec![3, 128, 0, 16];
        v6.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(inet_to_string(&v6, false).as_deref(), Some("2001:db8::1"));
        // Inconsistent length or unknown family: None, never a panic.
        assert_eq!(inet_to_string(&[2, 32, 0, 4, 1, 2], false), None);
        assert_eq!(inet_to_string(&[9, 32, 0, 4, 1, 2, 3, 4], false), None);
        assert_eq!(inet_to_string(&[2, 33, 0, 4, 1, 2, 3, 4], false), None);
    }

    #[test]
    fn macaddr_6_and_8_bytes() {
        assert_eq!(
            macaddr_to_string(&[0x08, 0x00, 0x2b, 0x01, 0x02, 0x03]).as_deref(),
            Some("08:00:2b:01:02:03")
        );
        assert_eq!(
            macaddr_to_string(&[0x08, 0x00, 0x2b, 0x01, 0x02, 0x03, 0x04, 0x05]).as_deref(),
            Some("08:00:2b:01:02:03:04:05")
        );
        assert_eq!(macaddr_to_string(&[1, 2, 3]), None);
    }

    #[test]
    fn infinity_is_detected_not_rendered_as_a_far_future_date() {
        assert_eq!(date_special(i32::MAX), Some("infinity"));
        assert_eq!(timestamp_special(i64::MIN), Some("-infinity"));
        assert_eq!(date_special(0), None);
    }
}
