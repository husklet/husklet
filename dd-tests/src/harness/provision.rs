use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Shared paths/config for a run.
pub struct Ctx {
    pub repo: PathBuf,   // dd repo root (shared mount, visible to the mac-side JIT)
    pub guests: PathBuf, // dd-tests/guests
    pub cache: PathBuf,  // compiled-guest cache (under target/, shared)
    pub images: PathBuf, // image rootfs dir (default the poc images)
}

impl Ctx {
    pub fn discover() -> Ctx {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let cache = repo.join("target/dd-tests");
        std::fs::create_dir_all(cache.join("aarch64")).ok();
        Ctx {
            guests: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("guests"),
            images: std::env::var("DD_IMAGES")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/Users/x/dd/poc/images")),
            cache,
            repo,
        }
    }
    /// Resolve an image name to its rootfs path for the engine's guest arch.
    ///
    /// The image dirs hold prebuilt rootfses for different guest arches (e.g. an aarch64 `alpine` and an
    /// x86-64 `nginx_alpine`), so a naive substring match could hand an x86-64 rootfs to the aarch64
    /// engine — the guest would then exit 255 with empty output. We therefore (1) require the rootfs's
    /// ELF arch to match `e.arch()`, and (2) prefer an EXACT image-name match over a mere substring one.
    /// Among equal candidates the lowest sorted dir name wins, so selection is deterministic.
    pub(crate) fn rootfs_path(&self, name: &str, e: Engine) -> Option<String> {
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
            if !rootfs.is_dir() {
                continue;
            }
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
            if best.as_ref().map_or(true, |b| key < *b) {
                best = Some(key);
            }
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
    for cand in [
        "bin/busybox",
        "bin/dash",
        "bin/bash",
        "bin/ls",
        "bin/cat",
        "bin/sh",
    ] {
        let p = rootfs.join(cand);
        // Skip symlinks (e.g. sh -> dash): resolve only plain ELF files to keep this host-path safe.
        if p.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
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
            if repo == name {
                return Some(0);
            }
        }
    }
    if let Ok(json) = std::fs::read_to_string(dir.join("dd-image.json")) {
        if let Some(img) = json
            .split("\"name\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
        {
            // img is "repo:tag" (or "ns/repo:tag"); accept full match or the repo before ':'.
            if img == name || img.split(':').next() == Some(name) {
                return Some(1);
            }
        }
    }
    if dname == name {
        return Some(2);
    }
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
        let o = Command::new(cc)
            .args(["-O2", "-static-pie", "-pthread"])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .args(libs)
            .output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile {source} [{}]: {}",
                e.arch(),
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Compile a guest STATIC NON-PIE (`-static -no-pie` → ET_EXEC), so the loader biases it high and turns
/// on dispatch.c's non-PIE pointer-arg rebase (`g_nonpie_lo`). Cached under cache/<arch>/nopie/ so it
/// never collides with the same source's static-PIE build. Same native/qemu oracle as `compile`.
fn compile_nopie(ctx: &Ctx, source: &str, e: Engine) -> Result<String, String> {
    let src = ctx.guests.join(source);
    let out = ctx
        .cache
        .join(e.arch())
        .join("nopie")
        .join(source.trim_end_matches(".c"));
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
        let o = Command::new(cc)
            .args(["-O2", "-static", "-no-pie", "-pthread"])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .args(libs)
            .output()
            .map_err(|err| format!("{cc} spawn: {err}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile-nopie {source} [{}]: {}",
                e.arch(),
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Provision the guest binary path for a case on an engine. `Ok(None)` = skip (no guest for this arch).
pub(crate) fn provision(ctx: &Ctx, c: &Case, e: Engine) -> Result<Option<String>, String> {
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
        Bin::Fixture(fx) => Ok(fx
            .iter()
            .find(|(fe, _)| *fe == e)
            .map(|(_, p)| resolve(ctx, p))),
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
        let script = format!(
            "clang -arch arm64 -nostartfiles -e _start -o '{}' '{}' -lSystem",
            out.display(),
            src.display()
        );
        let o = if cfg!(target_os = "macos") {
            Command::new("bash").arg("-lc").arg(&script).output()
        } else {
            Command::new("mac")
                .arg("bash")
                .arg("-lc")
                .arg(&script)
                .output()
        }
        .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile darwin/{source}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
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
        let script = format!(
            "clang -arch arm64 -O2 -o '{}' '{}'",
            out.display(),
            src.display()
        );
        let o = if cfg!(target_os = "macos") {
            Command::new("bash").arg("-lc").arg(&script).output()
        } else {
            Command::new("mac")
                .arg("bash")
                .arg("-lc")
                .arg(&script)
                .output()
        }
        .map_err(|e| format!("mac clang spawn: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "compile darwin(libc) {source}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
    }
    Ok(out.to_string_lossy().into_owned())
}
fn resolve(ctx: &Ctx, p: &str) -> String {
    if p.starts_with('/') {
        p.into()
    } else {
        ctx.repo
            .parent()
            .unwrap()
            .join("poc")
            .join(p)
            .to_string_lossy()
            .into_owned()
    }
}
