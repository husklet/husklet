use std::process::Command;
use std::time::Instant;

use super::*;

// ─── perf measurement ────────────────────────────────────────────────────────
// The default matrix times each cell as a single wall-clock `run()` (that's the "111ms" in a row).
// For the PERFORMANCE table we want a cleaner number: guest COMPILATION excluded (provisioned once,
// up-front), only the guest EXECUTION timed, `n` times, MEDIAN reported to damp shared-host noise —
// and, for cases that carry an `Oracle` check, the SAME treatment for the native ground-truth run so
// the caller can compute a jit/native slowdown ratio. This is purely additive: the default path
// (`run()`) is untouched, so `make test` stays byte-identical.

/// Median-and-status timing for one case on one engine (see [`run_perf`]).
pub struct Timed {
    /// Authoritative correctness — identical to what the default matrix path (`run`) would report.
    pub status: Status,
    /// Median guest-execution wall time (ms), `None` if the cell was skipped / had no command.
    pub jit_ms: Option<u128>,
    /// Median native-oracle wall time (ms); `Some` only for cases with an `Oracle` check.
    pub oracle_ms: Option<u128>,
    /// Whether this case carries an `Oracle` check (a true jit-vs-native ratio is available).
    pub has_oracle: bool,
}

/// Median of `n` (≥1) timed invocations of `f`.
fn median_ms(n: usize, mut f: impl FnMut()) -> u128 {
    let n = n.max(1);
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        v.push(t.elapsed().as_millis());
    }
    v.sort_unstable();
    v[v.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::median_ms;
    use std::time::Duration;

    // NOTE on the impl: median_ms returns `v[v.len()/2]` after sorting. For an even count that is
    // the UPPER of the two middle samples, NOT their average. `n` is clamped via `n.max(1)`, so
    // n=0 still runs the closure exactly once and never indexes an empty Vec (no panic).

    #[test]
    fn median_ms_runs_closure_n_times() {
        let mut calls = 0usize;
        let _ = median_ms(5, || calls += 1);
        assert_eq!(calls, 5);
    }

    #[test]
    fn median_ms_single_runs_once() {
        let mut calls = 0usize;
        let _ = median_ms(1, || calls += 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn median_ms_zero_clamps_to_one_no_panic() {
        // the `n.max(1)` guard: n=0 must run the closure once and return a value (not panic on v[0]).
        let mut calls = 0usize;
        let m = median_ms(0, || calls += 1);
        assert_eq!(calls, 1);
        let _ = m; // any u128 is acceptable; the point is it returned without panicking.
    }

    #[test]
    fn median_ms_single_element_reports_that_elements_time() {
        // one sample of ~25ms → median is that sample. sleep is a floor, so this lower bound is
        // load-safe (a noisy host only makes it larger).
        let m = median_ms(1, || std::thread::sleep(Duration::from_millis(25)));
        assert!(m >= 20, "expected ~25ms, got {m}");
    }

    #[test]
    fn median_ms_odd_returns_middle_sample() {
        // durations by call index: [0, 30, 60] → sorted ≈ [0, 30, 60], median = v[1] ≈ 30ms.
        // lower bound proves it is NOT the minimum (~0) sample.
        let mut i = 0usize;
        let m = median_ms(3, || {
            let d = [0u64, 30, 60][i.min(2)];
            i += 1;
            std::thread::sleep(Duration::from_millis(d));
        });
        assert!(m >= 20, "expected the middle (~30ms) sample, got {m}");
    }

    #[test]
    fn median_ms_even_returns_upper_of_two_middles_not_average() {
        // two samples by call index: [40, 0] → sorted ≈ [0, 40], v[len/2] = v[1] ≈ 40ms (the UPPER
        // middle). An average would be ~20ms; the ~40ms floor (sleep is a lower bound) proves the
        // impl takes v[len/2], not the mean.
        let mut i = 0usize;
        let m = median_ms(2, || {
            let d = if i == 0 { 40u64 } else { 0 };
            i += 1;
            std::thread::sleep(Duration::from_millis(d));
        });
        assert!(m >= 35, "expected upper-middle (~40ms), not the ~20ms average, got {m}");
    }
}

/// Rebuild the exact JIT launch command for a case+engine (guest already provisioned/compiled), so the
/// perf loop can time `execution` without paying compilation again. Mirrors the setup in [`run`].
/// Returns `(guest_path, (program, args))`, or `None` if the cell has no runnable command (skip).
fn perf_cmd(ctx: &Ctx, c: &Case, e: Engine) -> Option<(String, (String, Vec<String>))> {
    let guest = match provision(ctx, c, e) {
        Ok(Some(g)) => g,
        _ => return None,
    };
    let rootfs = c.rootfs.and_then(|r| ctx.rootfs_path(r, e));
    if c.rootfs.is_some() && rootfs.is_none() {
        return None;
    }
    let rootfs_str = rootfs.unwrap_or_default();
    // Shared with run() so perf times byte-identically to the matrix; only argv diverges (perf uses the
    // plain guest path, run() may use a jailed in-rootfs copy).
    let mut cfg = build_cfg(c, e, &rootfs_str);
    cfg.argv = guest_argv(c, guest.clone());
    let cmd = cfg.command(e.jit())?;
    Some((guest, cmd))
}

/// Run one timed-rerun invocation under the `timeout` hang guard and report whether it SUCCEEDED.
/// Returns `Some(true)` on a clean exit, `Some(false)` on a non-zero exit OR a `timeout` kill (exit 124 —
/// a hang), and `None` if the guard itself couldn't be spawned. The perf timing loop uses this so a rerun
/// that starts failing or HANGING is never timed as a fast, healthy run (the old loop discarded status and
/// had no timeout wrapper, so a regressed/hung invocation could be reported as an acceptable median).
pub fn run_guarded(prog: &str, args: &[String], secs: u32) -> Option<bool> {
    let out = Command::new("timeout")
        .arg(secs.to_string())
        .arg(prog)
        .args(args)
        .output()
        .ok()?;
    Some(out.status.success())
}

/// The native ground-truth command for a guest (mirrors the `Oracle` branch of [`eval`]):
/// aarch64 runs directly, x86_64 under qemu-user. Program is always `timeout` (hang guard).
fn oracle_cmd(guest: &str, args: &[String], e: Engine) -> (String, Vec<String>) {
    let mut a: Vec<String> = vec!["25".into()];
    if e == Engine::LinuxX86_64 {
        a.push("qemu-x86_64".into());
    }
    a.push(guest.into());
    a.extend(args.iter().cloned());
    ("timeout".into(), a)
}

/// Run one case on one engine with perf measurement. Correctness is evaluated exactly as [`run`]
/// (the returned `status` is authoritative and identical to the default matrix), then the guest
/// execution is timed `n` times (median), and — for `Oracle` cases — the native run is timed the same
/// way. Compilation is excluded from the timings (the guest is provisioned before the clock starts).
pub fn run_perf(ctx: &Ctx, c: &Case, e: Engine, n: usize) -> Timed {
    // Authoritative correctness first (byte-identical to the default matrix path).
    let status = run(ctx, c, e);
    if matches!(status, Status::Skip(_)) {
        return Timed {
            status,
            jit_ms: None,
            oracle_ms: None,
            has_oracle: false,
        };
    }
    let (guest, jit) = match perf_cmd(ctx, c, e) {
        Some(x) => x,
        None => {
            return Timed {
                status,
                jit_ms: None,
                oracle_ms: None,
                has_oracle: false,
            }
        }
    };
    // Time the JIT invocation n times, but RECHECK success under the hang guard each rerun: a rerun that
    // fails or hangs must not be reported as a fast, healthy median. A single failed rerun invalidates the
    // timing (jit_ms -> None) so the perf table can't greenlight a regressed/hung lane.
    let mut jit_ok = true;
    let jit_ms = median_ms(n, || {
        if run_guarded(&jit.0, &jit.1, 25) != Some(true) {
            jit_ok = false;
        }
    });
    let jit_ms = jit_ok.then_some(jit_ms);
    let has_oracle = c.checks.iter().any(|k| matches!(k, Check::Oracle));
    let oracle_ms = has_oracle.then(|| {
        let (op, oa) = oracle_cmd(&guest, &c.args, e);
        median_ms(n, || {
            let _ = Command::new(&op).args(&oa).output();
        })
    });
    Timed {
        status,
        jit_ms,
        oracle_ms,
        has_oracle,
    }
}
