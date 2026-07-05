//! Time helpers: unix-epoch readers and dependency-free RFC3339 formatting.
use super::*;

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Nanoseconds since the unix epoch. Backs the sub-second precision of `State.StartedAt`/`FinishedAt`
/// (docker reports these to the nanosecond) so a quick `docker restart` — which can re-start within the
/// same wall-clock SECOND — still advances `StartedAt`, matching docker (a second-precision stamp would
/// collide and report the restart as a no-op).
pub(crate) fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Format a unix timestamp as an RFC3339 UTC string (Docker's inspect `Created` is a string).
/// Pure integer civil-date math (Howard Hinnant's algorithm) — no chrono dependency.
pub(crate) fn fmt_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// RFC3339 with nanosecond fraction (docker's `State.StartedAt`/`FinishedAt` shape, e.g.
/// `2026-07-02T21:32:17.123456789Z`). `nanos` is a unix-epoch nanosecond count; the whole-second part
/// reuses [`fmt_rfc3339`] and the fraction is appended, so two events in the same second differ.
pub(crate) fn fmt_rfc3339_nanos(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let base = fmt_rfc3339(secs);
    // splice the fraction before the trailing 'Z'
    format!("{}.{:09}Z", &base[..base.len() - 1], frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_rfc3339_epoch_zero() {
        assert_eq!(fmt_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn fmt_rfc3339_known_epoch() {
        // A well-known unix timestamp round-trips to the documented RFC3339 string.
        assert_eq!(fmt_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn fmt_rfc3339_leap_day() {
        // 1_709_164_800 == 2024-02-29T00:00:00Z (2024 is a leap year, Feb has 29 days).
        assert_eq!(fmt_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        // Same leap day with a non-zero time-of-day exercises the hh/mm/ss split too.
        assert_eq!(fmt_rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn fmt_rfc3339_negative_pre_epoch() {
        // Negative seconds must floor via div_euclid (not truncate toward zero): one second
        // before the epoch is the last second of 1969.
        assert_eq!(fmt_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn fmt_rfc3339_nanos_zero_pads_fraction_to_nine() {
        // 1_700_000_000 s + 123 ns -> fraction is left-zero-padded to 9 digits before the 'Z'.
        let nanos = 1_700_000_000i64 * 1_000_000_000 + 123;
        assert_eq!(fmt_rfc3339_nanos(nanos), "2023-11-14T22:13:20.000000123Z");
    }

    #[test]
    fn fmt_rfc3339_nanos_distinguishes_sub_second_events() {
        // Two events in the SAME whole second must produce different strings (the reason docker
        // reports StartedAt/FinishedAt to the nanosecond).
        let base = 1_700_000_000i64 * 1_000_000_000;
        let a = fmt_rfc3339_nanos(base + 5);
        let b = fmt_rfc3339_nanos(base + 500);
        assert_eq!(a, "2023-11-14T22:13:20.000000005Z");
        assert_eq!(b, "2023-11-14T22:13:20.000000500Z");
        assert_ne!(a, b);
    }
}
