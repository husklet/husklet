//! fork-server ("ddjitd", W3D) — cold-vs-forkserver EQUIVALENCE lane, BOTH Linux engines.
//!
//! The resident fork-server (`ddjit --server SOCK`) forks pre-initialized engine instances instead of
//! cold-spawning one per launch; `ddjit --client SOCK PROG ...` forwards a launch. The correctness
//! contract is INDISTINGUISHABILITY: for any guest, launching through the fork-server must produce the
//! same stdout, the same exit code, and the same fatal-signal death as a cold-spawned engine. This lane
//! drives that battery end to end on linux/aarch64 AND linux/x86_64 (the shared implementation lives in
//! `dd-jit/src/runtime/os/linux/forkserver.c`, parameterized by the per-target
//! container_init/engine_global_init/load_program/run_loaded/dd_run seam):
//!   1. identity probe — argv, guest env (DD_GUEST_ENV), cwd, tty triple, exit code.
//!   2. stdio plumbing — stdin reaches the guest, stdout comes back byte-identical.
//!   3. exit codes — arbitrary codes round-trip.
//!   4. fatal guest signal — a SIGSEGV death reproduces as a SIGSEGV death of the client (bash: 139).
//!   5. signal forwarding — SIGTERM sent to the CLIENT reaches the guest's handler (exit 7, "TERM").
//!   6. warm prewarm path — a `--prewarm` server serves the SAME golden output (pristine-image restore).
//!   7. pcache discipline — with DDJIT_PCACHE=1 in the CLIENT env: golden output, the fork-without-exec
//!      guest must NOT save (the never-save-from-fork-child latch), and a cold-populated cache must
//!      load warm inside a runner with identical output.
//!
//! Engine invocations mirror the matrix harness (`bash -lc` on macOS, `mac bash -lc` off-macOS); every
//! script carries its own env (the mac bridge drops ambient env) and every server started here is
//! reaped BY PID (kill guard), even on panic.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Run a shell script on the mac side (directly on macOS; through the `mac` bridge elsewhere), with a
/// hard timeout so a hung engine can never wedge the lane.
fn mac_sh(script: &str, timeout_s: u32) -> std::process::Output {
    let mut c = Command::new("timeout");
    c.arg(timeout_s.to_string());
    if cfg!(target_os = "macos") {
        c.arg("bash");
    } else {
        c.arg("mac").arg("bash");
    }
    c.arg("-lc").arg(script);
    c.output().expect("spawn mac shell")
}

fn out_str(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Compile the probe guest for an engine arch. Returns None (=> skip) if the compiler is unavailable.
fn compile_probe(arch: &str) -> Option<PathBuf> {
    let cc = match arch {
        "aarch64" => "gcc",
        _ => "x86_64-linux-gnu-gcc",
    };
    let src = repo().join("dd-tests/guests/forkserver/probe.c");
    let outdir = repo().join("target/dd-tests/forkserver").join(arch);
    std::fs::create_dir_all(&outdir).ok()?;
    let out = outdir.join("probe");
    let o = Command::new(cc)
        .args(["-O2", "-static-pie", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .ok()?;
    if !o.status.success() {
        eprintln!(
            "[forkserver] {cc} failed ({}): skipping",
            String::from_utf8_lossy(&o.stderr).trim()
        );
        return None;
    }
    Some(out)
}

/// A running fork-server, reaped by explicit PID on drop (panic-safe).
struct Server {
    pid: String,
    sock: String,
}

impl Server {
    /// Start `<engine> --server <sock> [extra args]` mac-side and wait for the socket to appear.
    fn start(engine: &str, sock: &str, extra: &str, log: &str) -> Option<Server> {
        let script = format!(
            "rm -f {sock}; {engine} --server {sock} {extra} >{log} 2>&1 & echo $!; \
             for i in $(seq 100); do [ -S {sock} ] && exit 0; sleep 0.1; done; exit 1"
        );
        let o = mac_sh(&script, 60);
        let pid = out_str(&o).trim().to_string();
        if !o.status.success() || pid.is_empty() {
            eprintln!(
                "[forkserver] server failed to start: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            // best-effort reap in case the process exists but the socket never appeared
            if !pid.is_empty() {
                mac_sh(&format!("kill -9 {pid} 2>/dev/null; true"), 20);
            }
            return None;
        }
        Some(Server {
            pid,
            sock: sock.to_string(),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // TERM first (exercises the graceful accept-loop stop), then KILL; always remove the socket.
        let s = format!(
            "kill {p} 2>/dev/null; for i in 1 2 3 4 5; do ps -p {p} >/dev/null 2>&1 || break; sleep 0.2; done; \
             kill -9 {p} 2>/dev/null; rm -f {sock}; true",
            p = self.pid,
            sock = self.sock
        );
        mac_sh(&s, 30);
    }
}

/// Run one guest launch cold and through the fork-server with identical env/cwd/stdin, and require an
/// identical (stdout, exit-code) pair. `prefix` is a shell prefix applied to BOTH (env vars, cd, pipe).
fn assert_equiv(
    name: &str,
    engine: &str,
    sock: &str,
    prefix: &str,
    guest_args: &str,
) -> (String, i32) {
    let cold = mac_sh(&format!("{prefix} {engine} {guest_args}; echo rc=$?"), 60);
    let fsrv = mac_sh(
        &format!("{prefix} {engine} --client {sock} {guest_args}; echo rc=$?"),
        60,
    );
    let (co, fo) = (out_str(&cold), out_str(&fsrv));
    assert_eq!(
        co,
        fo,
        "[{name}] cold vs forkserver stdout diverged\ncold stderr: {}\nfsrv stderr: {}",
        String::from_utf8_lossy(&cold.stderr),
        String::from_utf8_lossy(&fsrv.stderr)
    );
    let rc = co
        .lines()
        .last()
        .and_then(|l| l.strip_prefix("rc="))
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(-1);
    (co, rc)
}

fn engine_lane(guest: ddjit::Guest) {
    if !ddjit::available(guest) {
        eprintln!(
            "[forkserver] {} engine not built — skipping",
            guest.target()
        );
        return;
    }
    let arch = guest.arch();
    let engine = guest.jit_path().expect("engine path");
    let probe = match compile_probe(arch) {
        Some(p) => p.to_string_lossy().into_owned(),
        None => return,
    };
    let tag = format!("fsrv-test-{}-{}", std::process::id(), arch);
    let sock = format!("/tmp/{tag}.sock");
    let log = format!("/tmp/{tag}.log");
    let srv = match Server::start(&engine, &sock, "", &log) {
        Some(s) => s,
        None => panic!("fork-server failed to start for {arch} (log: {log})"),
    };

    // ---- 1. identity: argv + guest env + cwd + tty triple + exit code ----
    let (o, rc) = assert_equiv(
        "identity",
        &engine,
        &sock,
        "cd /tmp && DD_GUEST_ENV=FSRV_TEST_ENV=hello",
        &format!("{probe} id one two"),
    );
    assert_eq!(rc, 42, "[identity {arch}] exit code");
    assert!(
        o.contains("env=hello"),
        "[identity {arch}] guest env must arrive: {o}"
    );
    assert!(
        o.contains("argv[2]=one") && o.contains("argv[3]=two"),
        "[identity {arch}] argv: {o}"
    );
    assert!(
        o.contains("cwd=") && o.contains("tty=000"),
        "[identity {arch}] cwd/tty: {o}"
    );

    // ---- 1b. cwd equivalence: a DIFFERENT client cwd must be seen by the runner ----
    let (o2, _) = assert_equiv(
        "cwd",
        &engine,
        &sock,
        "cd /private/var/tmp &&",
        &format!("{probe} id"),
    );
    assert!(
        o2.contains("cwd=/private/var/tmp"),
        "[cwd {arch}] client cwd must reach the guest: {o2}"
    );

    // ---- 2. stdio plumbing ----
    let (o3, rc3) = assert_equiv(
        "stdin",
        &engine,
        &sock,
        "echo forkserver-stdin |",
        &format!("{probe} stdin"),
    );
    assert_eq!(rc3, 0);
    assert!(
        o3.contains("<forkserver-stdin"),
        "[stdin {arch}] stdin must reach the guest: {o3}"
    );

    // ---- 3. exit codes ----
    let (_, rc4) = assert_equiv("exit", &engine, &sock, "", &format!("{probe} exit 17"));
    assert_eq!(rc4, 17, "[exit {arch}]");

    // ---- 4. fatal guest signal: both deaths must look like SIGSEGV (bash rc=139) ----
    let (_, rc5) = assert_equiv("segv", &engine, &sock, "", &format!("{probe} segv"));
    assert_eq!(
        rc5, 139,
        "[segv {arch}] client must die by the guest's signal"
    );

    // ---- 5. signal forwarding: SIGTERM to the CLIENT reaches the guest's handler ----
    for mode in ["", &format!("--client {sock} ")] {
        let script = format!(
            "{engine} {mode}{probe} waitterm >/tmp/{tag}-term.out 2>/dev/null & CP=$!; \
             for i in $(seq 50); do grep -q ready /tmp/{tag}-term.out 2>/dev/null && break; sleep 0.1; done; \
             kill -TERM $CP; wait $CP; echo rc=$?; cat /tmp/{tag}-term.out"
        );
        let o = mac_sh(&script, 60);
        let s = out_str(&o);
        let label = if mode.is_empty() {
            "cold"
        } else {
            "forkserver"
        };
        assert!(
            s.contains("rc=7"),
            "[term {arch} {label}] guest handler must run (exit 7): {s}"
        );
        assert!(
            s.contains("TERM"),
            "[term {arch} {label}] handler output: {s}"
        );
    }

    drop(srv);

    // ---- 6. warm prewarm path: a --prewarm server must serve the same golden output ----
    let wsock = format!("/tmp/{tag}-warm.sock");
    let wsrv = Server::start(
        &engine,
        &wsock,
        &format!("--prewarm {probe}"),
        &format!("/tmp/{tag}-warm.log"),
    )
    .unwrap_or_else(|| panic!("prewarm server failed to start for {arch}"));
    for round in 0..2 {
        let (ow, rcw) = assert_equiv(
            "warm",
            &engine,
            &wsock,
            "DD_GUEST_ENV=FSRV_TEST_ENV=warmy",
            &format!("{probe} id"),
        );
        assert_eq!(rcw, 42, "[warm {arch} r{round}]");
        assert!(
            ow.contains("env=warmy"),
            "[warm {arch} r{round}] env through the WARM path: {ow}"
        );
    }
    let (_, rcw2) = assert_equiv("warm-exit", &engine, &wsock, "", &format!("{probe} exit 5"));
    assert_eq!(rcw2, 5, "[warm-exit {arch}] warm path exit code");
    drop(wsrv);

    // ---- 7. pcache discipline through the fork-server (client env carries DDJIT_PCACHE) ----
    let pcdir = format!("/tmp/{tag}-pcache");
    let sock2 = format!("/tmp/{tag}-pc.sock");
    let srv2 = Server::start(&engine, &sock2, "", &format!("/tmp/{tag}-pc.log"))
        .unwrap_or_else(|| panic!("pcache server failed to start for {arch}"));
    let pfx = format!("DDJIT_PCACHE=1 DDJIT_PCACHE_DIR={pcdir} COLDPROF=1");
    mac_sh(&format!("rm -rf {pcdir}; mkdir -p {pcdir}"), 20);
    // 7a. a runner is a fork child: it must produce golden output and must NOT save (no cache files).
    let f1 = mac_sh(
        &format!("{pfx} {engine} --client {sock2} {probe} exit 3; echo rc=$?"),
        60,
    );
    assert!(
        out_str(&f1).contains("rc=3"),
        "[pcache {arch}] runner rc: {}",
        out_str(&f1)
    );
    let ls1 = mac_sh(&format!("ls {pcdir} | wc -l"), 20);
    assert_eq!(out_str(&ls1).trim(), "0",
        "[pcache {arch}] a fork-server runner must never pcache_save (never-save-from-fork-child latch)");
    // 7b. populate the cache COLD (standalone save), then a runner must serve identical output warm.
    let c1 = mac_sh(&format!("{pfx} {engine} {probe} id; echo rc=$?"), 60);
    let ls2 = mac_sh(&format!("ls {pcdir} | wc -l"), 20);
    assert_ne!(
        out_str(&ls2).trim(),
        "0",
        "[pcache {arch}] the standalone cold run must save"
    );
    let f2 = mac_sh(
        &format!("{pfx} {engine} --client {sock2} {probe} id; echo rc=$?"),
        60,
    );
    assert_eq!(
        out_str(&c1),
        out_str(&f2),
        "[pcache {arch}] warm-load inside a runner must be byte-identical"
    );
    assert!(
        String::from_utf8_lossy(&f2.stderr).contains("[pcache] HIT"),
        "[pcache {arch}] the runner must warm-load the cold-saved arena: {}",
        String::from_utf8_lossy(&f2.stderr)
    );
    drop(srv2);
    mac_sh(
        &format!("rm -rf {pcdir} /tmp/{tag}*.log /tmp/{tag}-term.out"),
        20,
    );
}

#[test]
fn forkserver_equivalence_aarch64() {
    engine_lane(ddjit::Guest::LinuxAarch64);
}

#[test]
fn forkserver_equivalence_x86_64() {
    engine_lane(ddjit::Guest::LinuxX86_64);
}
