//! #339 persistent translated-code cache — LIFECYCLE lane (aarch64 engine).
//!
//! The matrix cases (`cases/ext/pcachex.rs`) prove single-run correctness under DDJIT_PCACHE=1; this lane
//! drives the multi-invocation protocol end to end against a private cache dir:
//!   1. cold run  -> saves a cache file; warm run -> HIT, byte-identical stdout/exit.
//!   2. EIGHT CONCURRENT processes on the same key (the crash that blocked the first #339 attempt: a fork
//!      child's exit-time save persisted the parent's stale reloc table, and the next load stomped
//!      movz/movk rewrites over live code) — all must succeed with identical output.
//!   3. fork child OUTLIVES the parent (the poisoned-save repro shape) -> the file must stay clean and
//!      later runs must still HIT and succeed.
//!   4. stale guest binary (recompiled -> new inode/mtime) -> the old entry must NOT be used (MISS).
//!   5. kill-switch (DDJIT_NOPCACHE=1) -> the cache is fully inert.
//!   6. corrupt / truncated cache file -> checksum/validation MISS, correct output, self-healing re-save.
//!
//! Engine invocations mirror the matrix harness (ddjit::SpawnConfig -> `mac bash -lc` off-macOS), with
//! per-run env baked into the launch script (the mac bridge drops ambient env). COLDPROF=1 lets the
//! assertions distinguish HIT / MISS / save on stderr without affecting guest-visible behaviour.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Compile a pcachex guest as linux/aarch64 static-PIE into the repo-visible target dir.
fn compile_guest(name: &str, out_name: &str) -> PathBuf {
    let src = repo().join("dd-tests/guests/pcachex").join(name);
    let outdir = repo().join("target/dd-tests/pcache");
    std::fs::create_dir_all(&outdir).unwrap();
    let out = outdir.join(out_name);
    let o = Command::new("gcc")
        .args(["-O2", "-static-pie", "-o"])
        .arg(&out).arg(&src)
        .output()
        .unwrap_or_else(|e| panic!("gcc spawn: {e}"));
    assert!(o.status.success(), "compile {name}: {}", String::from_utf8_lossy(&o.stderr));
    out
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// One engine run of `guest` with the given extra env (DDJIT_PCACHE etc.), hang-guarded by `timeout`.
fn run_engine(guest: &PathBuf, env: &[(&str, &str)]) -> Run {
    let mut cfg = ddjit::SpawnConfig::new(String::new(), String::new());
    for (k, v) in env {
        cfg.env.push(((*k).into(), (*v).into()));
    }
    cfg.argv = vec![guest.to_string_lossy().into_owned()];
    let (prog, args) = cfg.command(ddjit::Guest::LinuxAarch64).expect("engine command");
    let out = Command::new("timeout").arg("30").arg(&prog).args(&args).output().expect("spawn engine");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// Same, but N processes launched concurrently (the same-key race lane).
fn run_engine_concurrent(guest: &PathBuf, env: &[(&str, &str)], n: usize) -> Vec<Run> {
    let mut cfg = ddjit::SpawnConfig::new(String::new(), String::new());
    for (k, v) in env {
        cfg.env.push(((*k).into(), (*v).into()));
    }
    cfg.argv = vec![guest.to_string_lossy().into_owned()];
    let (prog, args) = cfg.command(ddjit::Guest::LinuxAarch64).expect("engine command");
    let children: Vec<_> = (0..n)
        .map(|_| {
            Command::new("timeout").arg("40").arg(&prog).args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn engine")
        })
        .collect();
    children
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().expect("wait engine");
            Run {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                code: out.status.code().unwrap_or(-1),
            }
        })
        .collect()
}

fn cache_files(dir: &PathBuf) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| {
        rd.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "pcache").unwrap_or(false))
            .collect()
    }).unwrap_or_default()
}

#[test]
fn pcache_lifecycle_aarch64() {
    if !ddjit::available(ddjit::Guest::LinuxAarch64) {
        eprintln!("linux/aarch64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    let dir = repo().join("target/dd-tests/pcache/dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = dir.to_string_lossy().into_owned();
    let base: Vec<(&str, &str)> = vec![("DDJIT_PCACHE", "1"), ("DDJIT_PCACHE_DIR", &dirs), ("COLDPROF", "1")];

    // ---- 1. cold -> warm, identical output, file created, HIT observed ----
    let hello = compile_guest("selfexec.c", "selfexec");
    let cold = run_engine(&hello, &base);
    assert_eq!(cold.code, 0, "cold run failed: {}\n{}", cold.stdout, cold.stderr);
    assert_eq!(cold.stdout, "pcache selfexec reaped=6 sum=75 bad=0\n", "cold stdout");
    assert!(cold.stderr.contains("[pcache] MISS"), "cold run must MISS: {}", cold.stderr);
    assert!(cold.stderr.contains("save ok"), "cold run must save: {}", cold.stderr);
    assert!(!cache_files(&dir).is_empty(), "cold run must create a cache file");
    let warm = run_engine(&hello, &base);
    assert_eq!(warm.code, 0, "warm run failed: {}\n{}", warm.stdout, warm.stderr);
    assert_eq!(warm.stdout, cold.stdout, "warm stdout must be byte-identical to cold");
    assert!(warm.stderr.contains("[pcache] HIT"), "warm run must HIT: {}", warm.stderr);

    // ---- 2. eight CONCURRENT processes on the same key (the prior-attempt blocker) ----
    for round in 0..2 {
        for r in run_engine_concurrent(&hello, &base, 8) {
            assert_eq!(r.code, 0, "concurrent round {round} rc={} stderr: {}", r.code, r.stderr);
            assert_eq!(r.stdout, cold.stdout, "concurrent round {round}: output must stay deterministic");
        }
    }

    // ---- 3. fork child outlives the parent (poisoned-save repro): file stays clean ----
    let outlive = compile_guest("forkoutlive.c", "forkoutlive");
    let oc = run_engine(&outlive, &base);
    assert_eq!(oc.code, 0, "forkoutlive cold: {}", oc.stderr);
    assert_eq!(oc.stdout, "pcache forkoutlive forked=1\n");
    std::thread::sleep(std::time::Duration::from_millis(1200)); // let the orphaned child exit (its save must be REFUSED)
    for i in 0..3 {
        let ow = run_engine(&outlive, &base);
        assert_eq!(ow.code, 0, "forkoutlive warm {i} (poisoned file?): {}", ow.stderr);
        assert_eq!(ow.stdout, "pcache forkoutlive forked=1\n", "forkoutlive warm {i}");
        assert!(ow.stderr.contains("[pcache] HIT"), "forkoutlive warm {i} must HIT: {}", ow.stderr);
    }

    // ---- 4. stale binary: a fresh copy (new inode/mtime) must MISS, never load the old entry ----
    let stale = repo().join("target/dd-tests/pcache/selfexec-stale");
    std::fs::copy(&hello, &stale).unwrap();
    let s1 = run_engine(&stale, &base);
    assert!(s1.stderr.contains("[pcache] MISS"), "fresh copy must MISS (identity re-keyed): {}", s1.stderr);
    assert_eq!(s1.stdout, cold.stdout);
    let s2 = run_engine(&stale, &base);
    assert!(s2.stderr.contains("[pcache] HIT"), "copy's second run must HIT its own entry: {}", s2.stderr);
    // now REPLACE the binary in place (same path, new content identity) -> its old entry must not be used
    std::fs::remove_file(&stale).unwrap();
    std::fs::copy(&hello, &stale).unwrap();
    let s3 = run_engine(&stale, &base);
    assert!(s3.stderr.contains("[pcache] MISS"), "replaced binary must MISS: {}", s3.stderr);
    assert_eq!(s3.stdout, cold.stdout);

    // ---- 4b. CROSS-LAYOUT loads must MISS (v0.9.20 integration bug): an arena saved under the default
    // IRQSLIM layout must never load into a NOIRQSLIM engine (nor vice versa) — the block-entry layout
    // differs (2-insn poll header, forward chains at body+8), so a cross-mode load would enter
    // mid-instruction. Both the mode-hashed cache id AND PC_VERSION_EFF (live g_fwdskip mixed into the
    // header version, mirroring x86) guarantee the MISS. Same contract for NOIBSLIM (hash_tail shape).
    for flip in ["NOIRQSLIM", "NOIBSLIM"] {
        let mut menv = base.clone();
        menv.push((flip, "1"));
        let m1 = run_engine(&hello, &menv);
        assert_eq!(m1.code, 0, "{flip} run failed: {}", m1.stderr);
        assert_eq!(m1.stdout, cold.stdout, "{flip} output must match");
        assert!(m1.stderr.contains("[pcache] MISS"), "{flip}=1 must not load a default-layout save: {}", m1.stderr);
        let m2 = run_engine(&hello, &menv);
        assert!(m2.stderr.contains("[pcache] HIT"), "{flip}=1 second run must HIT its own entry: {}", m2.stderr);
        assert_eq!(m2.stdout, cold.stdout);
    }
    // ...and the default layout still HITs its own entry afterwards (no cross-mode clobber).
    let back = run_engine(&hello, &base);
    assert!(back.stderr.contains("[pcache] HIT"), "default layout must still HIT: {}", back.stderr);
    assert_eq!(back.stdout, cold.stdout);

    // ---- 5. kill-switch: DDJIT_NOPCACHE=1 wins over DDJIT_PCACHE=1 (cache fully inert) ----
    let mut kenv = base.clone();
    kenv.push(("DDJIT_NOPCACHE", "1"));
    let k = run_engine(&hello, &kenv);
    assert_eq!(k.code, 0);
    assert_eq!(k.stdout, cold.stdout);
    assert!(!k.stderr.contains("[pcache]"), "kill-switch run must not touch the cache: {}", k.stderr);

    // ---- 6. corrupt + truncated cache files: graceful MISS, correct output, self-healed by re-save ----
    for f in cache_files(&dir) {
        use std::io::{Seek, SeekFrom, Write};
        let mut fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        let mid = fh.metadata().unwrap().len() / 2;
        fh.seek(SeekFrom::Start(mid)).unwrap();
        fh.write_all(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef]).unwrap();
    }
    let c1 = run_engine(&hello, &base);
    assert_eq!(c1.code, 0, "corrupt-file run must still succeed: {}", c1.stderr);
    assert_eq!(c1.stdout, cold.stdout);
    assert!(c1.stderr.contains("[pcache] MISS"), "corrupt file must MISS (checksum): {}", c1.stderr);
    for f in cache_files(&dir) {
        let len = std::fs::metadata(&f).unwrap().len();
        let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.set_len(len / 3).unwrap(); // truncate
    }
    let c2 = run_engine(&hello, &base);
    assert_eq!(c2.code, 0, "truncated-file run must still succeed: {}", c2.stderr);
    assert_eq!(c2.stdout, cold.stdout);
    let c3 = run_engine(&hello, &base);
    assert!(c3.stderr.contains("[pcache] HIT"), "self-healed file must HIT again: {}", c3.stderr);
}
