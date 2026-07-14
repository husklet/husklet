use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The guest ISA a target runs. Linux/arm is aarch64; amd is x86_64.
pub(super) fn target_arch(t: Target) -> &'static str {
    match t {
        Target::ArmLinux => "aarch64",
        Target::AmdLinux => "x86_64",
    }
}

/// SINGLE-ARCH STORE: the local `poc/images` store holds each image ref at only ONE architecture (it is
/// a per-ref rootfs, not a multi-arch manifest). When a scenario's target arch differs from the arch the
/// store actually holds, the Dd daemon would serve the WRONG-arch rootfs (e.g. an aarch64 mongo image
/// for an `amd-linux` cell), which the engine then runs under the *other* arch's JIT — a false gap, not a
/// real one. We read the arch the store holds from the image's `dd-image.json` sidecar and, for the Dd
/// backend, cleanly SKIP the cell whose arch the store can't serve. The Real oracle pulls the correct
/// arch per `--platform`, so it is never skipped. Memoized per image; `None` (no sidecar / no `arch`
/// field, i.e. arch unknown) means "don't skip" — we only skip on a PROVEN mismatch.
pub(super) fn store_arch(cfg: &Cfg, image: &str) -> Option<String> {
    static C: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(image) {
        return v.clone();
    }
    let mut found = None;
    let want = format!("\"name\":\"{image}\"");
    if let Ok(rd) = std::fs::read_dir(&cfg.images) {
        for e in rd.flatten() {
            let dir = e.path();
            let Ok(txt) = std::fs::read_to_string(dir.join("dd-image.json")) else {
                continue;
            };
            if !txt.contains(&want) {
                continue;
            }
            // Prefer the recorded arch; fall back to sniffing a rootfs ELF's e_machine when the sidecar
            // has none (many older pre-seeded fixtures dropped it) so the single-arch skip still fires.
            if let Some(i) = txt.find("\"arch\":\"") {
                let rest = &txt[i + 8..];
                if let Some(j) = rest.find('"') {
                    found = Some(rest[..j].to_string());
                }
            }
            if found.is_none() {
                found = sniff_rootfs_arch(&dir.join("rootfs")).map(String::from);
            }
            break;
        }
    }
    cache
        .lock()
        .unwrap()
        .insert(image.to_string(), found.clone());
    found
}

/// Read an ELF file's `e_machine` (little-endian, offset 18): 0x3e = x86_64, 0xb7 = aarch64.
fn elf_arch(path: &std::path::Path) -> Option<&'static str> {
    use std::io::Read;
    let mut buf = [0u8; 20];
    std::fs::File::open(path).ok()?.read_exact(&mut buf).ok()?;
    if &buf[0..4] != b"\x7fELF" {
        return None;
    }
    match u16::from_le_bytes([buf[18], buf[19]]) {
        0x3e => Some("x86_64"),
        0xb7 => Some("aarch64"),
        _ => None,
    }
}

/// Probe a rootfs for the arch of its binaries — a single-arch store's rootfs is uniformly one ISA.
/// Try well-known shell/coreutil paths first (covers every alpine/glibc image), then the first regular
/// file under `bin`/`usr/bin`. Scratch/distroless images with no reachable ELF return `None` (no skip).
fn sniff_rootfs_arch(rootfs: &std::path::Path) -> Option<&'static str> {
    const CAND: &[&str] = &[
        "bin/busybox",
        "bin/sh",
        "bin/dash",
        "bin/bash",
        "usr/bin/env",
        "bin/cat",
        "bin/ls",
        "usr/bin/coreutils",
    ];
    for c in CAND {
        if let Some(a) = elf_arch(&rootfs.join(c)) {
            return Some(a);
        }
    }
    for d in ["bin", "usr/bin"] {
        if let Ok(rd) = std::fs::read_dir(rootfs.join(d)) {
            for e in rd.flatten().take(40) {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    if let Some(a) = elf_arch(&e.path()) {
                        return Some(a);
                    }
                }
            }
        }
    }
    None
}
