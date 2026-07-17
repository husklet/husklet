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

/// Whole seconds AND nanoseconds since the unix epoch, read from a SINGLE clock sample so the two
/// components are coherent (unlike calling `now_secs()` + `now_nanos()`, which sample twice and can
/// straddle a second boundary). A pre-epoch clock (`duration_since` errors) yields `(0, 0)`, matching
/// the event bus's historical inline behavior. Used by `events::emit_event` for an event's
/// `time`/`timeNano` pair.
pub(crate) fn now_epoch_parts() -> (i64, i64) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.as_nanos() as i64),
        Err(_) => (0, 0),
    }
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

/// Days since the unix epoch for a civil (proleptic Gregorian) date — the inverse of the date math in
/// [`fmt_rfc3339`] (Howard Hinnant's `days_from_civil`). Dependency-free.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse a docker `--since`/`--until` / logs time filter into unix SECONDS. Docker accepts several forms;
/// hl previously honored only integer seconds, so RFC3339 strings and Go durations disabled the filter
/// (logs) or turned a bounded event query into an unbounded stream. Handles:
///   - Unix `seconds[.nanos]` (`"1700000000"`, `"1700000000.5"`) — the integer seconds.
///   - RFC3339 / RFC3339Nano (`"2017-01-05T00:36:05Z"`, `"...05.123456789Z"`, with `±HH:MM` offset).
///   - Go duration relative to now (`"10m"`, `"1h30m"`, `"90s"`) — interpreted as `now - duration`.
/// `now` is the reference epoch for duration forms (usually [`now_secs`]). Returns `None` only when the
/// string matches none of the forms.
pub(crate) fn parse_docker_ts(s: &str, now: i64) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Unix seconds[.nanos]: all-digit integer part (optionally signed), optional `.frac`.
    let intpart = s.split('.').next().unwrap_or(s);
    if intpart
        .strip_prefix('-')
        .unwrap_or(intpart)
        .chars()
        .all(|c| c.is_ascii_digit())
        && !intpart.is_empty()
        && intpart.chars().any(|c| c.is_ascii_digit())
    {
        // Distinguish a bare integer (unix secs) from a duration like "90" (invalid) — a pure integer with
        // no unit letters is unix seconds.
        if s.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        {
            return intpart.parse::<i64>().ok();
        }
    }
    // RFC3339: begins with `YYYY-MM-HLT`.
    if s.len() >= 20 && s.as_bytes().get(4) == Some(&b'-') && (s.contains('T') || s.contains('t')) {
        return parse_rfc3339(s);
    }
    // Go duration relative to now.
    parse_go_duration(s).map(|d| now - d)
}

/// Parse an RFC3339/RFC3339Nano timestamp into unix seconds (fraction truncated). Handles `Z` and a
/// numeric `±HH:MM` offset. Dependency-free.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse().ok() };
    if bytes.len() < 19 {
        return None;
    }
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let hh = num(11, 13)?;
    let mm = num(14, 16)?;
    let ss = num(17, 19)?;
    let mut secs = days_from_civil(y, mo, d) * 86400 + hh * 3600 + mm * 60 + ss;
    // Timezone: find the offset marker after the seconds/fraction. Scan from position 19 for Z/+/-.
    let rest = &s[19..];
    let tz_start = rest.find(['Z', 'z', '+']).or_else(|| {
        // A '-' offset appears after the fraction; skip a leading fractional part before looking.
        rest.char_indices()
            .find(|&(i, c)| c == '-' && i > 0)
            .map(|(i, _)| i)
    });
    if let Some(off) = tz_start {
        let tz = &rest[off..];
        if !(tz.starts_with('Z') || tz.starts_with('z')) && tz.len() >= 6 {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let oh: i64 = tz.get(1..3)?.parse().ok()?;
            let om: i64 = tz.get(4..6)?.parse().ok()?;
            secs -= sign * (oh * 3600 + om * 60); // convert local -> UTC
        }
    }
    Some(secs)
}

/// Parse a Go-style duration (`"10m"`, `"1h30m"`, `"90s"`, `"1h"`) into whole seconds. Supports h/m/s
/// units (the ones docker time filters use); returns `None` for unrecognized input.
fn parse_go_duration(s: &str) -> Option<i64> {
    let mut total = 0i64;
    let mut num = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'h' => n * 3600,
                'm' => n * 60,
                's' => n,
                _ => return None,
            };
            saw_unit = true;
        }
    }
    if !num.is_empty() || !saw_unit {
        return None; // trailing digits without a unit, or no unit at all
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_docker_ts_unix_seconds() {
        assert_eq!(parse_docker_ts("1700000000", 0), Some(1_700_000_000));
        assert_eq!(parse_docker_ts("1700000000.5", 0), Some(1_700_000_000));
    }

    #[test]
    fn parse_docker_ts_rfc3339() {
        assert_eq!(
            parse_docker_ts("2023-11-14T22:13:20Z", 0),
            Some(1_700_000_000)
        );
        // RFC3339Nano fraction is truncated to whole seconds.
        assert_eq!(
            parse_docker_ts("2023-11-14T22:13:20.123456789Z", 0),
            Some(1_700_000_000)
        );
        // A '+02:00' offset converts local -> UTC (subtract two hours).
        assert_eq!(
            parse_docker_ts("2023-11-15T00:13:20+02:00", 0),
            Some(1_700_000_000)
        );
        assert_eq!(
            parse_docker_ts("2017-01-05T00:36:05Z", 0),
            Some(1_483_576_565)
        );
    }

    #[test]
    fn parse_docker_ts_go_duration_relative_to_now() {
        assert_eq!(parse_docker_ts("10m", 1000), Some(1000 - 600));
        assert_eq!(parse_docker_ts("1h30m", 10_000), Some(10_000 - 5400));
        assert_eq!(parse_docker_ts("90s", 1000), Some(1000 - 90));
    }

    #[test]
    fn parse_docker_ts_rejects_garbage() {
        assert_eq!(parse_docker_ts("not-a-time", 0), None);
        assert_eq!(parse_docker_ts("", 0), None);
    }

    #[test]
    fn now_epoch_parts_is_coherent_and_positive() {
        // Live clock is well past the epoch, so both components are positive and the nanos value is
        // consistent with the seconds value read from the SAME sample: nanos/1e9 == secs (the two
        // fields never straddle a second boundary, which is the whole point of the single-sample read).
        let (secs, nanos) = now_epoch_parts();
        assert!(
            secs > 1_700_000_000,
            "secs {secs} should be a recent epoch time"
        );
        assert!(nanos > 0);
        assert_eq!(
            nanos / 1_000_000_000,
            secs,
            "nanos and secs come from one sample"
        );
    }

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
