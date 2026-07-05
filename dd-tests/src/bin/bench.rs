//! dd microbenchmark — TRUE DBT execution overhead, with process/VM startup EXCLUDED.
//!
//! The trick: the guest (`guests/bench/microbench.c`) times its OWN compute kernel with
//! `clock_gettime(CLOCK_MONOTONIC)` and prints only the kernel time in nanoseconds
//! (`KERNEL <name> <ns>`). Because the number is measured *inside* the guest, everything
//! outside the kernel loop — process spawn, dynamic-loader/static-PIE relocation, the `mac`
//! bridge, engine warm-up, exit — is excluded from BOTH the native run and the dd run. So the
//! reported slowdown is genuine steady-state DBT overhead, not a spawn-tax artifact.
//!
//! Four lanes per kernel, all fed the SAME self-timed guest, one guest per ISA:
//!   native-arm64  the static aarch64 guest run directly on the Linux host — the hardware floor.
//!   dd-arm64      the aarch64 guest through dd's JIT on the macOS host (reuses the exact launch
//!                 path the test harness uses: `ddjit::SpawnConfig::command(Guest::LinuxAarch64)`).
//!   dd-x86        the x86_64 guest through dd's x86→ARM64 JIT (`…command(Guest::LinuxX86_64)`).
//!   qemu-x86      the x86_64 guest under `qemu-x86_64` — the reference x86→ARM DBT baseline.
//!
//! Median-of-N (BENCH_N, default 3; one warm-up discarded per lane so we measure steady state).
//! Output: a table sorted by dd-x86 slowdown desc, plus `target/dd-tests/bench.{csv,json}`.
//!
//!   make bench            # or: cargo run -q -p dd-tests --release --bin bench
//!   BENCH_N=5 make bench  # more repetitions
//!   BENCH_K=alu,fp make bench   # restrict to some kernels
//!
//! This is a SEPARATE target: it does not touch `make test` or the `make perf` table.

use ddjit::{Guest, SpawnConfig};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every kernel the guest implements (must match `microbench.c`'s KERNELS table), in a fixed order.
const KERNELS: &[&str] = &[
    "alu", "muldiv", "branch", "mem", "fp", "simd", "memcpy", "call", "syscall", "sqlsys",
    "epollsys",
];

/// Per-run wall-clock guard (seconds). The kernels are ~0.5–1s native; even a slow dd/qemu lane on
/// the heaviest kernel stays well under this, so `timeout` only ever fires on a genuine hang.
const RUN_TIMEOUT_S: &str = "120";

struct Lane {
    /// column label
    name: &'static str,
    /// median kernel time (ns), one entry per kernel index; `None` = the lane failed for that kernel.
    ns: Vec<Option<f64>>,
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Parse the guest's `KERNEL <name> <ns>` line for `kernel` out of a lane's stdout.
fn parse_ns(stdout: &str, kernel: &str) -> Option<f64> {
    for line in stdout.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some("KERNEL") && it.next() == Some(kernel) {
            if let Some(n) = it.next() {
                return n.parse::<f64>().ok();
            }
        }
    }
    None
}

/// Run `(prog, args)` once under a `timeout` guard; return the guest's self-timed ns for `kernel`.
fn run_once(prog: &str, args: &[String], kernel: &str) -> Option<f64> {
    let mut full = vec![RUN_TIMEOUT_S.to_string(), prog.to_string()];
    full.extend(args.iter().cloned());
    let out = Command::new("timeout").args(&full).output().ok()?;
    let so = String::from_utf8_lossy(&out.stdout);
    let ns = parse_ns(&so, kernel);
    if ns.is_none() {
        eprintln!(
            "[bench]   {kernel}: no output from `{prog}` (code {:?}) — stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    ns
}

/// One warm-up (discarded) + `n` timed runs → median of the guest-reported ns. `None` if any run failed.
fn measure(prog: &str, args: &[String], kernel: &str, n: usize) -> Option<f64> {
    let _ = run_once(prog, args, kernel); // warm-up: prime dd's code cache / host page cache
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        samples.push(run_once(prog, args, kernel)?);
    }
    Some(median(&mut samples))
}

/// The dd-JIT launch command for a bare static-PIE guest, byte-identical to how the test harness
/// launches guests: `SpawnConfig::command(guest)` builds the `mac bash -lc 'exec env <engine> …'`.
fn dd_cmd(guest_bin: &str, guest: Guest, kernel: &str) -> Option<(String, Vec<String>)> {
    let mut cfg = SpawnConfig::new(String::new(), String::new()); // no rootfs → runs un-jailed
    cfg.argv = vec![guest_bin.to_string(), kernel.to_string()];
    cfg.command(guest)
}

fn compile(cc: &str, src: &Path, out: &Path) -> Result<(), String> {
    let o = Command::new(cc)
        .args(["-O2", "-static-pie", "-pthread", "-o"])
        .arg(out)
        .arg(src)
        .output()
        .map_err(|e| format!("{cc}: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "{cc} {}: {}",
            src.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(())
}

/// If `$DDJIT_DIR` is unset, point it at the newest built engine out-dir so `make bench` works
/// straight after `make jit` regardless of the (worktree vs main) build layout. resolve_bundled in
/// the ddjit crate checks `$DDJIT_DIR` first, so this makes the Mach-O engines discoverable.
fn ensure_ddjit_dir(repo: &Path) {
    if std::env::var_os("DDJIT_DIR").is_some() {
        return;
    }
    let build = repo.join("target/release/build");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(&build) {
        for e in rd.flatten() {
            let out = e.path().join("out");
            let eng = out.join("ddjit-linux_aarch64");
            if let Ok(md) = std::fs::metadata(&eng) {
                let t = md.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().map_or(true, |(bt, _)| t > *bt) {
                    best = Some((t, out));
                }
            }
        }
    }
    if let Some((_, dir)) = best {
        eprintln!("[bench] DDJIT_DIR={}", dir.display());
        std::env::set_var("DDJIT_DIR", dir);
    }
}

fn main() {
    let n: usize = std::env::var("BENCH_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let only: Option<Vec<String>> = std::env::var("BENCH_K").ok().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });
    let kernels: Vec<&str> = KERNELS
        .iter()
        .cloned()
        .filter(|k| only.as_ref().map_or(true, |o| o.iter().any(|x| x == k)))
        .collect();

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // <repo>/dd-tests
    let repo = manifest.parent().unwrap().to_path_buf(); // <repo> (under the /Users/x/dd shared tree)
    ensure_ddjit_dir(&repo);

    let src = manifest.join("guests/bench/microbench.c");
    let bench_dir = repo.join("target/bench"); // guest binaries — MUST be in the shared tree so the mac engine sees them
    std::fs::create_dir_all(&bench_dir).ok();
    let g_arm = bench_dir.join("microbench.aarch64");
    let g_x86 = bench_dir.join("microbench.x86_64");

    eprintln!(
        "[bench] compiling guest (aarch64 + x86_64) → {}",
        bench_dir.display()
    );
    if let Err(e) = compile("gcc", &src, &g_arm) {
        eprintln!("[bench] FATAL: {e}");
        std::process::exit(1);
    }
    if let Err(e) = compile("x86_64-linux-gnu-gcc", &src, &g_x86) {
        eprintln!("[bench] FATAL: {e}");
        std::process::exit(1);
    }
    let (g_arm, g_x86) = (
        g_arm.to_string_lossy().into_owned(),
        g_x86.to_string_lossy().into_owned(),
    );

    // Guard: the dd lanes need the built Mach-O engines (run `make jit` first).
    let have_arm = ddjit::available(Guest::LinuxAarch64);
    let have_x86 = ddjit::available(Guest::LinuxX86_64);
    if !have_arm || !have_x86 {
        eprintln!("[bench] WARNING: dd engine(s) not found (arm64={have_arm} x86_64={have_x86}); run `make jit`. dd lanes will be blank.");
    }

    // Collect four lanes of self-timed medians.
    let mut nat = Lane {
        name: "native-arm64",
        ns: vec![],
    };
    let mut dda = Lane {
        name: "dd-arm64",
        ns: vec![],
    };
    let mut ddx = Lane {
        name: "dd-x86",
        ns: vec![],
    };
    let mut qem = Lane {
        name: "qemu-x86",
        ns: vec![],
    };

    eprintln!("[bench] BENCH_N={n}; kernels: {}", kernels.join(", "));
    for k in &kernels {
        eprintln!("[bench] === {k} ===");
        // native arm64: the static aarch64 guest, straight on the Linux host (real hardware floor).
        nat.ns.push(measure(&g_arm, &[k.to_string()], k, n));
        // dd-arm64 / dd-x86: through the engine on the mac bridge.
        dda.ns.push(if have_arm {
            dd_cmd(&g_arm, Guest::LinuxAarch64, k).and_then(|(p, a)| measure(&p, &a, k, n))
        } else {
            None
        });
        ddx.ns.push(if have_x86 {
            dd_cmd(&g_x86, Guest::LinuxX86_64, k).and_then(|(p, a)| measure(&p, &a, k, n))
        } else {
            None
        });
        // qemu-x86: the x86→ARM DBT baseline on the Linux host.
        qem.ns.push(measure(
            "qemu-x86_64",
            &[g_x86.clone(), k.to_string()],
            k,
            n,
        ));
    }

    // ── render: a row per kernel, sorted by dd-x86 slowdown (×native) descending ──
    let ratio = |lane: &Lane, i: usize| -> Option<f64> {
        match (lane.ns[i], nat.ns[i]) {
            (Some(v), Some(b)) if b > 0.0 => Some(v / b),
            _ => None,
        }
    };
    let mut order: Vec<usize> = (0..kernels.len()).collect();
    order.sort_by(|&a, &b| {
        let ka = ratio(&ddx, a).unwrap_or(-1.0);
        let kb = ratio(&ddx, b).unwrap_or(-1.0);
        kb.partial_cmp(&ka).unwrap()
    });

    let ms = |o: Option<f64>| o.map(|v| v / 1e6);
    let fmt_ms = |o: Option<f64>| o.map(|v| format!("{v:.1}")).unwrap_or_else(|| "n/a".into());
    let fmt_x = |o: Option<f64>| {
        o.map(|v| format!("{v:.2}×"))
            .unwrap_or_else(|| "n/a".into())
    };

    println!("\n  dd microbenchmark — startup-EXCLUDED kernel time (self-timed inside the guest)");
    println!("  BENCH_N={n} (median), sorted by dd-x86 slowdown ↓\n");
    println!(
        "  {:<9} {:>14} {:>12} {:>12} {:>12}",
        "Kernel", "native-arm64", "dd-arm64", "dd-x86", "qemu-x86"
    );
    println!(
        "  {:<9} {:>14} {:>12} {:>12} {:>12}",
        "", "(ms)", "(×nat)", "(×nat)", "(×nat)"
    );
    println!("  {}", "─".repeat(63));
    for &i in &order {
        println!(
            "  {:<9} {:>14} {:>12} {:>12} {:>12}",
            kernels[i],
            fmt_ms(ms(nat.ns[i])),
            fmt_x(ratio(&dda, i)),
            fmt_x(ratio(&ddx, i)),
            fmt_x(ratio(&qem, i))
        );
    }
    println!();

    // ── persist: target/dd-tests/bench.{csv,json} ──
    let outdir = repo.join("target/dd-tests");
    std::fs::create_dir_all(&outdir).ok();
    let (csv_path, json_path) = (outdir.join("bench.csv"), outdir.join("bench.json"));

    let mut csv = String::from(
        "kernel,native_arm64_ms,dd_arm64_x,dd_x86_x,qemu_x86_x,dd_arm64_ms,dd_x86_ms,qemu_x86_ms\n",
    );
    let cell_ms = |o: Option<f64>| o.map(|v| format!("{:.3}", v / 1e6)).unwrap_or_default();
    let cell_x = |o: Option<f64>| o.map(|v| format!("{v:.4}")).unwrap_or_default();
    for &i in &order {
        csv += &format!(
            "{},{},{},{},{},{},{},{}\n",
            kernels[i],
            cell_ms(nat.ns[i]),
            cell_x(ratio(&dda, i)),
            cell_x(ratio(&ddx, i)),
            cell_x(ratio(&qem, i)),
            cell_ms(dda.ns[i]),
            cell_ms(ddx.ns[i]),
            cell_ms(qem.ns[i])
        );
    }
    std::fs::write(&csv_path, &csv).ok();

    let jn = |o: Option<f64>| {
        o.map(|v| format!("{v:.6}"))
            .unwrap_or_else(|| "null".into())
    };
    let mut json = String::new();
    json += &format!("{{\n  \"bench_n\": {n},\n  \"note\": \"self-timed kernel ns; startup/VM/spawn excluded\",\n  \"lanes\": [\"{}\",\"{}\",\"{}\",\"{}\"],\n  \"kernels\": [\n",
        nat.name, dda.name, ddx.name, qem.name);
    for (row, &i) in order.iter().enumerate() {
        json += &format!(
            "    {{ \"kernel\": \"{}\", \"native_arm64_ms\": {}, \"dd_arm64_ms\": {}, \"dd_x86_ms\": {}, \"qemu_x86_ms\": {}, \"dd_arm64_x\": {}, \"dd_x86_x\": {}, \"qemu_x86_x\": {} }}{}\n",
            kernels[i],
            jn(ms(nat.ns[i])), jn(ms(dda.ns[i])), jn(ms(ddx.ns[i])), jn(ms(qem.ns[i])),
            jn(ratio(&dda, i)), jn(ratio(&ddx, i)), jn(ratio(&qem, i)),
            if row + 1 == order.len() { "" } else { "," });
    }
    json += "  ]\n}\n";
    std::fs::write(&json_path, &json).ok();

    println!("  wrote {}", csv_path.display());
    println!("  wrote {}\n", json_path.display());
}
