/// A JIT engine = one guest target (OS × ISA) the runtime can execute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    LinuxAarch64,
    LinuxX86_64,
}

impl Engine {
    pub const ALL: [Engine; 2] = [Engine::LinuxAarch64, Engine::LinuxX86_64];
    pub fn jit(self) -> hl_jit::Guest {
        match self {
            Engine::LinuxAarch64 => hl_jit::Guest::LinuxAarch64,
            Engine::LinuxX86_64 => hl_jit::Guest::LinuxX86_64,
        }
    }
    pub fn os(self) -> &'static str {
        self.jit().os()
    }
    pub fn arch(self) -> &'static str {
        self.jit().arch()
    }
    /// Display label that disambiguates same-ISA targets (e.g. linux/aarch64 vs darwin/aarch64).
    pub fn label(self) -> String {
        format!("{}/{}", self.os(), self.arch())
    }
    /// Whether this engine's JIT binary was built (by dd-jit's build.rs).
    pub fn available(self) -> bool {
        hl_jit::available(self.jit())
    }
    /// Whether guest binaries for this target can be compiled locally (only linux/aarch64 via native gcc).
    pub fn can_compile(self) -> bool {
        matches!(self, Engine::LinuxAarch64 | Engine::LinuxX86_64)
    }
}

/// How a case's guest binary is obtained for a given engine.
pub enum Bin {
    /// Compile a Linux C source under `guests/` (gcc -static-pie, per Linux arch). Linux engines only.
    Source(&'static str),
    /// Like `Source`, but linked STATIC NON-PIE (`-static -no-pie`) so the guest is an ET_EXEC that the
    /// loader biases high (`g_nonpie_lo` set) — the ONLY state that exercises dispatch.c's non-PIE g2h
    /// pointer-arg rebase switch. Used to guard that whole class  against regression. Linux only.
    SourceNoPie(&'static str),
    /// A portable POSIX C source under `guests/` built for *every* engine: gcc -static-pie for the two
    /// Linux engines. The one source proves the behaviour is identical across the Linux engines.
    /// Checks must be golden (deterministic stdout/exit).
    Portable(&'static str),
    /// Prebuilt fixture binaries, one per engine that has one.
    Fixture(&'static [(Engine, &'static str)]),
    /// The guest program is already inside the rootfs; `args[0]` names it (e.g. `/bin/sh`).
    InRootfs,
}

/// A single expectation, evaluated against the JIT run.
pub enum Check {
    Exit(i32),
    Out(&'static str),
    OutHas(&'static str),
    /// Run the same guest natively and require identical stdout + exit (aarch64 source guests only).
    Oracle,
}

/// One test: a guest program + how to launch it + what to expect.
pub struct Case {
    pub name: &'static str,
    pub bin: Bin,
    pub args: Vec<String>,
    pub rootfs: Option<&'static str>, // image name (resolved per-arch) or None = bare
    pub lowers: Vec<String>,
    /// Run under an overlay: inject the resolved rootfs as its own lower so g_nlower>0 activates the
    /// overlay open/getdents/lseek code path (needed to reproduce overlay-only bugs, e.g. lseek).
    pub overlay: bool,
    /// scratch/distroless guard: run the compiled guest inside a synthesized EMPTY rootfs (only a
    /// `/tmp` landing dir for the jailed guest copy) — the FROM-scratch condition (no shell, interpreter
    /// or libc on disk). Proves the loader/exec path resolves + execs a static binary that is the sole
    /// executable in its rootfs, exactly like nats-server / hello-world's `/hello`. Ignores `rootfs`.
    pub scratch: bool,
    pub mem_max: u64,
    pub engines: Vec<Engine>,
    /// Engines where this case is a KNOWN failure (jit86 translator/service bugs under debugging) — a
    /// fail there is reported `xfail`, not a regression.
    pub xfail: Vec<Engine>,
    /// Run the guest with the untrusted-guest SENTRY split enabled (sets `HL_JIT_UNTRUSTED=1` in the
    /// engine's env so fs/net/proc syscalls route through the forked sentry over the SPSC ring). OFF by
    /// default → the existing matrix is byte-identical. Sets ONLY `HL_JIT_UNTRUSTED` (the ring
    /// marshaling/forwarding); the stronger `HL_JIT_SANDBOX` public mode is driven by `.sandbox()` below.
    pub untrusted: bool,
    /// Run the guest under the PUBLIC sandbox mode (`docker run --security-opt sandbox`, i.e.
    /// `Container::builder().sandbox(true)`): sets BOTH `HL_JIT_UNTRUSTED=1` and `HL_JIT_SANDBOX=1` in the
    /// engine's env, exactly as the public builder emits. On macOS this additionally confines the JIT
    /// worker under the deny-default Seatbelt profile (sentry.c `worker_sandbox`); on Linux the Seatbelt
    /// step is a no-op, so the case pins that the public-mode env combo launches and a fully-forwarded
    /// guest still produces the trusted golden. OFF by default. Implies untrusted (sandbox is only sound
    /// once fs/net/proc are forwarded).
    pub sandbox: bool,
    /// docker `--cpus` online-CPU cap (0 = unset). Threads to SpawnConfig.cpus -> HL_CPUS.
    pub cpus: u32,
    /// docker `--read-only` rootfs. Threads to SpawnConfig.read_only -> HL_ROOTFS_RO.
    pub read_only: bool,
    /// docker `--ulimit` (name, soft, hard) triples. Threads to SpawnConfig.ulimits -> HL_ULIMITS.
    pub ulimits: Vec<(String, u64, u64)>,
    /// Extra engine env (`(KEY, VALUE)`) baked into the launch script — used to exercise the container
    /// network model in-process (e.g. `HL_NETNS`/`HL_NETBR`/`HL_IP` turn on the private-loopback + per-
    /// network AF_UNIX switch a bare guest otherwise never sees). Inert on the native oracle run.
    pub env: Vec<(String, String)>,
    pub checks: Vec<Check>,
}

/// A named collection of cases.
pub struct Group {
    pub name: &'static str,
    pub cases: Vec<Case>,
}

pub fn group(name: &'static str, cases: Vec<Case>) -> Group {
    Group { name, cases }
}

// ---- ergonomic builders ----
fn base(name: &'static str, bin: Bin) -> Case {
    let engines = match &bin {
        Bin::Source(_) => vec![Engine::LinuxAarch64, Engine::LinuxX86_64], // same source, both Linux engines
        Bin::SourceNoPie(_) => vec![Engine::LinuxAarch64, Engine::LinuxX86_64], // non-PIE ET_EXEC, both Linux
        Bin::Portable(_) => Engine::ALL.to_vec(), // every engine: Linux x2
        Bin::Fixture(fx) => fx.iter().map(|(e, _)| *e).collect(),
        Bin::InRootfs => vec![Engine::LinuxAarch64], // container rootfs fixtures are aarch64 today
    };
    Case {
        name,
        bin,
        args: vec![],
        rootfs: None,
        lowers: vec![],
        overlay: false,
        scratch: false,
        mem_max: 0,
        engines,
        xfail: vec![],
        untrusted: false,
        sandbox: false,
        cpus: 0,
        read_only: false,
        ulimits: vec![],
        env: vec![],
        checks: vec![],
    }
}
/// A case whose guest is compiled from a Linux/aarch64 C source under `guests/`.
pub fn src(name: &'static str, source: &'static str) -> Case {
    base(name, Bin::Source(source))
}
/// A case whose guest is compiled STATIC NON-PIE (ET_EXEC) — the only build that turns on dispatch.c's
/// non-PIE pointer-arg rebase (`g_nonpie_lo`). Pair with `.oracle()` to prove every rebased syscall
/// dereferences a valid low.bss/stack pointer identically to native (regression guard for).
pub fn src_nopie(name: &'static str, source: &'static str) -> Case {
    base(name, Bin::SourceNoPie(source))
}
/// A case whose guest is a portable POSIX source under `guests/`, run on EVERY engine (Linux x2).
/// Use golden checks — the same deterministic output must appear on every Linux engine.
pub fn port(name: &'static str, source: &'static str) -> Case {
    base(name, Bin::Portable(source))
}
/// A case whose guest is a prebuilt fixture, per engine.
pub fn fixture(name: &'static str, fx: &'static [(Engine, &'static str)]) -> Case {
    base(name, Bin::Fixture(fx))
}
/// A case that runs a program already inside the rootfs (e.g. busybox); `a` is the full argv.
pub fn in_rootfs(name: &'static str, rootfs: &'static str, a: &[&str]) -> Case {
    let mut c = base(name, Bin::InRootfs);
    c.rootfs = Some(rootfs);
    c.args = a.iter().map(|s| s.to_string()).collect();
    c
}

impl Case {
    pub fn arg(mut self, a: &str) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn args(mut self, a: &[&str]) -> Self {
        self.args.extend(a.iter().map(|s| s.to_string()));
        self
    }
    pub fn rootfs(mut self, r: &'static str) -> Self {
        self.rootfs = Some(r);
        self
    }
    pub fn lower(mut self, l: &str) -> Self {
        self.lowers.push(l.into());
        self
    }
    pub fn overlay(mut self) -> Self {
        self.overlay = true;
        self
    }
    /// run this compiled guest inside a synthesized EMPTY (FROM-scratch) rootfs — the guest is the
    /// sole executable, no shell/interpreter/libc on disk. Guards the loader/exec path for scratch/
    /// distroless images (nats-server, hello-world's `/hello`). Linux engines only (compiled static-PIE).
    pub fn scratch(mut self) -> Self {
        self.scratch = true;
        self
    }
    pub fn mem(mut self, m: u64) -> Self {
        self.mem_max = m;
        self
    }
    /// docker `--cpus` online-CPU cap for this case (container isolation / resource fidelity).
    pub fn cpus(mut self, n: u32) -> Self {
        self.cpus = n;
        self
    }
    /// docker `--read-only` rootfs for this case.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    /// Add a docker `--ulimit NAME=SOFT:HARD` for this case.
    pub fn ulimit(mut self, name: &str, soft: u64, hard: u64) -> Self {
        self.ulimits.push((name.into(), soft, hard));
        self
    }
    /// Set an extra engine env var for this case (e.g. `HL_NETNS`/`HL_NETBR`/`HL_IP` to enable the
    /// container network switch). Baked into the JIT launch env; not passed to the native oracle.
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn only(mut self, e: &[Engine]) -> Self {
        self.engines = e.to_vec();
        self
    }
    pub fn exit(mut self, c: i32) -> Self {
        self.checks.push(Check::Exit(c));
        self
    }
    pub fn out(mut self, s: &'static str) -> Self {
        self.checks.push(Check::Out(s));
        self
    }
    pub fn has(mut self, s: &'static str) -> Self {
        self.checks.push(Check::OutHas(s));
        self
    }
    pub fn oracle(mut self) -> Self {
        self.checks.push(Check::Oracle);
        self
    }
    /// Mark this case a KNOWN failure on the given engines (jit86 bugs under debugging): a fail there
    /// is reported `xfail` (not a regression); an unexpected pass is reported `XPASS`.
    pub fn xfail(mut self, e: &[Engine]) -> Self {
        self.xfail = e.to_vec();
        self
    }
    /// Enable the untrusted-guest SENTRY split for this case (`HL_JIT_UNTRUSTED=1` in the engine's env):
    /// fs/net/proc syscalls are marshaled to the forked sentry over the ring instead of run in the JIT
    /// worker. Used to re-run a guest under the split and assert the SAME golden output as the trusted
    /// baseline. Linux-engine only in effect (the sentry is Linux-only); the env is inert on darwin.
    pub fn untrusted(mut self) -> Self {
        self.untrusted = true;
        self
    }
    /// Enable the PUBLIC sandbox mode for this case (`HL_JIT_UNTRUSTED=1` + `HL_JIT_SANDBOX=1`, the exact
    /// env combo `Container::builder().sandbox(true)` / `docker run --security-opt sandbox` emit). Drives
    /// the sentry split AND the (macOS) Seatbelt worker confinement, so the public mode is no longer
    /// avoided by the matrix. Re-run a fully-forwarded guest under it and assert the trusted golden.
    pub fn sandbox(mut self) -> Self {
        self.sandbox = true;
        self
    }
}

/// Result of running one case on one engine.
pub enum Status {
    Pass,
    Fail(String),
    Skip(String),
    /// A KNOWN failure (the case is `.xfail()`-marked here) — tracked, not a regression.
    Xfail(String),
    /// An xfail-marked case that unexpectedly PASSED — the bug may be fixed; un-mark it.
    Xpass,
}

/// One matrix cell result, tagged with which engine produced it — the input to [`gate_failures`].
pub struct Cell {
    pub name: String,
    pub engine: Engine,
    pub status: Status,
}

/// Apply the CI gate invariants to a completed matrix run and return the list of gate failures (empty
/// ⇒ green). This is the single place the cargo-test path (`tests/suite.rs`) shares with the CLI runner,
/// so a green matrix can never hide any of these false-green conditions:
///
///   * **Fail** — an ordinary failed case.
///   * **XPASS** — an `.xfail()`-marked case that now passes. It is NOT a silent non-event: the marker
///     must be removed, else a later re-break reads as a "known" failure. So it fails the gate.
///   * **Dark lane** — a `required` engine whose JIT binary didn't build. Its whole lane skips silently,
///     so a matrix could report "0 failed" while the primary arch was never tested (the v0.9.56
///     regression). Fold each missing engine into the failures.
///   * **Empty / stale** — zero cases actually passed. A matrix that ran nothing (bad selection, every
///     lane dark) must not report green.
pub fn gate_failures(
    required: &[Engine],
    available: &dyn Fn(Engine) -> bool,
    cells: &[Cell],
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut passed = 0u32;
    for c in cells {
        match &c.status {
            Status::Pass => passed += 1,
            Status::Fail(m) => failures.push(format!("{} [{}]: {}", c.name, c.engine.label(), m)),
            Status::Xpass => failures.push(format!(
                "{} [{}]: XPASS — a known-failure (.xfail) case now passes; remove its .xfail marker",
                c.name,
                c.engine.label()
            )),
            Status::Skip(_) | Status::Xfail(_) => {}
        }
    }
    for &e in required {
        if !available(e) {
            failures.push(format!(
                "engine {} MISSING — its JIT binary was not built (failed dd-jit compile / empty \
                 HL_JIT_{} env); the ENTIRE {} lane was DARK (every case on it skipped, NOT tested)",
                e.label(),
                e.label().replace('/', "_").to_uppercase(),
                e.label()
            ));
        }
    }
    if passed == 0 {
        failures.push(
            "no case passed — the matrix ran nothing (empty selection or every lane dark/stale)"
                .to_string(),
        );
    }
    failures
}
