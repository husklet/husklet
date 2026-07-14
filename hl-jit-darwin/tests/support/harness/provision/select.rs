//! Image-rootfs arch/name selection: match a requested image name to the right prebuilt rootfs dir for
//! an engine's guest ISA. Used by `Ctx::rootfs_path` to reject arch-mismatched rootfses and prefer the
//! strongest (real pulled > sidecar-named > literal-dir > substring) match.
use super::*;

/// EM_* value (ELF `e_machine`) for an engine's guest ISA.
pub(super) fn elf_machine(e: Engine) -> u16 {
    match e.arch() {
        "x86_64" => 0x3E, // EM_X86_64
        _ => 0xB7,        // EM_AARCH64
    }
}

/// Read the ELF `e_machine` of a rootfs by probing a few common executables; `None` if undeterminable.
pub(super) fn rootfs_machine(rootfs: &Path) -> Option<u16> {
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
///   1 — the `name`/`repo` recorded in the dir's `hl-image.json` sidecar matches (non-registry dir).
///   2 — the dir is literally named `name` (hand-built bundle dirs like `gcc-bundle`).
///   `None` — no exact match (the caller may still fall back to a substring match).
pub(super) fn image_name_tier(dir: &Path, dname: &str, name: &str) -> Option<u8> {
    // Decode the docker store-dir encoding for a Hub `library/` image and match its repo. The store now
    // percent-encodes refs (`docker.io%2Flibrary%2Falpine%3Alatest`); older dirs used a `_`-flattened
    // form (`docker.io_library_alpine_latest`). Accept both so tier-0 still matches real pulled images.
    let decoded_repo = dname
        .strip_prefix("docker.io%2Flibrary%2F")
        .and_then(|rest| rest.split("%3A").next()) // repo before the `:`-tag separator
        .or_else(|| {
            dname
                .strip_prefix("docker.io_library_")
                .and_then(|rest| rest.rsplit_once('_').map(|(repo, _tag)| repo))
        });
    if decoded_repo == Some(name) {
        return Some(0);
    }
    if let Ok(json) = std::fs::read_to_string(dir.join("hl-image.json")) {
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
