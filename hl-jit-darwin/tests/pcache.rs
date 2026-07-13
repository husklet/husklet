//! persistent translated-code cache — LIFECYCLE lane (aarch64 engine).
//!
//! The matrix cases (`cases/ext/pcachex.rs`) prove single-run correctness under DDJIT_PCACHE=1; this lane
//! drives the multi-invocation protocol end to end against a private cache dir:
//!   1. cold run  -> saves a cache file; warm run -> HIT, byte-identical stdout/exit.
//!   2. EIGHT CONCURRENT processes on the same key (the crash that blocked the first attempt: a fork
//!      child's exit-time save persisted the parent's stale reloc table, and the next load stomped
//!      movz/movk rewrites over live code) — all must succeed with identical output.
//!   3. fork child OUTLIVES the parent (the poisoned-save repro shape) -> the file must stay clean and
//!      later runs must still HIT and succeed.
//!   4. stale guest binary (recompiled -> new inode/mtime) -> the old entry must NOT be used (MISS).
//!   5. kill-switch (DDJIT_NOPCACHE=1) -> the cache is fully inert.
//!   6. corrupt / truncated cache file -> checksum/validation MISS, correct output, self-healing re-save.
//!
//! Engine invocations mirror the matrix harness (hl_jit::SpawnConfig -> `mac bash -lc` off-macOS), with
//! per-run env baked into the launch script (the mac bridge drops ambient env). COLDPROF=1 lets the
//! assertions distinguish HIT / MISS / save on stderr without affecting guest-visible behaviour.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Compile a pcachex guest as linux/aarch64 static-PIE into the repo-visible target dir.
fn compile_guest(name: &str, out_name: &str) -> PathBuf {
    let src = repo().join("hl-jit-darwin/testdata/guests/pcachex").join(name);
    let outdir = repo().join("target/dd-tests/pcache");
    std::fs::create_dir_all(&outdir).unwrap();
    let out = outdir.join(out_name);
    let o = Command::new("gcc")
        .args(["-O2", "-static-pie", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap_or_else(|e| panic!("gcc spawn: {e}"));
    assert!(
        o.status.success(),
        "compile {name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// One engine run of `guest` with the given extra env (DDJIT_PCACHE etc.), hang-guarded by `timeout`.
fn run_engine(guest: &PathBuf, env: &[(&str, &str)]) -> Run {
    let mut cfg = hl_jit::SpawnConfig::new(String::new(), String::new());
    for (k, v) in env {
        cfg.env.push(((*k).into(), (*v).into()));
    }
    cfg.argv = vec![guest.to_string_lossy().into_owned()];
    let (prog, args) = cfg
        .command(hl_jit::Guest::LinuxAarch64)
        .expect("engine command");
    let out = Command::new("timeout")
        .arg("30")
        .arg(&prog)
        .args(&args)
        .output()
        .expect("spawn engine");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// Same, but N processes launched concurrently (the same-key race lane).
fn run_engine_concurrent(guest: &PathBuf, env: &[(&str, &str)], n: usize) -> Vec<Run> {
    let mut cfg = hl_jit::SpawnConfig::new(String::new(), String::new());
    for (k, v) in env {
        cfg.env.push(((*k).into(), (*v).into()));
    }
    cfg.argv = vec![guest.to_string_lossy().into_owned()];
    let (prog, args) = cfg
        .command(hl_jit::Guest::LinuxAarch64)
        .expect("engine command");
    let children: Vec<_> = (0..n)
        .map(|_| {
            Command::new("timeout")
                .arg("40")
                .arg(&prog)
                .args(&args)
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
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "pcache").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn pcache_lifecycle_aarch64() {
    if !hl_jit::available(hl_jit::Guest::LinuxAarch64) {
        eprintln!("linux/aarch64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    let dir = repo().join("target/dd-tests/pcache/dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = dir.to_string_lossy().into_owned();
    let base: Vec<(&str, &str)> = vec![
        ("DDJIT_PCACHE", "1"),
        ("DDJIT_PCACHE_DIR", &dirs),
        ("COLDPROF", "1"),
    ];

    // ---- 1. cold -> warm, identical output, file created, HIT observed ----
    let hello = compile_guest("selfexec.c", "selfexec");
    let cold = run_engine(&hello, &base);
    assert_eq!(
        cold.code, 0,
        "cold run failed: {}\n{}",
        cold.stdout, cold.stderr
    );
    assert_eq!(
        cold.stdout, "pcache selfexec reaped=6 sum=75 bad=0\n",
        "cold stdout"
    );
    assert!(
        cold.stderr.contains("[pcache] MISS"),
        "cold run must MISS: {}",
        cold.stderr
    );
    assert!(
        cold.stderr.contains("save ok"),
        "cold run must save: {}",
        cold.stderr
    );
    assert!(
        !cache_files(&dir).is_empty(),
        "cold run must create a cache file"
    );
    let warm = run_engine(&hello, &base);
    assert_eq!(
        warm.code, 0,
        "warm run failed: {}\n{}",
        warm.stdout, warm.stderr
    );
    assert_eq!(
        warm.stdout, cold.stdout,
        "warm stdout must be byte-identical to cold"
    );
    assert!(
        warm.stderr.contains("[pcache] HIT"),
        "warm run must HIT: {}",
        warm.stderr
    );

    // ---- 2. eight CONCURRENT processes on the same key (the prior-attempt blocker) ----
    for round in 0..2 {
        for r in run_engine_concurrent(&hello, &base, 8) {
            assert_eq!(
                r.code, 0,
                "concurrent round {round} rc={} stderr: {}",
                r.code, r.stderr
            );
            assert_eq!(
                r.stdout, cold.stdout,
                "concurrent round {round}: output must stay deterministic"
            );
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
        assert_eq!(
            ow.code, 0,
            "forkoutlive warm {i} (poisoned file?): {}",
            ow.stderr
        );
        assert_eq!(
            ow.stdout, "pcache forkoutlive forked=1\n",
            "forkoutlive warm {i}"
        );
        assert!(
            ow.stderr.contains("[pcache] HIT"),
            "forkoutlive warm {i} must HIT: {}",
            ow.stderr
        );
    }

    // ---- 4. stale binary: a fresh copy (new inode/mtime) must MISS, never load the old entry ----
    let stale = repo().join("target/dd-tests/pcache/selfexec-stale");
    std::fs::copy(&hello, &stale).unwrap();
    let s1 = run_engine(&stale, &base);
    assert!(
        s1.stderr.contains("[pcache] MISS"),
        "fresh copy must MISS (identity re-keyed): {}",
        s1.stderr
    );
    assert_eq!(s1.stdout, cold.stdout);
    let s2 = run_engine(&stale, &base);
    assert!(
        s2.stderr.contains("[pcache] HIT"),
        "copy's second run must HIT its own entry: {}",
        s2.stderr
    );
    // now REPLACE the binary in place (same path, new content identity) -> its old entry must not be used
    std::fs::remove_file(&stale).unwrap();
    std::fs::copy(&hello, &stale).unwrap();
    let s3 = run_engine(&stale, &base);
    assert!(
        s3.stderr.contains("[pcache] MISS"),
        "replaced binary must MISS: {}",
        s3.stderr
    );
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
        assert!(
            m1.stderr.contains("[pcache] MISS"),
            "{flip}=1 must not load a default-layout save: {}",
            m1.stderr
        );
        let m2 = run_engine(&hello, &menv);
        assert!(
            m2.stderr.contains("[pcache] HIT"),
            "{flip}=1 second run must HIT its own entry: {}",
            m2.stderr
        );
        assert_eq!(m2.stdout, cold.stdout);
    }
    // ...and the default layout still HITs its own entry afterwards (no cross-mode clobber).
    let back = run_engine(&hello, &base);
    assert!(
        back.stderr.contains("[pcache] HIT"),
        "default layout must still HIT: {}",
        back.stderr
    );
    assert_eq!(back.stdout, cold.stdout);

    // ---- 5. kill-switch: DDJIT_NOPCACHE=1 wins over DDJIT_PCACHE=1 (cache fully inert) ----
    let mut kenv = base.clone();
    kenv.push(("DDJIT_NOPCACHE", "1"));
    let k = run_engine(&hello, &kenv);
    assert_eq!(k.code, 0);
    assert_eq!(k.stdout, cold.stdout);
    assert!(
        !k.stderr.contains("[pcache]"),
        "kill-switch run must not touch the cache: {}",
        k.stderr
    );

    // ---- 6. corrupt + truncated cache files: graceful MISS, correct output, self-healed by re-save ----
    for f in cache_files(&dir) {
        use std::io::{Seek, SeekFrom, Write};
        let mut fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        let mid = fh.metadata().unwrap().len() / 2;
        fh.seek(SeekFrom::Start(mid)).unwrap();
        fh.write_all(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef])
            .unwrap();
    }
    let c1 = run_engine(&hello, &base);
    assert_eq!(
        c1.code, 0,
        "corrupt-file run must still succeed: {}",
        c1.stderr
    );
    assert_eq!(c1.stdout, cold.stdout);
    assert!(
        c1.stderr.contains("[pcache] MISS"),
        "corrupt file must MISS (checksum): {}",
        c1.stderr
    );
    for f in cache_files(&dir) {
        let len = std::fs::metadata(&f).unwrap().len();
        let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.set_len(len / 3).unwrap(); // truncate
    }
    let c2 = run_engine(&hello, &base);
    assert_eq!(
        c2.code, 0,
        "truncated-file run must still succeed: {}",
        c2.stderr
    );
    assert_eq!(c2.stdout, cold.stdout);
    let c3 = run_engine(&hello, &base);
    assert!(
        c3.stderr.contains("[pcache] HIT"),
        "self-healed file must HIT again: {}",
        c3.stderr
    );
}

/// exec re-key on the aarch64 engine: the two-file protocol (a save AT the execve boundary keyed to
/// the OUTGOING image, a re-key + reload for the new image), byte-identical warm output, no warm-run churn,
/// and self-heal. (The warm-stat sidecar + dead-weight SKIP + deferred-library restore land on the
/// aarch64 engine via the separate arm-pcache warm-fix; this lane covers what the shared exec-rekey adds.)
#[test]
fn pcache_policy_aarch64() {
    if !hl_jit::available(hl_jit::Guest::LinuxAarch64) {
        eprintln!("linux/aarch64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    let dir = repo().join("target/dd-tests/pcache/dir-policy");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = dir.to_string_lossy().into_owned();
    let base: Vec<(&str, &str)> = vec![
        ("DDJIT_PCACHE", "1"),
        ("DDJIT_PCACHE_DIR", &dirs),
        ("COLDPROF", "1"),
    ];

    // ---- exec re-key: driver epoch saved AT THE EXEC under its own key, applet epoch under its own
    // -> exactly TWO cache files; the warm run HITs both epochs (initial HIT + exec HIT) and re-saves neither.
    let chain = compile_guest("execchain.c", "execchain");
    let cold = run_engine(&chain, &base);
    assert_eq!(
        cold.code, 0,
        "execchain cold failed: {}\n{}",
        cold.stdout, cold.stderr
    );
    assert_eq!(
        cold.stdout, "pcache execchain applet ok argc=1\n",
        "cold stdout"
    );
    assert!(
        cold.stderr.contains("[pcache] MISS"),
        "driver epoch must MISS cold: {}",
        cold.stderr
    );
    assert!(
        cold.stderr.contains("[pcache] exec MISS"),
        "applet epoch must MISS cold: {}",
        cold.stderr
    );
    assert_eq!(
        cold.stderr.matches("save ok").count(),
        2,
        "cold run must save BOTH epochs (driver at exec, applet at exit): {}",
        cold.stderr
    );
    let files = cache_files(&dir);
    assert_eq!(
        files.len(),
        2,
        "exec re-key must produce two distinct cache files: {files:?}"
    );
    let warm = run_engine(&chain, &base);
    assert_eq!(warm.code, 0, "execchain warm failed: {}", warm.stderr);
    assert_eq!(
        warm.stdout, cold.stdout,
        "warm stdout must be byte-identical"
    );
    assert!(
        warm.stderr.contains("[pcache] HIT"),
        "driver epoch must HIT warm: {}",
        warm.stderr
    );
    assert!(
        warm.stderr.contains("[pcache] exec HIT"),
        "applet epoch must HIT warm: {}",
        warm.stderr
    );
    assert!(
        !warm.stderr.contains("save ok"),
        "a warm (restored) epoch must never re-save: {}",
        warm.stderr
    );
    // no churn: warm runs must not rewrite either published cache file
    let mtimes = |fs: &Vec<PathBuf>| -> Vec<std::time::SystemTime> {
        fs.iter()
            .map(|f| std::fs::metadata(f).unwrap().modified().unwrap())
            .collect()
    };
    let m1 = mtimes(&files);
    let warm2 = run_engine(&chain, &base);
    assert_eq!(warm2.stdout, cold.stdout);
    assert_eq!(
        m1,
        mtimes(&files),
        "warm runs must never rewrite the published cache files"
    );

    // ---- self-heal: remove both files -> both epochs MISS + re-save -> HIT again ----
    for f in cache_files(&dir) {
        std::fs::remove_file(f).unwrap();
    }
    let re = run_engine(&chain, &base);
    assert!(
        re.stderr.contains("[pcache] MISS"),
        "removed files must MISS: {}",
        re.stderr
    );
    assert!(
        re.stderr.contains("save ok"),
        "cold run must re-save: {}",
        re.stderr
    );
    let re2 = run_engine(&chain, &base);
    assert!(
        re2.stderr.contains("[pcache] HIT"),
        "fresh save must HIT again: {}",
        re2.stderr
    );
    assert_eq!(re2.stdout, cold.stdout);
}

// ---- x86 lane helpers (cross-compiled guest + x86 engine) ----
fn compile_guest_x86(name: &str, out_name: &str) -> PathBuf {
    let src = repo().join("hl-jit-darwin/testdata/guests/pcachex").join(name);
    let outdir = repo().join("target/dd-tests/pcache");
    std::fs::create_dir_all(&outdir).unwrap();
    let out = outdir.join(out_name);
    let o = Command::new("x86_64-linux-gnu-gcc")
        .args(["-O2", "-static-pie", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap_or_else(|e| panic!("x86_64-linux-gnu-gcc spawn: {e}"));
    assert!(
        o.status.success(),
        "compile x86 {name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}
fn run_engine_x86(guest: &PathBuf, guest_args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cfg = hl_jit::SpawnConfig::new(String::new(), String::new());
    for (k, v) in env {
        cfg.env.push(((*k).into(), (*v).into()));
    }
    cfg.argv = vec![guest.to_string_lossy().into_owned()];
    for a in guest_args {
        cfg.argv.push((*a).to_string());
    }
    let (prog, args) = cfg
        .command(hl_jit::Guest::LinuxX86_64)
        .expect("engine command");
    let out = Command::new("timeout")
        .arg("30")
        .arg(&prog)
        .args(&args)
        .output()
        .expect("spawn engine");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// FULL policy lane on the x86 engine (the mission engine, which owns exec re-key + selective
/// restore + the warm-stat sidecar + deferred-library activation):
///   1. exec re-key two-file protocol (execchain).
///   2. selective restore: a file-backed executable (library-like) map is DEFERRED on load and
///      activated only when the same file identity re-maps at the same base -> waste=0 (libmap).
///   3. dead-weight SKIP: a sidecar reporting waste==restored makes the next load skip the restore
///      (header-only, correct output, no resave churn), sticky until a fresh cold save clears it.
#[test]
fn pcache_policy_x86_64() {
    if !hl_jit::available(hl_jit::Guest::LinuxX86_64) {
        eprintln!("linux/x86_64 engine not built — skipping (pin DDJIT_DIR to a built engine)");
        return;
    }
    let dir = repo().join("target/dd-tests/pcache/dir-policy-x86");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = dir.to_string_lossy().into_owned();
    let base: Vec<(&str, &str)> = vec![
        ("DDJIT_PCACHE", "1"),
        ("DDJIT_PCACHE_DIR", &dirs),
        ("COLDPROF", "1"),
    ];

    // ---- 1. exec re-key: two distinct cache files, both epochs saved cold (driver at exec, applet at
    // exit), both HIT warm with no re-save. Proves a save can never key to the exec'd binary. ----
    let chain = compile_guest_x86("execchain.c", "execchain.x86");
    let cold = run_engine_x86(&chain, &[], &base);
    assert_eq!(
        cold.code, 0,
        "x86 execchain cold: {}\n{}",
        cold.stdout, cold.stderr
    );
    assert_eq!(
        cold.stdout, "pcache execchain applet ok argc=1\n",
        "x86 cold stdout"
    );
    assert!(
        cold.stderr.contains("[pcache] MISS"),
        "driver MISS: {}",
        cold.stderr
    );
    assert!(
        cold.stderr.contains("[pcache] exec MISS"),
        "applet MISS: {}",
        cold.stderr
    );
    assert_eq!(
        cold.stderr.matches("save ok").count(),
        2,
        "cold must save both epochs: {}",
        cold.stderr
    );
    assert_eq!(
        cache_files(&dir).len(),
        2,
        "exec re-key must produce two distinct cache files"
    );
    let warm = run_engine_x86(&chain, &[], &base);
    assert_eq!(warm.stdout, cold.stdout, "x86 warm stdout byte-identical");
    assert!(
        warm.stderr.contains("[pcache] HIT"),
        "driver HIT: {}",
        warm.stderr
    );
    assert!(
        warm.stderr.contains("[pcache] exec HIT"),
        "applet exec HIT: {}",
        warm.stderr
    );
    assert!(
        !warm.stderr.contains("save ok"),
        "warm epoch must never re-save: {}",
        warm.stderr
    );

    // ---- 2. selective restore of a file-backed executable (library-like) map: cold persists it in a
    // 1-entry manifest; warm DEFERS it and activates it on the identity-matched re-map -> waste=0. ----
    let ldir = repo().join("target/dd-tests/pcache/dir-lib-x86");
    let _ = std::fs::remove_dir_all(&ldir);
    std::fs::create_dir_all(&ldir).unwrap();
    let ldirs = ldir.to_string_lossy().into_owned();
    let lbase: Vec<(&str, &str)> = vec![
        ("DDJIT_PCACHE", "1"),
        ("DDJIT_PCACHE_DIR", &ldirs),
        ("COLDPROF", "1"),
    ];
    let blob = ldir.join("blob.bin").to_string_lossy().into_owned();
    let lib = compile_guest_x86("libmap.c", "libmap.x86");
    let lc = run_engine_x86(&lib, &[&blob], &lbase);
    assert_eq!(lc.code, 0, "libmap cold: {}\n{}", lc.stdout, lc.stderr);
    assert_eq!(
        lc.stdout, "pcache libmap acc=506500\n",
        "libmap cold stdout"
    );
    assert!(
        lc.stderr.contains(" libs) in "),
        "cold save must record a library manifest: {}",
        lc.stderr
    );
    let lw = run_engine_x86(&lib, &[&blob], &lbase);
    assert_eq!(lw.stdout, lc.stdout, "libmap warm stdout byte-identical");
    assert!(
        lw.stderr.contains("deferred-lib=1"),
        "the library map's block must be DEFERRED on load: {}",
        lw.stderr
    );
    assert!(
        lw.stderr.contains("[pcache] warm-note restored="),
        "warm run must record revival stats: {}",
        lw.stderr
    );
    assert!(
        lw.stderr.contains("waste=0"),
        "the deferred block must ACTIVATE on the identity-matched re-map (no waste): {}",
        lw.stderr
    );

    // ---- 3. dead-weight SKIP: flip the sidecar the warm run just wrote to waste==restored; the next load
    // must SKIP the restore (header-only: MISS, correct output), never re-save, and stay sticky until a
    // fresh cold save clears it. This is the mechanism that keeps a library-heavy warm run <= pcache-off. ----
    let sidecar = std::fs::read_dir(&ldir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().ends_with(".warm"))
        .expect("warm run must publish a .warm sidecar");
    let mut bytes = std::fs::read(&sidecar).unwrap(); // 32 B: magic, arena_used, restored, waste
    assert_eq!(bytes.len(), 32, "sidecar layout");
    let restored = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    bytes[24..32].copy_from_slice(&restored.to_le_bytes()); // waste := restored -> the restore is all dead weight
    std::fs::write(&sidecar, &bytes).unwrap();
    let sk = run_engine_x86(&lib, &[&blob], &lbase);
    assert_eq!(sk.code, 0, "skip run must still succeed: {}", sk.stderr);
    assert_eq!(sk.stdout, lc.stdout, "skip run output byte-identical");
    assert!(
        sk.stderr.contains("[pcache] SKIP"),
        "dead-weight sidecar must trigger a SKIP: {}",
        sk.stderr
    );
    assert!(
        !sk.stderr.contains("save ok"),
        "a skipped epoch must not churn-resave: {}",
        sk.stderr
    );
    let sk2 = run_engine_x86(&lib, &[&blob], &lbase);
    assert!(
        sk2.stderr.contains("[pcache] SKIP"),
        "the skip verdict must be sticky: {}",
        sk2.stderr
    );
    // self-heal: a fresh cold save (files removed) clears the stale skip verdict -> HIT again
    for f in cache_files(&ldir) {
        std::fs::remove_file(f).unwrap();
    }
    assert!(
        run_engine_x86(&lib, &[&blob], &lbase)
            .stderr
            .contains("[pcache] MISS"),
        "removed file must MISS"
    );
    assert!(
        run_engine_x86(&lib, &[&blob], &lbase)
            .stderr
            .contains("[pcache] HIT"),
        "fresh save must HIT again"
    );
}
