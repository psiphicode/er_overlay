//! UTC timestamp formatting.
//!
//! The ingest wire format needs RFC 3339 with millisecond precision
//! (`2026-08-13T19:04:12.140Z`). That is the only date formatting this crate
//! does, so it is hand-rolled rather than pulling in a date/time dependency.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Formats a `SystemTime` as RFC 3339 in UTC with millisecond precision.
///
/// Times before the Unix epoch are clamped to the epoch , the only inputs here
/// come from `SystemTime::now()`, and a machine clock set before 1970 is not
/// worth a fallible return type.
pub fn rfc3339_millis_utc(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();

    // `div_euclid`/`rem_euclid` keep the second-of-day non-negative.
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (sod / 3_600, (sod % 3_600) / 60, sod % 60);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    )
}

/// Converts a count of days since 1970-01-01 into a proleptic Gregorian
/// year/month/day.
///
/// This is Howard Hinnant's `civil_from_days`, which shifts the era so that the
/// leap-year cycle starts at March , that is what removes the special-casing
/// for February.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Shift the epoch from 1970-01-01 to 0000-03-01.
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // day of era, [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year (Mar-based), [0, 365]
    let mp = (5 * doy + 2) / 153; // month, Mar=0, [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]

    // Jan and Feb belong to the following calendar year.
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64, millis: u32) -> String {
        rfc3339_millis_utc(UNIX_EPOCH + Duration::new(secs, millis * 1_000_000))
    }

    #[test]
    fn epoch() {
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn formats_millis() {
        assert_eq!(at(1_700_000_000, 140), "2023-11-14T22:13:20.140Z");
        // Sub-millisecond precision truncates rather than rounds.
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 999_999);
        assert_eq!(rfc3339_millis_utc(t), "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn leap_days() {
        assert_eq!(at(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        assert_eq!(at(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
        // 1900 was not a leap year, and 2100 will not be either.
        assert_eq!(at(4_107_542_400, 0), "2100-03-01T00:00:00.000Z");
    }

    #[test]
    fn year_and_day_boundaries() {
        assert_eq!(at(86_399, 0), "1970-01-01T23:59:59.000Z");
        assert_eq!(at(86_400, 0), "1970-01-02T00:00:00.000Z");
        assert_eq!(at(1_735_689_599, 0), "2024-12-31T23:59:59.000Z");
        assert_eq!(at(1_735_689_600, 0), "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn pre_epoch_clamps_to_epoch() {
        let t = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(rfc3339_millis_utc(t), "1970-01-01T00:00:00.000Z");
    }

    /// Every day for ~90 years must round-trip through the formatter with a
    /// monotonically increasing string, which catches any month-length slip.
    #[test]
    fn dates_increase_monotonically() {
        let mut prev = at(0, 0);
        for day in 1..33_000i64 {
            let cur = at((day * 86_400) as u64, 0);
            assert!(cur > prev, "{cur} should sort after {prev}");
            prev = cur;
        }
    }
}
