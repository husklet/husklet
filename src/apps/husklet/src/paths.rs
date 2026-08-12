//! Canonical workspace runtime locations.

use std::path::PathBuf;

/// `$HOME`, or `.` as a last resort.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| ".".into(), Into::into)
}

/// `~/.hl` — state root (images, volumes, state.json, run/).
#[must_use]
pub fn hl_root() -> PathBuf {
    home().join(".hl")
}

/// `~/.hl/images` — image rootfs dirs (== `HL_IMAGES`).
#[must_use]
pub fn images_dir() -> PathBuf {
    hl_root().join("images")
}

/// Workspace resource daemon shipped beside the application or in its macOS resource directory.
#[must_use]
pub fn daemon_bin() -> PathBuf {
    DaemonBinary::resolve()
}

struct DaemonBinary;

impl DaemonBinary {
    fn resolve() -> PathBuf {
        if let Some(p) = std::env::var_os("HL_DOCKERD_BIN") {
            return PathBuf::from(p);
        }
        if let Some(binary) = Self::bundle() {
            return binary;
        }
        if let Some(sibling) = Self::sibling() {
            return sibling;
        }
        hl_root().join("dockerd")
    }

    fn sibling() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let sibling = executable.parent()?.join("dockerd");
        sibling.exists().then_some(sibling)
    }

    fn bundle() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let binary = macos.parent()?.join("Resources/dockerd");
        binary.exists().then_some(binary)
    }
}
