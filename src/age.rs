//! Compact relative ages for the menu: "now", "3m", "3h 20m", "5d 4h", "1w 6d", "2mo 1w", "1y 3mo".
//! Months are "mo" so they can't be confused with minutes.

use std::time::{SystemTime, UNIX_EPOCH};

/// Parses GitHub's UTC timestamps ("2026-11-09T20:53:12Z") to Unix seconds.
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Largest unit plus the next one down, both rounded down: "1w 6d", "3h 20m",
/// "2mo 1w", "1y 3mo". The second part is dropped when it is zero.
/// Months are 30 days and years 365 days.
pub fn short(secs: i64) -> String {
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    let two = |a: i64, ua: &str, b: i64, ub: &str| if b > 0 { format!("{a}{ua} {b}{ub}") } else { format!("{a}{ua}") };
    match () {
        _ if mins < 1 => "now".into(),
        _ if hours < 1 => format!("{mins}m"),
        _ if days < 1 => two(hours, "h", mins % 60, "m"),
        _ if days < 7 => two(days, "d", hours % 24, "h"),
        _ if days < 30 => two(days / 7, "w", days % 7, "d"),
        _ if days < 365 => two(days / 30, "mo", days % 30 / 7, "w"),
        _ => two(days / 365, "y", days % 365 / 30, "mo"),
    }
}

/// Age of a GitHub timestamp relative to now, or `None` if it can't be parsed.
pub fn since(timestamp: &str) -> Option<String> {
    let then = parse_utc(timestamp)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(short(now - then))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_utc("2026-11-09T20:53:12Z"), Some(1_794_257_592));
        assert_eq!(parse_utc("garbage"), None);
    }

    #[test]
    fn formats() {
        let (m, h, d) = (60, 3600, 86_400);
        assert_eq!(short(-5), "now");
        assert_eq!(short(59), "now");
        assert_eq!(short(3 * m), "3m");
        assert_eq!(short(59 * m), "59m");
        assert_eq!(short(4 * h), "4h");
        assert_eq!(short(3 * h + 20 * m), "3h 20m");
        assert_eq!(short(6 * d + 5 * h), "6d 5h");
        assert_eq!(short(13 * d + 7 * h), "1w 6d"); // the libron case
        assert_eq!(short(14 * d), "2w");
        assert_eq!(short(29 * d), "4w 1d");
        assert_eq!(short(30 * d), "1mo");
        assert_eq!(short(59 * d), "1mo 4w");
        assert_eq!(short(364 * d), "12mo"); // 30-day months: 360-364 days
        assert_eq!(short(365 * d), "1y");
        assert_eq!(short(800 * d), "2y 2mo");
    }
}
