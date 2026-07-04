//! dd-tests — a declarative test harness that runs guest programs across every JIT engine.
//!
//! A [`Case`] is one guest program + its expected behaviour. Cases are organised into named
//! [`Group`]s. The runner executes the **engine × case** matrix: each case runs on every engine whose
//! guest architecture it can be provisioned for (aarch64 guests are compiled on the fly; x86-64 guests
//! come from prebuilt fixtures, since there's no local cross-compiler). Checks are golden
//! (exit/stdout) or differential against a native oracle.
//!
//! ```ignore
//! group("compat", [
//!     src("hello", "hello.c").exit(42).out("hi\n"),
//!     src("sort",  "sort.c").oracle(),                 // diff vs native run
//! ])
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub mod cases;
pub mod scenario;   // real-software surface: drive popular images through dd-daemon (Real-oracle vs Dd)
pub mod scenarios;  // the scenario registry (one folder per category)
pub mod diag;       // turn a failed run into an actionable JIT bug report (signals/buckets/crash details)

/// A JIT engine = one guest target (OS × ISA) the runtime can execute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine { LinuxAarch64, LinuxX86_64, DarwinAarch64 }

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::LinuxAarch64, Engine::LinuxX86_64, Engine::DarwinAarch64];
    pub fn jit(self) -> ddjit::Guest {
        match self {
            Engine::LinuxAarch64 => ddjit::Guest::LinuxAarch64,
            Engine::LinuxX86_64 => ddjit::Guest::LinuxX86_64,
            Engine::DarwinAarch64 => ddjit::Guest::DarwinAarch64,
        }
    }
    pub fn os(self) -> &'static str { self.jit().os() }
    pub fn arch(self) -> &'static str { self.jit().arch() }
    /// Display label that disambiguates same-ISA targets (e.g. linux/aarch64 vs darwin/aarch64).
    pub fn label(self) -> String { format!("{}/{}", self.os(), self.arch()) }
    /// Whether this engine's JIT binary was built (by dd-jit's build.rs).
    pub fn available(self) -> bool { ddjit::available(self.jit()) }
    /// Whether guest binaries for this target can be compiled locally (only linux/aarch64 via native gcc).
    pub fn can_compile(self) -> bool { matches!(self, Engine::LinuxAarch64 | Engine::LinuxX86_64) }
}

/// How a case's guest binary is obtained for a given engine.
pub enum Bin {
    /// Compile a Linux C source under `guests/` (gcc -static-pie, per Linux arch). Linux engines only.
    Source(&'static str),
    /// Like `Source`, but linked STATIC NON-PIE (`-static -no-pie`) so the guest is an ET_EXEC that the
    /// loader biases high (`g_nonpie_lo` set) — the ONLY state that exercises dispatch.c's non-PIE g2h
    /// pointer-arg rebase switch. Used to guard that whole class (#409/#419) against regression. Linux only.
    SourceNoPie(&'static str),
    /// A portable POSIX C source under `guests/` built for *every* engine: gcc -static-pie for the two
    /// Linux engines, clang (full libSystem) Mach-O for darwin. The one source proves the behaviour is
    /// identical on Linux (JIT-emulated) and macOS (native under darwinjail) — so coverage isn't
    /// Linux-only. Checks must be golden (deterministic stdout/exit), since darwin has no native oracle.
    Portable(&'static str),
    /// Compile a macOS/aarch64 Mach-O C source under `guests/darwin/` (mac clang).
    DarwinSource(&'static str),
    /// A macOS-only C source (path relative to `guests/`, e.g. `darwin/kqueue.c`) built with the full
    /// libSystem (normal C runtime + main) and run on the darwin engine. For BSD/Mach-only APIs
    /// (kqueue, sysctl, mach_*) that have no Linux form — the darwin counterpart to a Linux-only `src`.
    DarwinLibc(&'static str),
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
    /// overlay open/getdents/lseek code path (needed to reproduce overlay-only bugs, e.g. #391 lseek).
    pub overlay: bool,
    /// #231 scratch/distroless guard: run the compiled guest inside a synthesized EMPTY rootfs (only a
    /// `/tmp` landing dir for the jailed guest copy) — the FROM-scratch condition (no shell, interpreter
    /// or libc on disk). Proves the loader/exec path resolves + execs a static binary that is the sole
    /// executable in its rootfs, exactly like nats-server / hello-world's `/hello`. Ignores `rootfs`.
    pub scratch: bool,
    pub mem_max: u64,
    pub engines: Vec<Engine>,
    /// Engines where this case is a KNOWN failure (jit86 translator/service bugs under debugging) — a
    /// fail there is reported `xfail`, not a regression.
    pub xfail: Vec<Engine>,
    /// Run the guest with the untrusted-guest SENTRY split enabled (sets `DDJIT_UNTRUSTED=1` in the
    /// engine's env so fs/net/proc syscalls route through the forked sentry over the SPSC ring). OFF by
    /// default → the existing matrix is byte-identical. `DDJIT_SANDBOX` is intentionally NOT set: this
    /// validates the ring marshaling/forwarding, not the (macOS) Seatbelt confinement.
    pub untrusted: bool,
    /// docker `--cpus` online-CPU cap (0 = unset). Threads to SpawnConfig.cpus -> DD_CPUS.
    pub cpus: u32,
    /// docker `--read-only` rootfs. Threads to SpawnConfig.read_only -> DD_ROOTFS_RO.
    pub read_only: bool,
    /// docker `--ulimit` (name, soft, hard) triples. Threads to SpawnConfig.ulimits -> DD_ULIMITS.
    pub ulimits: Vec<(String, u64, u64)>,
    /// Extra engine env (`(KEY, VALUE)`) baked into the launch script — used to exercise the container
    /// network model in-process (e.g. `DD_NETNS`/`DD_NETBR`/`DD_IP` turn on the private-loopback + per-
    /// network AF_UNIX switch a bare guest otherwise never sees). Inert on the native oracle run.
    pub env: Vec<(String, String)>,
    pub checks: Vec<Check>,
}

/// A named collection of cases.
pub struct Group { pub name: &'static str, pub cases: Vec<Case> }

pub fn group(name: &'static str, cases: Vec<Case>) -> Group { Group { name, cases } }

// ---- ergonomic builders ----
fn base(name: &'static str, bin: Bin) -> Case {
    let engines = match &bin {
        Bin::Source(_) => vec![Engine::LinuxAarch64, Engine::LinuxX86_64], // same source, both Linux engines
        Bin::SourceNoPie(_) => vec![Engine::LinuxAarch64, Engine::LinuxX86_64], // non-PIE ET_EXEC, both Linux
        Bin::Portable(_) => Engine::ALL.to_vec(),                          // every engine: Linux x2 + darwin
        Bin::DarwinSource(_) => vec![Engine::DarwinAarch64],
        Bin::DarwinLibc(_) => vec![Engine::DarwinAarch64],
        Bin::Fixture(fx) => fx.iter().map(|(e, _)| *e).collect(),
        Bin::InRootfs => vec![Engine::LinuxAarch64], // container rootfs fixtures are aarch64 today
    };
    Case { name, bin, args: vec![], rootfs: None, lowers: vec![], overlay: false, scratch: false, mem_max: 0, engines, xfail: vec![], untrusted: false, cpus: 0, read_only: false, ulimits: vec![], env: vec![], checks: vec![] }
}
/// A case whose guest is compiled from a Linux/aarch64 C source under `guests/`.
pub fn src(name: &'static str, source: &'static str) -> Case { base(name, Bin::Source(source)) }
/// A case whose guest is compiled STATIC NON-PIE (ET_EXEC) — the only build that turns on dispatch.c's
/// non-PIE pointer-arg rebase (`g_nonpie_lo`). Pair with `.oracle()` to prove every rebased syscall
/// dereferences a valid low .bss/stack pointer identically to native (regression guard for #409/#419).
pub fn src_nopie(name: &'static str, source: &'static str) -> Case { base(name, Bin::SourceNoPie(source)) }
/// A case whose guest is a portable POSIX source under `guests/`, run on EVERY engine (Linux x2 +
/// darwin). Use golden checks — the same deterministic output must appear on Linux and macOS.
pub fn port(name: &'static str, source: &'static str) -> Case { base(name, Bin::Portable(source)) }
/// A case whose guest is compiled from a macOS/aarch64 Mach-O C source under `guests/darwin/`.
pub fn darwin_src(name: &'static str, source: &'static str) -> Case { base(name, Bin::DarwinSource(source)) }
/// A macOS-only case (source path relative to `guests/`, e.g. `darwin/kqueue.c`), full-libSystem, run
/// on the darwin engine only. For BSD/Mach APIs with no Linux equivalent. Golden-checked.
pub fn darwin_libc(name: &'static str, source: &'static str) -> Case { base(name, Bin::DarwinLibc(source)) }
/// A case whose guest is a prebuilt fixture, per engine.
pub fn fixture(name: &'static str, fx: &'static [(Engine, &'static str)]) -> Case { base(name, Bin::Fixture(fx)) }
/// A case that runs a program already inside the rootfs (e.g. busybox); `a` is the full argv.
pub fn in_rootfs(name: &'static str, rootfs: &'static str, a: &[&str]) -> Case {
    let mut c = base(name, Bin::InRootfs);
    c.rootfs = Some(rootfs);
    c.args = a.iter().map(|s| s.to_string()).collect();
    c
}

impl Case {
    pub fn arg(mut self, a: &str) -> Self { self.args.push(a.into()); self }
    pub fn args(mut self, a: &[&str]) -> Self { self.args.extend(a.iter().map(|s| s.to_string())); self }
    pub fn rootfs(mut self, r: &'static str) -> Self { self.rootfs = Some(r); self }
    pub fn lower(mut self, l: &str) -> Self { self.lowers.push(l.into()); self }
    pub fn overlay(mut self) -> Self { self.overlay = true; self }
    /// #231: run this compiled guest inside a synthesized EMPTY (FROM-scratch) rootfs — the guest is the
    /// sole executable, no shell/interpreter/libc on disk. Guards the loader/exec path for scratch/
    /// distroless images (nats-server, hello-world's `/hello`). Linux engines only (compiled static-PIE).
    pub fn scratch(mut self) -> Self { self.scratch = true; self }
    pub fn mem(mut self, m: u64) -> Self { self.mem_max = m; self }
    /// docker `--cpus` online-CPU cap for this case (container isolation / resource fidelity).
    pub fn cpus(mut self, n: u32) -> Self { self.cpus = n; self }
    /// docker `--read-only` rootfs for this case.
    pub fn read_only(mut self) -> Self { self.read_only = true; self }
    /// Add a docker `--ulimit NAME=SOFT:HARD` for this case.
    pub fn ulimit(mut self, name: &str, soft: u64, hard: u64) -> Self { self.ulimits.push((name.into(), soft, hard)); self }
    /// Set an extra engine env var for this case (e.g. `DD_NETNS`/`DD_NETBR`/`DD_IP` to enable the
    /// container network switch). Baked into the JIT launch env; not passed to the native oracle.
    pub fn env(mut self, k: &str, v: &str) -> Self { self.env.push((k.into(), v.into())); self }
    pub fn only(mut self, e: &[Engine]) -> Self { self.engines = e.to_vec(); self }
    pub fn exit(mut self, c: i32) -> Self { self.checks.push(Check::Exit(c)); self }
    pub fn out(mut self, s: &'static str) -> Self { self.checks.push(Check::Out(s)); self }
    pub fn has(mut self, s: &'static str) -> Self { self.checks.push(Check::OutHas(s)); self }
    pub fn oracle(mut self) -> Self { self.checks.push(Check::Oracle); self }
    /// Mark this case a KNOWN failure on the given engines (jit86 bugs under debugging): a fail there
    /// is reported `xfail` (not a regression); an unexpected pass is reported `XPASS`.
    pub fn xfail(mut self, e: &[Engine]) -> Self { self.xfail = e.to_vec(); self }
    /// Enable the untrusted-guest SENTRY split for this case (`DDJIT_UNTRUSTED=1` in the engine's env):
    /// fs/net/proc syscalls are marshaled to the forked sentry over the ring instead of run in the JIT
    /// worker. Used to re-run a guest under the split and assert the SAME golden output as the trusted
    /// baseline. Linux-engine only in effect (the sentry is Linux-only); the env is inert on darwin.
    pub fn untrusted(mut self) -> Self { self.untrusted = true; self }
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

/// Shared paths/config for a run.
pub struct Ctx {
    pub repo: PathBuf,     // dd repo root (shared mount, visible to the mac-side JIT)
    pub guests: PathBuf,   // dd-tests/guests
    pub cache: PathBuf,    // compiled-guest cache (under target/, shared)
    pub images: PathBuf,   // image rootfs dir (default the poc images)
}

impl Ctx {
    pub fn discover() -> Ctx {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let cache = repo.join("target/dd-tests");
        std::fs::create_dir_all(cache.join("aarch64")).ok();
        Ctx {
            guests: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("guests"),
            images: std::env::var("DD_IMAGES").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/x/dd/poc/images")),
            cache, repo,
        }
    }
    /// Resolve an image name to its rootfs path for the engine's guest arch.
    ///
    /// The image dirs hold prebuilt rootfses for different guest arches (e.g. an aarch64 `alpine` and an
    /// x86-64 `nginx_alpine`), so a naive substring match could hand an x86-64 rootfs to the aarch64
    /// engine — the guest would then exit 255 with empty output. We therefore (1) require the rootfs's
    /// ELF arch to match `e.arch()`, and (2) prefer an EXACT image-name match over a mere substring one.
    /// Among equal candidates the lowest sorted dir name wins, so selection is deterministic.
    fn rootfs_path(&self, name: &str, e: Engine) -> Option<String> {
        // Score candidates and keep the best (lowest) key. The key orders by:
        //   tier — see image_name_tier (0 registry-encoded pull, 1 sidecar-name, 2 literal dir-name),
        //          3 incidental substring match; a real pulled image (e.g. docker.io_library_alpine_latest,
        //          which has /etc/hostname) thus beats a hand-rolled `alpine/` dir that merely shares the name.
        //   arch — 0 if the rootfs's ELF arch matches the engine, 1 if undeterminable (arch MISMATCHES are
        //          rejected outright, so we never feed an x86-64 rootfs to the aarch64 engine or vice-versa).
        //   dir name — final tie-break so selection is deterministic.
        let mut best: Option<(u8, u8, String, PathBuf)> = None;
        for ent in std::fs::read_dir(&self.images).ok()?.flatten() {
            let dir = ent.path();
            let rootfs = dir.join("rootfs");
            if !rootfs.is_dir() { continue; }
            let dname = dir.file_name()?.to_string_lossy().to_string();
            let tier = match image_name_tier(&dir, &dname, name) {
                Some(t) => t,
                None if dname.contains(name) => 3, // incidental substring — weakest match
                None => continue,
            };
            let arch = match rootfs_machine(&rootfs) {
                Some(m) if m == elf_machine(e) => 0,
                Some(_) => continue,
                None => 1,
            };
            let key = (tier, arch, dname, rootfs);
            if best.as_ref().map_or(true, |b| key < *b) { best = Some(key); }
        }
        best.map(|(.., rootfs)| rootfs.to_string_lossy().into_owned())
    }
}

/// EM_* value (ELF `e_machine`) for an engine's guest ISA.
fn elf_machine(e: Engine) -> u16 {
    match e.arch() {
        "x86_64" => 0x3E, // EM_X86_64
        _ => 0xB7,        // EM_AARCH64
    }
}

/// Read the ELF `e_machine` of a rootfs by probing a few common executables; `None` if undeterminable.
fn rootfs_machine(rootfs: &Path) -> Option<u16> {
    for cand in ["bin/busybox", "bin/dash", "bin/bash", "bin/ls", "bin/cat", "bin/sh"] {
        let p = rootfs.join(cand);
        // Skip symlinks (e.g. sh -> dash): resolve only plain ELF files to keep this host-path safe.
        if p.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(true) { continue; }
        if let Ok(b) = std::fs::read(&p) {
            if b.len() >= 20 && &b[0..4] == b"\x7fELF" {
                return Some(u16::from_le_bytes([b[18], b[19]]));
            }
        }
    }
    None
}

/// Exact-match tier for an image dir against a requested `name` (lower = stronger):
///   0 — a `docker.io_<ns>_<repo>_<tag>` registry-encoded dir whose decoded repo matches: a REAL pulled
///       image (these carry the full rootfs, e.g. `/etc/hostname`), so it beats a hand-built dir.
///   1 — the `name`/`repo` recorded in the dir's `dd-image.json` sidecar matches (non-registry dir).
///   2 — the dir is literally named `name` (hand-built bundle dirs like `gcc-bundle`).
///   `None` — no exact match (the caller may still fall back to a substring match).
fn image_name_tier(dir: &Path, dname: &str, name: &str) -> Option<u8> {
    // Decode the docker dir encoding: docker.io_library_alpine_latest -> repo "alpine".
    if let Some(rest) = dname.strip_prefix("docker.io_library_") {
        if let Some((repo, _tag)) = rest.rsplit_once('_') {
            if repo == name { return Some(0); }
        }
    }
    if let Ok(json) = std::fs::read_to_string(dir.join("dd-image.json")) {
        if let Some(img) = json.split("\"name\":\"").nth(1).and_then(|s| s.split('"').next()) {
            // img is "repo:tag" (or "ns/repo:tag"); accept full match or the repo before ':'.
            if img == name || img.split(':').next() == Some(name) { return Some(1); }
        }
    }
    if dname == name { return Some(2); }
    None
}

/// Compile a guest C source for a Linux engine. aarch64 = native gcc, x86_64 = the cross compiler; both
/// static-PIE, cached by mtime under cache/<arch>/. The same source runs on both engines (the point —
/// it makes the engine matrix dense). Returns the binary path.
fn compile(ctx: &Ctx, source: &str, e: Engine) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx.cache.join(e.arch()).join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let needs = !out.exists()
        || std::fs::metadata(&src).and_then(|m| m.modified()).ok()
            >= std::fs::metadata(&out).and_then(|m| m.modified()).ok();
    if needs {
        // aarch64: native gcc + libsqlite3/libdl (real-software guests). x86_64: the cross compiler,
        // libm only (no x86 libsqlite3 on the dev host). Static, unused libs aren't pulled.
        let (cc, libs): (&str, &[&str]) = match e {
            Engine::LinuxAarch64 => ("gcc", &["-lsqlite3", "-lm", "-ldl"]),
            Engine::LinuxX86_64 => ("x86_64-linux-gnu-gcc", &["-lm"]),
            _ => return Err(format!("{} is not a compilable Linux target", e.label())),
        };
        let o = Command::new(cc).args(["-O2", "-static-pie", "-pthread"])
            .arg("-o").arg(&out).arg(&src).args(libs).output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() { return Err(format!("compile {source} [{}]: {}", e.arch(), String::from_utf8_lossy(&o.stderr).trim())); }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Compile a guest STATIC NON-PIE (`-static -no-pie` → ET_EXEC), so the loader biases it high and turns
/// on dispatch.c's non-PIE pointer-arg rebase (`g_nonpie_lo`). Cached under cache/<arch>/nopie/ so it
/// never collides with the same source's static-PIE build. Same native/qemu oracle as `compile`.
fn compile_nopie(ctx: &Ctx, source: &str, e: Engine) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx.cache.join(e.arch()).join("nopie").join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let needs = !out.exists()
        || std::fs::metadata(&src).and_then(|m| m.modified()).ok()
            >= std::fs::metadata(&out).and_then(|m| m.modified()).ok();
    if needs {
        let (cc, libs): (&str, &[&str]) = match e {
            Engine::LinuxAarch64 => ("gcc", &["-lm"]),
            Engine::LinuxX86_64 => ("x86_64-linux-gnu-gcc", &["-lm"]),
            _ => return Err(format!("{} is not a compilable Linux target", e.label())),
        };
        let o = Command::new(cc).args(["-O2", "-static", "-no-pie", "-pthread"])
            .arg("-o").arg(&out).arg(&src).args(libs).output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() { return Err(format!("compile-nopie {source} [{}]: {}", e.arch(), String::from_utf8_lossy(&o.stderr).trim())); }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Provision the guest binary path for a case on an engine. `Ok(None)` = skip (no guest for this arch).
fn provision(ctx: &Ctx, c: &Case, e: Engine) -> Result<Option<String>, String> {
    match &c.bin {
        Bin::Source(s) if e.can_compile() => compile(ctx, s, e).map(Some),
        Bin::Source(_) => Ok(None),
        Bin::SourceNoPie(s) if e.can_compile() => compile_nopie(ctx, s, e).map(Some),
        Bin::SourceNoPie(_) => Ok(None),
        // portable POSIX: Linux engines via gcc (same as Source), darwin via clang+libSystem.
        Bin::Portable(s) if e.can_compile() => compile(ctx, s, e).map(Some),
        Bin::Portable(s) if e == Engine::DarwinAarch64 => compile_darwin_libc(ctx, s).map(Some),
        Bin::Portable(_) => Ok(None),
        Bin::DarwinSource(s) if e == Engine::DarwinAarch64 => compile_darwin(ctx, s).map(Some),
        Bin::DarwinSource(_) => Ok(None),
        Bin::DarwinLibc(s) if e == Engine::DarwinAarch64 => compile_darwin_libc(ctx, s).map(Some),
        Bin::DarwinLibc(_) => Ok(None),
        Bin::Fixture(fx) => Ok(fx.iter().find(|(fe, _)| *fe == e).map(|(_, p)| resolve(ctx, p))),
        Bin::InRootfs => Ok(Some(String::new())), // nothing to build; argv[0] is in-rootfs
    }
}

/// Compile a static macOS/arm64 Mach-O guest from `guests/darwin/<source>` via the mac toolchain.
/// (Darwin guests use a different syscall ABI than linux, so they're their own sources; checked golden
/// since they can't run natively on a linux dev host for an oracle.)
fn compile_darwin(ctx: &Ctx, source: &str) -> Result<String, String> {
    let src = ctx.guests.join("darwin").join(source);
    let out = ctx.cache.join("darwin").join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let fresh = std::fs::metadata(&out).and_then(|m| m.modified()).ok()
        >= std::fs::metadata(&src).and_then(|m| m.modified()).ok();
    if !out.exists() || !fresh {
        let script = format!("clang -arch arm64 -nostartfiles -e _start -o '{}' '{}' -lSystem",
            out.display(), src.display());
        let o = if cfg!(target_os = "macos") { Command::new("bash").arg("-lc").arg(&script).output() }
                else { Command::new("mac").arg("bash").arg("-lc").arg(&script).output() }
            .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() { return Err(format!("compile darwin/{source}: {}", String::from_utf8_lossy(&o.stderr).trim())); }
    }
    Ok(out.to_string_lossy().into_owned())
}
/// Compile a *portable* guest from `guests/<source>` as a normal macOS/arm64 Mach-O linked against the
/// full libSystem (real C runtime + main), cached under cache/darwin/. Runs natively under darwinjail —
/// so the same POSIX source that runs on the Linux engines also runs (un-emulated) on macOS.
fn compile_darwin_libc(ctx: &Ctx, source: &str) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx.cache.join("darwin").join(source.trim_end_matches(".c"));
    std::fs::create_dir_all(out.parent().unwrap()).ok();
    let fresh = std::fs::metadata(&out).and_then(|m| m.modified()).ok()
        >= std::fs::metadata(&src).and_then(|m| m.modified()).ok();
    if !out.exists() || !fresh {
        let script = format!("clang -arch arm64 -O2 -o '{}' '{}'", out.display(), src.display());
        let o = if cfg!(target_os = "macos") { Command::new("bash").arg("-lc").arg(&script).output() }
                else { Command::new("mac").arg("bash").arg("-lc").arg(&script).output() }
            .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() { return Err(format!("compile darwin(libc) {source}: {}", String::from_utf8_lossy(&o.stderr).trim())); }
    }
    Ok(out.to_string_lossy().into_owned())
}
fn resolve(ctx: &Ctx, p: &str) -> String {
    if p.starts_with('/') { p.into() } else { ctx.repo.parent().unwrap().join("poc").join(p).to_string_lossy().into_owned() }
}

/// Run one case on one engine and evaluate its checks.
pub fn run(ctx: &Ctx, c: &Case, e: Engine) -> Status {
    if !c.engines.contains(&e) { return Status::Skip("n/a for engine".into()); }
    if !e.available() { return Status::Skip(format!("{} JIT not built", e.label())); }
    let guest = match provision(ctx, c, e) {
        Ok(Some(g)) => g,
        Ok(None) => return Status::Skip(format!("no {} guest", e.label())),
        Err(err) => return Status::Fail(err),
    };
    // #231 scratch/distroless guard: synthesize an otherwise-EMPTY rootfs (just a `/tmp` landing dir for
    // the jailed guest copy below) — the FROM-scratch condition, with no shell/interpreter/libc on disk.
    // Self-contained (built under the cache tree, no poc image needed), so the loader/exec path is proven
    // to resolve + exec a static binary that is the sole executable in its rootfs.
    let rootfs = if c.scratch {
        let d = ctx.cache.join("scratchfs");
        if std::fs::create_dir_all(d.join("tmp")).is_err() { return Status::Skip("scratchfs create failed".into()); }
        Some(d.to_string_lossy().into_owned())
    } else {
        c.rootfs.and_then(|r| ctx.rootfs_path(r, e))
    };
    if c.rootfs.is_some() && rootfs.is_none() { return Status::Skip(format!("no {} rootfs", e.label())); }

    // A COMPILED guest + a rootfs on a Linux engine: the engine resolves argv[0] INSIDE the jail
    // (xresolve_overlay at startup), so a host path outside the rootfs can never load. Copy the built
    // guest into the image's /tmp under a unique name and run it by its in-guest path (removed after
    // the run; the fixture rootfs' /tmp is already scratch for the sh-based cases). Darwin keeps the
    // host path: darwinjail runs our own Mach-O natively and only arms the jail around it.
    let mut jail_copy: Option<(String, String)> = None; // (host file to clean up, in-guest argv[0])
    if let Some(rfs) = &rootfs {
        if !matches!(c.bin, Bin::InRootfs) && e != Engine::DarwinAarch64 {
            let leaf = format!("ddguest_{}_{}_{}", c.name.replace('/', "_"), e.arch(), std::process::id());
            let host = format!("{rfs}/tmp/{leaf}");
            match std::fs::copy(&guest, &host) {
                Ok(_) => {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755));
                    jail_copy = Some((host, format!("/tmp/{leaf}")));
                }
                Err(err) => return Status::Fail(format!("copy guest into rootfs: {err}")),
            }
        }
    }

    let rootfs_str = rootfs.unwrap_or_default();
    let mut cfg = ddjit::SpawnConfig::new(String::new(), rootfs_str.clone());
    cfg.lowers = c.lowers.clone();
    // .overlay(): inject the rootfs as its own lower so g_nlower>0 turns on the overlay open/lseek path
    // (linux engines only; darwin has no overlayfs). Reproduces overlay-only bugs like #391 in the matrix.
    if c.overlay && !rootfs_str.is_empty() && e != Engine::DarwinAarch64 { cfg.lowers.push(rootfs_str.clone()); }
    cfg.mem_max = c.mem_max;
    cfg.cpus = c.cpus;
    cfg.read_only = c.read_only;
    cfg.ulimits = c.ulimits.clone();
    // Untrusted-guest SENTRY split: bake DDJIT_UNTRUSTED=1 into the engine's launch env (via SpawnConfig's
    // `env`, which serializes into the `exec env …` prefix of the launch script — so it survives the `mac`
    // bridge that drops ambient env). DDJIT_SANDBOX is left unset on purpose (ring/forwarding, not Seatbelt).
    if c.untrusted { cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into())); }
    for (k, v) in &c.env { cfg.env.push((k.clone(), v.clone())); }
    let argv0 = jail_copy.as_ref().map(|(_, g)| g.clone()).unwrap_or_else(|| guest.clone());
    cfg.argv = match &c.bin {
        Bin::InRootfs => c.args.clone(),
        _ => std::iter::once(argv0).chain(c.args.iter().cloned()).collect(),
    };
    let (prog, args) = match cfg.command(e.jit()) { Some(x) => x, None => return Status::Skip("no command".into()) };

    // ── Reliable guest-stdout capture across the `mac` bridge ─────────────────────────────────────
    // On a Linux dev host the engine runs mac-side and its stdout is streamed back to this runner by
    // the OrbStack `mac` bridge. Under host load that bridge occasionally DROPS a guest's FINAL
    // buffered stdout write at teardown while STILL propagating the exit code — so an otherwise-correct
    // result (rc=0, right value or empty, never a *wrong* value) is truncated to empty and the case
    // spuriously fails. Seen on epoll_oneshot / pidfd / posixtimer / threadrss (#390). `.output()`
    // already drains the bridge's pipe to EOF, but the bytes were lost UPSTREAM in the bridge, so no
    // reader-side drain can recover them. Fix: redirect the guest's stdout into a file on the shared
    // repo tree (the SAME absolute path is visible to both the mac-side engine and this Linux runner,
    // Golden Rule 4) and read it back AFTER the process exits. A file write is durable — the final line
    // survives any bridge-teardown race. Proven: a minimal `mac` write dropped 2/800 through the pipe
    // under a mac-side CPU flood; the file redirect dropped 0/800 under a heavier flood. stderr stays
    // on the pipe (diagnostics only, never asserted) and the exit code is unchanged (still the guest's,
    // propagated by the bridge). On a real Mac there is no bridge and no race — the same file capture
    // is equally correct — so the path is unified (no per-guest fflush/usleep workaround needed).
    //
    // Darwin (darwinjail) shares this run()/redirect path — the `> file` below binds to the darwin launch
    // script exactly as it does for linux — but the DRAIN FILE LOCATION needs two darwin-only adjustments,
    // or the capture silently drops to empty on the darwin engine:
    //   (1) Seatbelt. The darwinjail arms a Seatbelt profile (DD_SANDBOX, only WHEN a rootfs is set) whose
    //       body is `(deny file-write* (subpath "/")) (allow file-write* (subpath "<rootfs>") …)` — writes
    //       outside the rootfs are DENIED. A drain under target/dd-tests/stdout/ (i.e. under /Users) is
    //       outside that set, so a rootfs darwin case's guest write to it is refused → empty file. (On a
    //       host that already confines the process, e.g. OrbStack, sandbox_init fails and this is masked —
    //       but on a real mac the deny is live.) Fix: for a rootfs darwin case, drain INTO the rootfs's
    //       /tmp — a Seatbelt-allowed subpath that is on the shared tree (so this Linux runner reads it
    //       back by the same host path) and stays writable even under docker --read-only.
    //   (2) Filename collision. darwin/aarch64 and linux/aarch64 share e.arch()=="aarch64", so the
    //       `{name}_{arch}_{pid}` file would be the SAME for both engines within one runner process — tag
    //       the darwin drain with the OS too so its file can never be clobbered by the same-arch linux run.
    // The linux drain path is left byte-identical (the `else` arm below).
    let drain_file = if e == Engine::DarwinAarch64 && !rootfs_str.is_empty() {
        // rootfs armed → the Seatbelt profile only permits writes under the rootfs; /tmp is rw even RO.
        PathBuf::from(&rootfs_str).join("tmp").join(format!("ddstdout_{}_{}_{}.out",
            c.name.replace('/', "_"), e.os(), std::process::id()))
    } else if e == Engine::DarwinAarch64 {
        // bare darwin → no Seatbelt; keep the shared drain dir but OS-tag the name (no arch collision).
        ctx.cache.join("stdout").join(format!("{}_{}_{}_{}.out",
            c.name.replace('/', "_"), e.os(), e.arch(), std::process::id()))
    } else {
        ctx.cache.join("stdout").join(format!("{}_{}_{}.out",
            c.name.replace('/', "_"), e.arch(), std::process::id()))
    };
    let mut args = args;
    let drained = std::fs::create_dir_all(drain_file.parent().unwrap()).is_ok();
    if drained {
        let _ = std::fs::remove_file(&drain_file);
        // The launch script is the last arg (`… bash -lc <script>`); appending a stdout redirect binds
        // it to the trailing `exec … argv` command, so the guest inherits fd 1 = this file. fd 1 stays
        // a NON-tty (isatty(1)==0 for both a pipe and a regular file), so guest behaviour is unchanged.
        if let Some(script) = args.last_mut() {
            *script = format!("{} > {}", script, shq(&drain_file.to_string_lossy()));
        }
    }

    // Wrap in `timeout` so a hung/looping guest can't block the matrix (the x86 JIT can mistranslate
    // into an infinite loop). 124 = timed out.
    let out = Command::new("timeout").arg("25").arg(&prog).args(&args).output();
    if let Some((host, _)) = &jail_copy { let _ = std::fs::remove_file(host); }
    let out = match out {
        Ok(o) => o,
        Err(err) => { let _ = std::fs::remove_file(&drain_file); return Status::Fail(format!("spawn: {err}")); }
    };
    // Recover the guest's stdout from the drained file (durable; immune to the bridge-teardown drop);
    // fall back to the bridge pipe only if the redirect could not be set up. Then remove the file.
    let stdout_bytes: Vec<u8> = if drained {
        let b = std::fs::read(&drain_file).unwrap_or_default();
        let _ = std::fs::remove_file(&drain_file);
        b
    } else {
        out.stdout.clone()
    };
    // a known failure on this engine is reported xfail, not a regression
    let fail = |msg: String| if c.xfail.contains(&e) { Status::Xfail(msg) } else { Status::Fail(msg) };
    if out.status.code() == Some(124) { return fail(format!("timeout (>25s) [{}]", e.label())); }
    if std::env::var("DD_DEBUG").is_ok() {
        eprintln!("\n[dbg] {} {:?}\n[dbg] out={:?}\n[dbg] err={:?}\n[dbg] code={:?}", prog, args,
            String::from_utf8_lossy(&stdout_bytes), String::from_utf8_lossy(&out.stderr), out.status.code());
    }

    let stdout = strip_noise(&stdout_bytes);
    let code = out.status.code().unwrap_or(-1);
    for chk in &c.checks {
        if let Err(msg) = eval(chk, &stdout, code, &guest, &c.args, e) {
            if std::env::var("CRASHDBG").is_ok() {
                eprintln!("[crashdbg {}] code={code} stderr={}", e.label(),
                    String::from_utf8_lossy(&out.stderr).trim());
            }
            return fail(msg);
        }
    }
    if c.xfail.contains(&e) { Status::Xpass } else { Status::Pass }
}

fn eval(chk: &Check, stdout: &str, code: i32, guest: &str, args: &[String], e: Engine) -> Result<(), String> {
    match chk {
        Check::Exit(want) => (code == *want).then_some(()).ok_or_else(|| format!("exit {code} != {want}")),
        Check::Out(want) => (stdout == *want).then_some(()).ok_or_else(|| format!("stdout {:?} != {:?}", stdout, want)),
        Check::OutHas(sub) => stdout.contains(sub).then_some(()).ok_or_else(|| format!("stdout {:?} lacks {:?}", stdout, sub)),
        Check::Oracle => {
            // native ground truth: aarch64 runs directly; x86_64 runs under qemu-user.
            let o = match e {
                Engine::LinuxX86_64 => Command::new("timeout").arg("25").arg("qemu-x86_64").arg(guest).args(args).output(),
                _ => Command::new("timeout").arg("25").arg(guest).args(args).output(),
            }.map_err(|err| format!("oracle spawn: {err}"))?;
            let (eo, ec) = (strip_noise(&o.stdout), o.status.code().unwrap_or(-1));
            if eo != stdout || ec != code { Err(format!("oracle mismatch (jit {code}/{stdout:?} vs native {ec}/{eo:?})")) } else { Ok(()) }
        }
    }
}

/// Single-quote a string for safe inclusion in the mac-side `bash -lc` launch script (used to append
/// the stdout-drain redirect target). Mirrors `SpawnConfig::shq`.
fn shq(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('\'');
    for c in s.chars() { if c == '\'' { o.push_str("'\\''"); } else { o.push(c); } }
    o.push('\'');
    o
}

/// Drop the JIT's diagnostic "unhandled syscall ..." lines so they don't pollute stdout checks.
fn strip_noise(b: &[u8]) -> String {
    String::from_utf8_lossy(b).lines().filter(|l| !l.contains("unhandled syscall")).collect::<Vec<_>>().join("\n")
        + if b.ends_with(b"\n") && !b.is_empty() { "\n" } else { "" }
}

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
    for _ in 0..n { let t = Instant::now(); f(); v.push(t.elapsed().as_millis()); }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Rebuild the exact JIT launch command for a case+engine (guest already provisioned/compiled), so the
/// perf loop can time `execution` without paying compilation again. Mirrors the setup in [`run`].
/// Returns `(guest_path, (program, args))`, or `None` if the cell has no runnable command (skip).
fn perf_cmd(ctx: &Ctx, c: &Case, e: Engine) -> Option<(String, (String, Vec<String>))> {
    let guest = match provision(ctx, c, e) { Ok(Some(g)) => g, _ => return None };
    let rootfs = c.rootfs.and_then(|r| ctx.rootfs_path(r, e));
    if c.rootfs.is_some() && rootfs.is_none() { return None; }
    let rootfs_str = rootfs.unwrap_or_default();
    let mut cfg = ddjit::SpawnConfig::new(String::new(), rootfs_str.clone());
    cfg.lowers = c.lowers.clone();
    // .overlay(): inject the rootfs as its own lower so g_nlower>0 turns on the overlay open/lseek path
    // (linux engines only; darwin has no overlayfs). Reproduces overlay-only bugs like #391 in the matrix.
    if c.overlay && !rootfs_str.is_empty() && e != Engine::DarwinAarch64 { cfg.lowers.push(rootfs_str); }
    cfg.mem_max = c.mem_max;
    cfg.cpus = c.cpus;
    cfg.read_only = c.read_only;
    cfg.ulimits = c.ulimits.clone();
    if c.untrusted { cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into())); }
    for (k, v) in &c.env { cfg.env.push((k.clone(), v.clone())); }
    cfg.argv = match &c.bin {
        Bin::InRootfs => c.args.clone(),
        _ => std::iter::once(guest.clone()).chain(c.args.iter().cloned()).collect(),
    };
    let cmd = cfg.command(e.jit())?;
    Some((guest, cmd))
}

/// The native ground-truth command for a guest (mirrors the `Oracle` branch of [`eval`]):
/// aarch64 runs directly, x86_64 under qemu-user. Program is always `timeout` (hang guard).
fn oracle_cmd(guest: &str, args: &[String], e: Engine) -> (String, Vec<String>) {
    let mut a: Vec<String> = vec!["25".into()];
    if e == Engine::LinuxX86_64 { a.push("qemu-x86_64".into()); }
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
        return Timed { status, jit_ms: None, oracle_ms: None, has_oracle: false };
    }
    let (guest, jit) = match perf_cmd(ctx, c, e) {
        Some(x) => x,
        None => return Timed { status, jit_ms: None, oracle_ms: None, has_oracle: false },
    };
    let jit_ms = median_ms(n, || { let _ = Command::new(&jit.0).args(&jit.1).output(); });
    let has_oracle = c.checks.iter().any(|k| matches!(k, Check::Oracle));
    let oracle_ms = has_oracle.then(|| {
        let (op, oa) = oracle_cmd(&guest, &c.args, e);
        median_ms(n, || { let _ = Command::new(&op).args(&oa).output(); })
    });
    Timed { status, jit_ms: Some(jit_ms), oracle_ms, has_oracle }
}
