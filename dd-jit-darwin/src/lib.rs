//! # ddjit — the dd VM-less JIT container runtime + its bindings.
//!
//! The JIT runs a Linux container by translating its code and servicing its syscalls in userspace — no
//! VM. The C runtime (`src/runtime/`) is compiled by `build.rs` into one codesigned binary per guest
//! architecture (`aarch64`, `x86_64`); this crate exposes those binaries plus the typed
//! [`SpawnConfig`] launch contract and the [`spawn_io`]/[`LaunchConfig`] FFI entry the runtime forks
//! through. `dd-daemon` (and any other front-end) depends on this crate via the `dd-jit` API layer.

use std::path::Path;

mod launch;
pub use launch::{spawn, spawn_io, LaunchConfig, SpawnIo};

/// A guest target = (OS personality, ISA) the JIT can run. Each maps to one binary built by `build.rs`
/// from `targets/<target>.c`. The OS axis is `linux` (jit / jit86) or `darwin` (jitdarwin — native
/// macOS Mach-O containers); the ISA axis is `aarch64` or `x86_64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Guest {
    #[default]
    LinuxAarch64,
    LinuxX86_64,
    DarwinAarch64,
}

impl Guest {
    pub const ALL: [Guest; 3] = [
        Guest::LinuxAarch64,
        Guest::LinuxX86_64,
        Guest::DarwinAarch64,
    ];

    /// Guest OS personality: `"linux"` or `"darwin"`.
    pub fn os(self) -> &'static str {
        match self {
            Guest::DarwinAarch64 => "darwin",
            _ => "linux",
        }
    }
    /// Guest instruction set: `"aarch64"` or `"x86_64"`.
    pub fn arch(self) -> &'static str {
        match self {
            Guest::LinuxX86_64 => "x86_64",
            _ => "aarch64",
        }
    }
    /// The build-target name, matching `build.rs` and `targets/<target>.c`.
    pub fn target(self) -> &'static str {
        match self {
            Guest::LinuxAarch64 => "linux_aarch64",
            Guest::LinuxX86_64 => "linux_x86_64",
            Guest::DarwinAarch64 => "darwin_aarch64",
        }
    }

    /// Pick a target from an OS + arch (e.g. detected from a binary's magic / an image's metadata).
    pub fn detect(os: &str, arch: &str) -> Option<Guest> {
        match (os, arch.to_ascii_lowercase().as_str()) {
            ("linux", "aarch64" | "arm64" | "arm64/v8") => Some(Guest::LinuxAarch64),
            ("linux", "x86_64" | "amd64" | "x86-64") => Some(Guest::LinuxX86_64),
            ("darwin", "aarch64" | "arm64") => Some(Guest::DarwinAarch64),
            _ => None,
        }
    }

    /// Absolute path to the JIT binary, or `None` if it can't be located.
    ///
    /// Resolution order (see [`resolve_bundled`]): `$DDJIT_DIR/ddjit-<target>`, then beside the running
    /// executable, then the `build.rs`-baked compile-time path (dev/`cargo` layout). `None` if none exist.
    pub fn jit_path(self) -> Option<String> {
        resolve_bundled(
            &format!("ddjit-{}", self.target()),
            match self {
                Guest::LinuxAarch64 => env!("DDJIT_LINUX_AARCH64"),
                Guest::LinuxX86_64 => env!("DDJIT_LINUX_X86_64"),
                Guest::DarwinAarch64 => env!("DDJIT_DARWIN_AARCH64"),
            },
        )
    }

    /// Path to the darwinjail interposing dylib (DYLD_INSERT) that runs native macOS arm64 binaries in a
    /// container. Resolved the same way as the engines (see `resolve_bundled`). `None` if absent.
    pub fn jail_dylib(&self) -> Option<String> {
        resolve_bundled("darwinjail.dylib", env!("DDJAIL_DARWIN_AARCH64"))
    }
}

/// Resolve a shipped artifact (a `ddjit-<target>` engine or `darwinjail.dylib`) at runtime. The backend
/// knows nothing about any particular deployment (GUI app, LaunchAgent, install location) — an embedder
/// that relocates the engines just points `$DDJIT_DIR` at them. Resolution order:
///   1. `$DDJIT_DIR/<name>` — an explicit override for a relocated/bundled layout,
///   2. `<dir of the running executable>/<name>` — engines sitting beside the host binary (the common
///      packaged layout), so a plain run works with no env set,
///   3. the `build.rs`-baked compile-time path (cargo/dev layout).
/// Every candidate is existence-checked, so a stale baked CI path is never returned.
fn resolve_bundled(name: &str, baked: &str) -> Option<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("DDJIT_DIR") {
        dirs.push(std::path::PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
        }
    }
    for d in dirs {
        let cand = d.join(name);
        if cand.exists() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    if !baked.is_empty() && Path::new(baked).exists() {
        Some(baked.to_string())
    } else {
        None
    }
}

/// A published port: the host port forwards to the container port (`docker -p HOST:CONTAINER`).
#[derive(Clone, Debug)]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
}

/// A bind mount: a host directory mounted at a path inside the container (`docker -v HOST:CONTAINER`).
/// `ro` marks the mount read-only (`-v HOST:CONTAINER:ro`): the JIT then fails write-intent syscalls
/// under `container` with EROFS. Default `false` = read-write (the normal `-v src:dst`).
#[derive(Clone, Debug)]
pub struct Volume {
    pub container: String,
    pub host: String,
    pub ro: bool,
}

/// Everything needed to launch one container in the JIT. Mirrors the JIT's flag/env contract:
/// `--rootfs/--lower/--hostname/--mem-max/--pids-max/--uid/--gid/--publish` + `DDVOL`/`DD_NETNS` env.
#[derive(Clone, Debug, Default)]
pub struct SpawnConfig {
    /// Working directory on the host (where relative image/rootfs paths resolve).
    pub work_dir: String,
    /// The writable rootfs (the overlay UPPER, or a plain single rootfs).
    pub rootfs: String,
    /// Read-only overlay lower layers, highest-priority first (the OCI image layers).
    pub lowers: Vec<String>,
    /// Bind-mounted volumes.
    pub volumes: Vec<Volume>,
    /// Published ports (`-p`).
    pub publish: Vec<PortMap>,
    /// Private-loopback network namespace id (isolates 127.0.0.0/8). `None` = shared.
    pub netns: Option<String>,
    /// UTS hostname.
    pub hostname: Option<String>,
    /// cgroup `memory.max` in bytes (0 = unlimited).
    pub mem_max: u64,
    /// cgroup `pids.max` (0 = unlimited).
    pub pids_max: u32,
    /// docker `--cpus`: online-CPU count the container advertises = ceil(NanoCpus/1e9). 0 = unlimited.
    pub cpus: u32,
    /// docker `--read-only`: writes to the rootfs/overlay-upper fail EROFS (/proc /dev /sys /tmp /run stay rw).
    pub read_only: bool,
    /// docker `--ulimit`: (name, soft, hard) triples, e.g. ("nofile", 1024, 2048). Serialized to DD_ULIMITS.
    pub ulimits: Vec<(String, u64, u64)>,
    /// USER-ns uid / gid (default: root = 0).
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Extra environment for the guest process.
    pub env: Vec<(String, String)>,
    /// The guest argv (entrypoint + args).
    pub argv: Vec<String>,
}

impl SpawnConfig {
    pub fn new(work_dir: impl Into<String>, rootfs: impl Into<String>) -> Self {
        SpawnConfig {
            work_dir: work_dir.into(),
            rootfs: rootfs.into(),
            ..Default::default()
        }
    }
}

/// True if the JIT binary for `guest` was built and exists.
pub fn available(guest: Guest) -> bool {
    guest
        .jit_path()
        .map(|p| Path::new(&p).exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_detect() {
        assert_eq!(Guest::detect("linux", "amd64"), Some(Guest::LinuxX86_64));
        assert_eq!(Guest::detect("linux", "arm64"), Some(Guest::LinuxAarch64));
        assert_eq!(
            Guest::detect("darwin", "aarch64"),
            Some(Guest::DarwinAarch64)
        );
        assert_eq!(Guest::detect("plan9", "aarch64"), None);
        assert_eq!(Guest::DarwinAarch64.os(), "darwin");
        assert_eq!(Guest::LinuxX86_64.arch(), "x86_64");
    }
}
