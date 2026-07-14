//! Guest provisioning: turn a `Case`'s `Bin` into a runnable binary path for an engine — compiling C
//! sources on demand (Linux via gcc/cross-gcc) or resolving a prebuilt fixture / in-rootfs argv.
//! Also owns `Ctx` (shared run paths) and image-rootfs selection.
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

mod compile;
mod select;

use compile::{compile, compile_nopie};
use select::{elf_machine, image_name_tier, rootfs_machine};

/// Shared paths/config for a run.
pub struct Ctx {
    pub repo: PathBuf,   // dd repo root (shared mount, visible to the mac-side JIT)
    pub guests: PathBuf, // hl-jit-darwin/testdata/guests (the JIT engine owns the C guest corpus)
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
            // The C guest corpus is owned by the engine crate (ownership-matrix Step 2), so it now
            // lives at hl-jit-darwin/testdata/guests, resolved from the shared repo root rather than
            // this helper crate's manifest dir.
            guests: repo.join("hl-jit-darwin/testdata/guests"),
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

/// Provision the guest binary path for a case on an engine. `Ok(None)` = skip (no guest for this arch).
pub(crate) fn provision(ctx: &Ctx, c: &Case, e: Engine) -> Result<Option<String>, String> {
    match &c.bin {
        Bin::Source(s) if e.can_compile() => compile(ctx, s, e).map(Some),
        Bin::Source(_) => Ok(None),
        Bin::SourceNoPie(s) if e.can_compile() => compile_nopie(ctx, s, e).map(Some),
        Bin::SourceNoPie(_) => Ok(None),
        // portable POSIX: Linux engines via gcc (same as Source).
        Bin::Portable(s) if e.can_compile() => compile(ctx, s, e).map(Some),
        Bin::Portable(_) => Ok(None),
        Bin::Fixture(fx) => Ok(fx
            .iter()
            .find(|(fe, _)| *fe == e)
            .map(|(_, p)| resolve(ctx, p))),
        Bin::InRootfs => Ok(Some(String::new())), // nothing to build; argv[0] is in-rootfs
    }
}

/// Resolve a prebuilt-fixture path. Absolute paths pass through. A relative path (e.g.
/// `guests/arm/go_cgo_stackgrow_arm`) is tried IN-REPO first — `hl-jit-darwin/testdata/<p>`, so a fixture committed
/// next to the compiled-guest sources is found from any checkout, including a `.claude/worktrees/*`
/// worktree — then against a `poc/` sidecar dir walking UP from the repo root (the historical layout,
/// `<repo-parent>/poc/<p>`; the ancestor walk makes it work from a worktree too, whose parent is
/// `.claude/worktrees/`, not the dir that holds `poc/`). If nothing exists, fall back to the historical
/// `<repo-parent>/poc/<p>` so the runner's error still names the conventional location.
fn resolve(ctx: &Ctx, p: &str) -> String {
    if p.starts_with('/') {
        return p.into();
    }
    let in_repo = ctx.repo.join("hl-jit-darwin/testdata").join(p);
    if in_repo.is_file() {
        return in_repo.to_string_lossy().into_owned();
    }
    let mut dir = Some(ctx.repo.as_path());
    while let Some(d) = dir {
        let cand = d.join("poc").join(p);
        if cand.is_file() {
            return cand.to_string_lossy().into_owned();
        }
        dir = d.parent();
    }
    ctx.repo
        .parent()
        .unwrap()
        .join("poc")
        .join(p)
        .to_string_lossy()
        .into_owned()
}
