//! Tiny time helpers — no chrono dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (saturating to 0 if the clock is before 1970).
pub(crate) fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Current time as an `YYYY-MM-DDTHH:MM:SSZ` string (civil-from-days, UTC).
pub(crate) fn iso_now() -> String {
    iso_from_secs(unix_secs())
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` using Howard Hinnant's
/// days-from-civil algorithm. Pure integer math, no external crate.
pub(crate) fn iso_from_secs(secs: i64) -> String {
    let z = secs + 719_468 * 86_400;
    let days = z.div_euclid(86_400);
    let secs_of_day = z.rem_euclid(86_400);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day / 60) % 60;
    let ss = secs_of_day % 60;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_adj = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y_adj + 1 } else { y_adj };
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_from_secs_matches_known_epochs() {
        assert_eq!(iso_from_secs(0), "1970-01-01T00:00:00Z");
        // 2021-01-01T00:00:00Z
        assert_eq!(iso_from_secs(1_609_459_200), "2021-01-01T00:00:00Z");
        // 2026-06-01T12:34:56Z
        assert_eq!(iso_from_secs(1_780_317_296), "2026-06-01T12:34:56Z");
    }
}
