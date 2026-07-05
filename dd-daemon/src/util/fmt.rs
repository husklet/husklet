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
