//! Daemon-write coherence (the docker-cp epoch blind spot).
//!
//! The engine's path/metadata caches (dd-jit fscache.c) are invalidated by GUEST syscalls, but the daemon
//! also writes into a LIVE container's filesystem from outside any engine — `docker cp`
//! (PUT /containers/{id}/archive) and the exec-spawn /etc/{hosts,resolv.conf,hostname} rewrites — which no
//! guest syscall announces, so a cached ENOENT could hide a file docker-cp just delivered. The contract
//! with the engine: a 4-byte native-endian u32 "external-writer generation" file per container at
//! `<dd_home>/containers/<cid>/fsgen`. spawn_cfg hands its path to every engine of the container
//! (DD_FSGEN_FILE — run, exec and health probes share one file, keyed like tmpfs by the target container
//! id); each engine maps it MAP_SHARED and polls it once per guest syscall, dropping ALL its caches when
//! it moves. The daemon calls [`fsgen_bump`] AFTER completing any such write, making the write visible to
//! the guest no later than its next syscall (kernel-dcache semantics, like real Docker on Linux).
//!
//! Kept together as a unit: this module will later move to dd-jit wholesale.
use super::*;

/// The container's external-writer generation file (see the module comment above).
pub(crate) fn fsgen_path(cid: &str) -> PathBuf {
    dd_home().join("containers").join(cid).join("fsgen")
}

/// Create the generation file (value 1) if it doesn't exist yet; returns its path. Called before any
/// engine of the container spawns (spawn_cfg) and defensively by [`fsgen_bump`]. Best-effort.
pub(crate) fn fsgen_ensure(cid: &str) -> PathBuf {
    let p = fsgen_path(cid);
    if !p.exists() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, 1u32.to_ne_bytes());
    }
    p
}

/// Atomically increment the container's external-writer generation — call AFTER a daemon-side write into
/// the container's filesystem completes. mmap + atomic fetch_add matches the engine's read side (same
/// 32-bit width; Release pairs with the engine's acquire load so the flush orders after our file writes).
/// Best-effort: a failure only means the engine re-learns the write through its normal (slower) paths.
pub(crate) fn fsgen_bump(cid: &str) {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU32, Ordering};
    let p = fsgen_ensure(cid);
    let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&p) else {
        return;
    };
    if f.metadata().map(|m| m.len()).unwrap_or(0) < 4 && f.set_len(4).is_err() {
        return;
    }
    unsafe {
        let m = libc::mmap(
            std::ptr::null_mut(),
            4,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            f.as_raw_fd(),
            0,
        );
        if m == libc::MAP_FAILED {
            return;
        }
        (*(m as *const AtomicU32)).fetch_add(1, Ordering::Release);
        libc::munmap(m, 4);
    }
}
