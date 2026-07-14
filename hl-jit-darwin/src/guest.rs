use std::path::Path;

/// A guest target = (OS personality, ISA) the JIT can run. Each maps to one binary built by `build.rs`
/// from `targets/<target>.c`. The OS axis is `linux` (jit / jit86); the ISA axis is `aarch64` or
/// `x86_64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Guest {
    /// Linux on ARM64 — same-arch, run by the native `jit` engine. The default.
    #[default]
    LinuxAarch64,
    /// Linux on x86-64 — translated to ARM64 by the `jit86` engine.
    LinuxX86_64,
}

impl Guest {
    /// Every guest target, for iterating over or probing which engines were built.
    pub const ALL: [Guest; 2] = [Guest::LinuxAarch64, Guest::LinuxX86_64];

    /// Guest OS personality: `"linux"`.
    pub fn os(self) -> &'static str {
        "linux"
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
        }
    }

    /// Pick a target from an OS + arch (e.g. detected from a binary's magic / an image's metadata).
    pub fn detect(os: &str, arch: &str) -> Option<Guest> {
        match (os, arch.to_ascii_lowercase().as_str()) {
            ("linux", "aarch64" | "arm64" | "arm64/v8") => Some(Guest::LinuxAarch64),
            ("linux", "x86_64" | "amd64" | "x86-64") => Some(Guest::LinuxX86_64),
            _ => None,
        }
    }

    /// Absolute path to the JIT binary, or `None` if it can't be located.
    ///
    /// Resolution order (see [`resolve_bundled`]): `$HL_JIT_DIR/ddjit-<target>`, then beside the running
    /// executable, then the `build.rs`-baked compile-time path (dev/`cargo` layout). `None` if none exist.
    pub fn jit_path(self) -> Option<String> {
        resolve_bundled(
            &format!("ddjit-{}", self.target()),
            match self {
                Guest::LinuxAarch64 => env!("HL_JIT_LINUX_AARCH64"),
                Guest::LinuxX86_64 => env!("HL_JIT_LINUX_X86_64"),
            },
        )
    }
}

/// Resolve a shipped artifact (a `ddjit-<target>` engine or `darwinjail.dylib`) at runtime. The backend
/// knows nothing about any particular deployment (GUI app, LaunchAgent, install location) — an embedder
/// that relocates the engines just points `$HL_JIT_DIR` at them. Resolution order:
///   1. `$HL_JIT_DIR/<name>` — an explicit override for a relocated/bundled layout,
///   2. `<dir of the running executable>/<name>` — engines sitting beside the host binary (the common
///      packaged layout), so a plain run works with no env set,
///   3. the `build.rs`-baked compile-time path (cargo/dev layout).
/// Every candidate is existence-checked, so a stale baked CI path is never returned.
fn resolve_bundled(name: &str, baked: &str) -> Option<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("HL_JIT_DIR") {
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

/// True if the JIT binary for `guest` was built and exists. Defers to [`Guest::jit_path`], which
/// existence-checks every candidate before returning it (see [`resolve_bundled`]) — so a `Some`
/// path is already known to exist, and no second check is needed here.
pub fn available(guest: Guest) -> bool {
    guest.jit_path().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_detect() {
        assert_eq!(Guest::detect("linux", "amd64"), Some(Guest::LinuxX86_64));
        assert_eq!(Guest::detect("linux", "arm64"), Some(Guest::LinuxAarch64));
        assert_eq!(Guest::detect("darwin", "aarch64"), None);
        assert_eq!(Guest::detect("plan9", "aarch64"), None);
        assert_eq!(Guest::LinuxAarch64.os(), "linux");
        assert_eq!(Guest::LinuxX86_64.arch(), "x86_64");
    }
}
