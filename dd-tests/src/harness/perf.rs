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
    let jit_ms = median_ms(n, || {
        let _ = Command::new(&jit.0).args(&jit.1).output();
    });
    let has_oracle = c.checks.iter().any(|k| matches!(k, Check::Oracle));
    let oracle_ms = has_oracle.then(|| {
        let (op, oa) = oracle_cmd(&guest, &c.args, e);
        median_ms(n, || {
            let _ = Command::new(&op).args(&oa).output();
        })
    });
    Timed {
        status,
        jit_ms: Some(jit_ms),
        oracle_ms,
        has_oracle,
    }
}
