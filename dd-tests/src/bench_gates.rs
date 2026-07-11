//! Guard helpers for the microbenchmark harness (`bin/bench`), extracted into the library so the
//! `gate_invariants` test suite can prove each one FAILS LOUDLY instead of silently emitting a
//! misleading benchmark (a zero-sample median, blank dd lanes, or an artifact that never landed).

/// Parse `BENCH_N` (median-of-N repetitions). REJECTS `0` — a zero-repetition run collects no samples,
/// so the "median" is meaningless (previously `BENCH_N=0` was accepted and reached empty-sample median
/// behavior). An absent/`None` value takes the default of 3; a non-numeric value is an error.
pub fn parse_bench_n(raw: Option<String>) -> Result<usize, String> {
    match raw {
        None => Ok(3),
        Some(s) => match s.trim().parse::<usize>() {
            Ok(0) => Err("BENCH_N must be >= 1 (zero repetitions produce no samples)".into()),
            Ok(n) => Ok(n),
            Err(_) => Err(format!("BENCH_N is not a valid repetition count: {s:?}")),
        },
    }
}

/// Verdict on whether the dd bench lanes can actually be produced. The benchmark's entire purpose is a
/// dd-vs-native comparison, so a missing engine that would leave the dd columns BLANK is a HARD failure
/// (a "passing" run with empty dd lanes is a lie) — unless the caller explicitly opts into a native-only
/// run via `BENCH_ALLOW_MISSING_DD`. Previously a missing engine only warned and wrote blank dd columns.
pub fn dd_lanes_verdict(have_arm: bool, have_x86: bool, allow_missing: bool) -> Result<(), String> {
    if (have_arm && have_x86) || allow_missing {
        return Ok(());
    }
    Err(format!(
        "dd engine(s) missing (arm64={have_arm} x86_64={have_x86}); the dd lanes would be blank. \
         Run `make jit`, or set BENCH_ALLOW_MISSING_DD=1 to accept a native-only run."
    ))
}

/// Persist a bench artifact, REPORTING a write failure to the caller instead of swallowing it — a
/// silent `.ok()` meant CI believed it had published results that never actually landed on disk.
pub fn persist_artifact(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    std::fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bench_n_rejects_zero() {
        assert!(parse_bench_n(Some("0".into())).is_err(), "BENCH_N=0 must be rejected");
        assert_eq!(parse_bench_n(Some("5".into())).unwrap(), 5);
        assert_eq!(parse_bench_n(None).unwrap(), 3, "absent -> default 3");
        assert!(parse_bench_n(Some("nope".into())).is_err());
    }

    #[test]
    fn dd_lanes_verdict_fails_when_missing_unless_allowed() {
        assert!(dd_lanes_verdict(true, true, false).is_ok(), "both engines present -> ok");
        assert!(dd_lanes_verdict(false, true, false).is_err(), "a missing engine hard-fails");
        assert!(dd_lanes_verdict(false, false, false).is_err());
        assert!(dd_lanes_verdict(false, false, true).is_ok(), "explicit opt-in allows native-only");
    }

    #[test]
    fn persist_artifact_reports_write_failure() {
        // Writing under a path whose parent is a regular file must ERROR (not be swallowed).
        let base = std::env::temp_dir().join(format!("dd-bench-gate-{}", std::process::id()));
        std::fs::write(&base, b"x").unwrap();
        let bad = base.join("bench.csv"); // parent is a file -> ENOTDIR
        assert!(persist_artifact(&bad, "data").is_err(), "a failed write must be reported");
        // A good path succeeds.
        let good = std::env::temp_dir().join(format!("dd-bench-ok-{}.csv", std::process::id()));
        assert!(persist_artifact(&good, "data").is_ok());
        let _ = std::fs::remove_file(&base);
        let _ = std::fs::remove_file(&good);
    }
}
