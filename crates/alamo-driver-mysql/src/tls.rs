//! What the server says about the channel (§12.2). `mysql_async` keeps the
//! rustls session private, so protocol, cipher and the validity of the
//! server certificate come from the session status variables. Subject and
//! issuer are not among them: on MySQL/MariaDB the UI shows them as not
//! available.

use mysql_async::prelude::Queryable;
use mysql_async::Conn;

use crate::MyError;

/// The `Ssl_*` status of the current session; every field is as the
/// server gave it (`None` = variable missing or unreadable).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TlsStatus {
    pub version: Option<String>,
    pub cipher: Option<String>,
    /// Unix seconds, from `Ssl_server_not_before`/`_after`.
    pub not_before: Option<i64>,
    pub not_after: Option<i64>,
}

/// `None` = the session is in plaintext (empty `Ssl_cipher`).
pub async fn tls_status(conn: &mut Conn) -> Result<Option<TlsStatus>, MyError> {
    let rows: Vec<(String, String)> = conn
        .query(
            "SHOW SESSION STATUS WHERE Variable_name IN \
             ('Ssl_version', 'Ssl_cipher', 'Ssl_server_not_before', 'Ssl_server_not_after')",
        )
        .await
        .map_err(MyError::from)?;
    let mut status = TlsStatus::default();
    for (name, value) in rows {
        let value = Some(value).filter(|v| !v.is_empty());
        match name.to_ascii_lowercase().as_str() {
            "ssl_version" => status.version = value,
            "ssl_cipher" => status.cipher = value,
            "ssl_server_not_before" => status.not_before = value.as_deref().and_then(parse_time),
            "ssl_server_not_after" => status.not_after = value.as_deref().and_then(parse_time),
            _ => {}
        }
    }
    Ok(status.cipher.is_some().then_some(status))
}

/// The server prints the dates as OpenSSL does (`Nov  4 10:00:00 2026 GMT`).
/// Anything else = `None`: a date we cannot read is "not available", never a
/// failed connection.
fn parse_time(s: &str) -> Option<i64> {
    let mut parts = s.split_whitespace();
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let day: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    let year: i64 = parts.next()?.parse().ok()?;
    if parts.next() != Some("GMT") || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// algorithm), the same arithmetic the std-only core would use.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openssl_dates() {
        assert_eq!(parse_time("Jan  1 00:00:00 1970 GMT"), Some(0));
        assert_eq!(parse_time("Nov  4 10:00:00 2026 GMT"), Some(1_793_786_400));
        assert_eq!(parse_time("Feb 29 23:59:59 2024 GMT"), Some(1_709_251_199));
    }

    #[test]
    fn unreadable_dates_are_none() {
        for s in [
            "",
            "2026-11-04 10:00:00",
            "Nov 4 10:00 2026 GMT",
            "Nov 40 10:00:00 2026 GMT",
        ] {
            assert_eq!(parse_time(s), None, "{s:?}");
        }
    }
}
