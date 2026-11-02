//! Canonical date/time formatting.
//!
//! It serves two purposes:
//!  1. the **degradation to `Utf8`** of §4.6, which must rewrite as text the
//!     typed values already accumulated;
//!  2. the **canonical form** of §8.2, which must always stay available for
//!     clipboard copy and export, independently of the localized rendering.
//!
//! No external dependencies: the civil-days algorithm is Hinnant's, valid over
//! the whole proleptic Gregorian range.

/// Days since the Unix epoch -> (year, month, day).
pub fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// (year, month, day) -> days since the Unix epoch. Exact inverse of
/// `civil_from_days`.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = (if m <= 2 { y - 1 } else { y }) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as i32
}

/// `YYYY-MM-DD`, with negative years rendered as `-YYYY-MM-DD`.
pub fn format_date32(days: i32) -> String {
    let (y, m, d) = civil_from_days(days);
    if y < 0 {
        format!("-{:04}-{:02}-{:02}", -y, m, d)
    } else {
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// `HH:MM:SS` or `HH:MM:SS.ffffff` if there are microseconds.
///
/// WARNING: this format is for the time of day. MySQL's `TIME` ranges from
/// `-838:59:59` to `838:59:59` and **is a duration, not a time of day**
/// (§4.6): it does not fit in `Time64Micros` and must be handled through
/// degradation.
pub fn format_time64_micros(micros: i64) -> String {
    // `unsigned_abs`, not `-micros`: the negation of `i64::MIN` is not
    // representable in i64 and panics in debug. From here on everything in u64.
    let sign = if micros < 0 { "-" } else { "" };
    let micros = micros.unsigned_abs();
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let h = secs / 3600;
    let mi = (secs % 3600) / 60;
    let s = secs % 60;
    if frac == 0 {
        format!("{sign}{h:02}:{mi:02}:{s:02}")
    } else {
        format!("{sign}{h:02}:{mi:02}:{s:02}.{frac:06}")
    }
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]`, with a `+00` suffix if `tz` is true.
///
/// Timestamptz values are kept in UTC: the **display** timezone is
/// presentation (§8.2) and is applied elsewhere; this is the canonical form.
pub fn format_timestamp_micros(micros: i64, tz: bool) -> String {
    let day = micros.div_euclid(86_400_000_000);
    let rem = micros.rem_euclid(86_400_000_000);
    let date = format_date32(day as i32);
    let time = format_time64_micros(rem);
    if tz {
        format!("{date} {time}+00")
    } else {
        format!("{date} {time}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(format_date32(0), "1970-01-01");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        // 2000-01-01, PostgreSQL's internal epoch
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(format_date32(10_957), "2000-01-01");
    }

    #[test]
    fn roundtrip_range() {
        for d in [-400_000i32, -1, 0, 1, 19_000, 100_000, 2_000_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d, "day {d}");
        }
    }

    #[test]
    fn timestamp_formatting() {
        assert_eq!(format_timestamp_micros(0, true), "1970-01-01 00:00:00+00");
        assert_eq!(
            format_timestamp_micros(1_000_000 * 3661 + 123_456, false),
            "1970-01-01 01:01:01.123456"
        );
    }

    #[test]
    fn time_extremes_do_not_overflow() {
        // `-i64::MIN` does not exist in i64: without `unsigned_abs` it panics in debug.
        assert_eq!(format_time64_micros(i64::MIN), "-2562047788:00:54.775808");
        assert_eq!(format_time64_micros(i64::MAX), "2562047788:00:54.775807");
        assert_eq!(format_time64_micros(-1), "-00:00:00.000001");
    }

    #[test]
    fn negative_timestamp_uses_euclidean_division() {
        // One microsecond before the epoch: the date must go back one day,
        // not produce a negative time.
        assert_eq!(
            format_timestamp_micros(-1, false),
            "1969-12-31 23:59:59.999999"
        );
    }
}
