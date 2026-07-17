//! Arch detection from binary magic: classify ELF / Mach-O headers, probe well-known paths, and fall
//! back to a bounded scan of the rootfs.

use super::*;
use std::collections::VecDeque;
use std::path::Path;

/// Classify a binary by its leading magic bytes: ELF -> linux (e_machine = aarch64/x86_64).
/// Returns `None` for anything else (scripts, data, an unrecognized machine).
fn sniff_magic(b: &[u8]) -> Option<Arch> {
    if b.len() > 19 && &b[0..4] == b"\x7fELF" {
        return match u16::from_le_bytes([b[18], b[19]]) {
            // ELF e_machine
            0xB7 => Some(Arch::LinuxAarch64),
            0x3E => Some(Arch::LinuxX86_64),
            _ => None,
        };
    }
    None
}

/// Read just the header of `p` (following symlinks) and classify its magic. Cheap: only the first 20
/// bytes are read, never the whole binary.
fn sniff_path(p: &Path) -> Option<Arch> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 20];
    let n = f.read(&mut buf).ok()?;
    sniff_magic(&buf[..n])
}

/// Fallback arch probe: a bounded breadth-first scan of the rootfs for the first binary whose magic
/// identifies a target. Catches images that ship a single executable at a non-standard path
/// (hello-world's `/hello`, nats's `/nats-server`) which the fixed probe list in [`detect_arch`] misses.
/// Shallow entries are examined first (top-level binaries win immediately) and the total entry budget is
/// capped so a large rootfs can never make discovery pathological. Symlinked directories are not
/// descended (avoids cycles); symlinked *files* are still classified (their target is read).
fn scan_for_binary(rootfs: &Path) -> Option<Arch> {
    let mut queue = VecDeque::from([rootfs.to_path_buf()]);
    let mut budget = 4096; // cap on entries examined across the whole walk
    while let Some(dir) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            match e.file_type() {
                Ok(ft) if ft.is_dir() => queue.push_back(e.path()),
                Ok(ft) if ft.is_file() || ft.is_symlink() => {
                    if let Some(g) = sniff_path(&e.path()) {
                        return Some(g);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Probe the rootfs and pick the target [`Arch`] from a binary's magic. Tries a handful of well-known
/// executable locations first (fast path), then falls back to a bounded scan of the whole rootfs so an
/// image with its binary at a non-standard path is still detected.
pub fn detect_arch(rootfs: &Path) -> Option<Arch> {
    // Includes darwin-userland paths (`profile/bin/*`, `opt/homebrew/bin/*`) so a *pulled* macOS image
    // — whose `hl-image.json` sidecar didn't survive the registry round-trip — is still detected as
    // darwin from its packed Mach-O binaries. `sniff_path` follows the profile symlinks to the real
    // Mach-O in the packed `/nix` (or Homebrew) closure.
    for probe in [
        "bin/busybox",
        "bin/sh",
        "bin/true",
        "usr/bin/coreutils",
        "usr/lib/dyld",
        "profile/bin/bash",
        "profile/bin/sh",
        "opt/homebrew/bin/brew",
    ] {
        if let Some(g) = sniff_path(&rootfs.join(probe)) {
            return Some(g);
        }
    }
    scan_for_binary(rootfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- sniff_magic: classify a binary by its leading magic bytes ----

    /// A 20-byte ELF header with `e_machine` (offset 18, little-endian u16) set to `machine`.
    fn elf_header(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 20];
        b[0..4].copy_from_slice(b"\x7fELF");
        let m = machine.to_le_bytes();
        b[18] = m[0];
        b[19] = m[1];
        b
    }

    /// An 8-byte Mach-O 64 header (MH_MAGIC_64) with `cputype` (offset 4, little-endian u32).
    fn macho_header(cputype: u32) -> Vec<u8> {
        let mut b = vec![0xCFu8, 0xFA, 0xED, 0xFE];
        b.extend_from_slice(&cputype.to_le_bytes());
        b
    }

    #[test]
    fn sniff_magic_elf_machines() {
        assert_eq!(sniff_magic(&elf_header(0xB7)), Some(Arch::LinuxAarch64)); // EM_AARCH64
        assert_eq!(sniff_magic(&elf_header(0x3E)), Some(Arch::LinuxX86_64)); // EM_X86_64
                                                                             // an ELF with an unrecognized machine (e.g. EM_MIPS 0x08) -> None
        assert_eq!(sniff_magic(&elf_header(0x08)), None);
    }

    #[test]
    fn sniff_magic_macho_unsupported() {
        // Mach-O images are no longer a supported target -> always None.
        assert_eq!(sniff_magic(&macho_header(0x0100000C)), None); // CPU_TYPE_ARM64
        assert_eq!(sniff_magic(&macho_header(0x01000007)), None); // CPU_TYPE_X86_64
    }

    #[test]
    fn sniff_magic_non_binaries_and_too_short() {
        assert_eq!(sniff_magic(b""), None);
        assert_eq!(sniff_magic(b"#!/bin/sh\n"), None); // a script, not ELF/Mach-O
                                                       // ELF magic but truncated below the e_machine offset (needs len > 19) -> None
        assert_eq!(sniff_magic(b"\x7fELF\x02\x01\x01"), None);
        // Mach-O magic but truncated below the cputype offset (needs len > 7) -> None
        assert_eq!(sniff_magic(&[0xCFu8, 0xFA, 0xED, 0xFE, 0x0C]), None);
    }
}
