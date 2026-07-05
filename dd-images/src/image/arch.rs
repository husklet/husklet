//! The image target (OS + ISA) and its detection from an OCI config blob.

use serde_json::Value;

/// The target an image runs as: OS + ISA. Runtime-agnostic (no dependency on a runtime's guest type);
/// map it to your runtime's own target when you launch the rootfs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    LinuxAarch64,
    LinuxX86_64,
    DarwinAarch64,
}

impl Arch {
    /// The `(os, architecture)` OCI pair, e.g. `("linux", "arm64")`.
    pub fn oci(self) -> (&'static str, &'static str) {
        match self {
            Arch::LinuxAarch64 => ("linux", "arm64"),
            Arch::LinuxX86_64 => ("linux", "amd64"),
            Arch::DarwinAarch64 => ("darwin", "arm64"),
        }
    }

    /// The `<os>_<isa>` slug (matches dd-jit's `Guest::target()`), e.g. `"linux_aarch64"`.
    pub fn target(self) -> &'static str {
        match self {
            Arch::LinuxAarch64 => "linux_aarch64",
            Arch::LinuxX86_64 => "linux_x86_64",
            Arch::DarwinAarch64 => "darwin_aarch64",
        }
    }

    /// The OS personality alone: `"linux"` or `"darwin"` (the first half of [`oci`](Self::oci)).
    pub fn os(self) -> &'static str {
        self.oci().0
    }

    /// The instruction set slug — `"aarch64"` or `"x86_64"` (NOT the OCI `arm64`/`amd64` label; this is
    /// the kernel `uname -m` form recorded in the on-disk `dd-image.json` sidecar so discovery can
    /// round-trip an image's arch even when its binaries can't be sniffed).
    pub fn isa(self) -> &'static str {
        match self {
            Arch::LinuxX86_64 => "x86_64",
            _ => "aarch64",
        }
    }

    /// Map an `(os, arch)` pair of loose strings (a binary's magic, an image's metadata, a sidecar's
    /// recorded fields) to an [`Arch`], accepting the common OCI/kernel spellings. `None` if unrecognized.
    pub fn detect(os: &str, arch: &str) -> Option<Arch> {
        match (os, arch.to_ascii_lowercase().as_str()) {
            ("linux", "aarch64" | "arm64" | "arm64/v8") => Some(Arch::LinuxAarch64),
            ("linux", "x86_64" | "amd64" | "x86-64") => Some(Arch::LinuxX86_64),
            ("darwin", "aarch64" | "arm64") => Some(Arch::DarwinAarch64),
            _ => None,
        }
    }
}

/// Map an OCI config blob's `architecture` + `os` to an [`Arch`]. `None` if unrecognized.
pub fn arch_from_config(config: &Value) -> Option<Arch> {
    let os = config["os"].as_str().unwrap_or("linux");
    match (os, config["architecture"].as_str()?) {
        ("darwin", "arm64" | "aarch64") => Some(Arch::DarwinAarch64),
        (_, "amd64" | "x86_64") => Some(Arch::LinuxX86_64),
        (_, "arm64" | "aarch64") => Some(Arch::LinuxAarch64),
        _ => None,
    }
}
