//! The bare-minimum workspace MODEL — pure data, std-only.
//!
//! A workspace is a named, persistent dev environment = a specific image (distro) + architecture, launched
//! as a terminal. This type holds ONLY what is needed to run a workspace: identity + the isolated-environment
//! knobs. Per-workspace FEATURE settings (egress/VPN, simulated CUDA device, the GUI display toggle, the
//! docker-socket mount, terminal scrollback) are NOT here — they are `hl`-side config that extends this bare
//! primitive (see `hl-cli`'s `config::WorkspaceConfig`). `hl-ws` never names a feature.

use std::path::{Path, PathBuf};

/// Target architecture for a workspace's image. Maps to the engine's `Guest` / `--platform` (an x86_64
/// image runs on an arm64 mac through the jit86 translator).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// Native ARM64 Linux.
    Arm64,
    /// x86-64 Linux (translated to ARM64 by the jit86 engine on Apple silicon).
    Amd64,
}

impl Arch {
    /// The stable on-disk / config token.
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::Amd64 => "amd64",
        }
    }
    /// Parse the config token (also accepts common aliases).
    pub fn parse(s: &str) -> Option<Arch> {
        match s.trim() {
            "arm64" | "aarch64" | "linux/arm64" => Some(Arch::Arm64),
            "amd64" | "x86_64" | "x86-64" | "linux/amd64" => Some(Arch::Amd64),
            _ => None,
        }
    }
    /// A proper, human-readable `os/arch` display label: `linux/aarch64`, `linux/x86_64`.
    pub fn os_arch_label(self) -> &'static str {
        match self {
            Arch::Arm64 => "linux/aarch64",
            Arch::Amd64 => "linux/x86_64",
        }
    }
    /// The Docker `--platform` string the image resolver uses to pick which arch to pull.
    pub fn platform(self) -> Option<&'static str> {
        match self {
            Arch::Arm64 => Some("linux/arm64"),
            Arch::Amd64 => Some("linux/amd64"),
        }
    }
}

/// A host→guest bind mount for a workspace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mount {
    pub host: String,
    pub container: String,
    pub ro: bool,
}

/// A configured workspace: identity (name + image + arch) plus the isolated-environment config needed to
/// RUN it. This is the bare run primitive; feature settings (vpn/cuda/gui/docker_sock/scrollback) live in
/// `hl`-side config that wraps this type. All fields are plain data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Workspace {
    pub name: String,
    pub image: String,
    pub arch: Arch,
    /// Where the workspace's persistent state lives; `None` = the default `<base>/workspaces/<name>`.
    pub storage: Option<PathBuf>,
    /// Login shell command line, e.g. `"/bin/bash -l"`; `None` = auto (bash if present, else sh).
    pub shell: Option<String>,
    /// CPU cap (cores) and memory cap (MB); `None` = unbounded.
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    /// Environment variables injected into every shell in the workspace.
    pub env: Vec<(String, String)>,
    /// Host→guest bind mounts.
    pub mounts: Vec<Mount>,
}

impl Workspace {
    pub fn new(name: impl Into<String>, image: impl Into<String>, arch: Arch) -> Workspace {
        Workspace {
            name: name.into(),
            image: image.into(),
            arch,
            storage: None,
            shell: None,
            cpus: None,
            memory_mb: None,
            env: Vec::new(),
            mounts: Vec::new(),
        }
    }
    /// The on-disk directory holding this workspace's persistent state (honors a configured `storage`).
    pub fn storage_dir(&self, base: &Path) -> PathBuf {
        self.storage
            .clone()
            .unwrap_or_else(|| base.join("workspaces").join(sanitize(&self.name)))
    }
    /// The persistent writable-upper directory, passed to `Image::overlay(upper, [rootfs])`.
    pub fn upper_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("upper")
    }
    /// Where a whole-workspace checkpoint (the frozen process tree) is written on close / read on reopen.
    pub fn checkpoint_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint")
    }
    /// The file recording the running container init's host pid.
    pub fn checkpoint_pid_file(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint.pid")
    }
    /// Per-pane checkpoint dir (each tab/split-pane is its own engine, frozen into its own slot).
    pub fn checkpoint_slot_dir(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(sanitize(slot))
    }
    /// The pid file for a per-pane checkpoint slot.
    pub fn checkpoint_slot_pid_file(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(format!("{}.pid", sanitize(slot)))
    }
    /// The default shell command to run in the workspace.
    pub fn default_shell() -> Vec<String> {
        vec!["/bin/bash".to_string(), "-l".to_string()]
    }
}

/// Sanitize a workspace name into a filesystem-safe directory component.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_parse_and_roundtrip() {
        for a in [Arch::Arm64, Arch::Amd64] {
            assert_eq!(Arch::parse(a.as_str()), Some(a));
        }
        assert_eq!(Arch::parse("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("x86_64"), Some(Arch::Amd64));
        assert_eq!(Arch::parse("nonsense"), None);
        assert_eq!(Arch::parse("darwin"), None);
        assert_eq!(Arch::Amd64.platform(), Some("linux/amd64"));
    }

    #[test]
    fn os_arch_label_is_full_os_slash_arch() {
        assert_eq!(Arch::Arm64.os_arch_label(), "linux/aarch64");
        assert_eq!(Arch::Amd64.os_arch_label(), "linux/x86_64");
    }

    #[test]
    fn upper_dir_is_named_and_stable() {
        let ws = Workspace::new("My Cool WS!", "ubuntu", Arch::Arm64);
        let dir = ws.upper_dir(Path::new("/home/u/.dd"));
        assert_eq!(dir, Path::new("/home/u/.dd/workspaces/My_Cool_WS_/upper"));
    }
}
