//! The image target (OS + ISA) and its detection from an OCI config blob.

use serde_json::Value;

/// The target an image runs as: OS + ISA. Runtime-agnostic (no dependency on a runtime's guest type);
/// map it to your runtime's own target when you launch the rootfs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// Linux on 64-bit ARM (OCI `linux`/`arm64`).
    LinuxAarch64,
    /// Linux on 64-bit x86 (OCI `linux`/`amd64`).
    LinuxX86_64,
}

impl Arch {
    /// The `(os, architecture)` OCI pair, e.g. `("linux", "arm64")`.
    pub fn oci(self) -> (&'static str, &'static str) {
        match self {
            Arch::LinuxAarch64 => ("linux", "arm64"),
            Arch::LinuxX86_64 => ("linux", "amd64"),
        }
    }

    /// The `<os>_<isa>` slug (matches hl-jit's `Guest::target()`), e.g. `"linux_aarch64"`.
    pub fn target(self) -> &'static str {
        match self {
            Arch::LinuxAarch64 => "linux_aarch64",
            Arch::LinuxX86_64 => "linux_x86_64",
        }
    }

    /// The OS personality alone: `"linux"` (the first half of [`oci`](Self::oci)).
    pub fn os(self) -> &'static str {
        self.oci().0
    }

    /// The instruction set slug — `"aarch64"` or `"x86_64"` (NOT the OCI `arm64`/`amd64` label; this is
    /// the kernel `uname -m` form recorded in the on-disk `hl-image.json` sidecar so discovery can
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
            _ => None,
        }
    }

    /// Map an OCI config blob's `architecture` + `os` to an [`Arch`]. `None` if unrecognized — including a
    /// config whose `os` is PRESENT but not `linux` (e.g. `"windows"`, `"darwin"`): such an image must
    /// be REJECTED, not silently treated as Linux (the pull/registry path treats `None` as "reject"). An
    /// absent/empty `os` still defaults to `linux` for back-compat with configs that omit it.
    pub fn from_config(config: &Value) -> Option<Arch> {
        let os = match config["os"].as_str() {
            None | Some("") => "linux",
            Some("linux") => "linux",
            Some(_) => return None, // present but unsupported OS -> reject
        };
        match (os, config["architecture"].as_str()?) {
            (_, "amd64" | "x86_64") => Some(Arch::LinuxX86_64),
            (_, "arm64" | "aarch64") => Some(Arch::LinuxAarch64),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_linux_aliases() {
        for a in ["aarch64", "arm64", "arm64/v8"] {
            assert_eq!(Arch::detect("linux", a), Some(Arch::LinuxAarch64), "{a}");
        }
        for a in ["x86_64", "amd64", "x86-64"] {
            assert_eq!(Arch::detect("linux", a), Some(Arch::LinuxX86_64), "{a}");
        }
        // case-insensitive on the arch label
        assert_eq!(Arch::detect("linux", "ARM64"), Some(Arch::LinuxAarch64));
        assert_eq!(Arch::detect("linux", "AMD64"), Some(Arch::LinuxX86_64));
    }

    #[test]
    fn detect_darwin_and_unknown() {
        // darwin is no longer a supported target -> None
        assert_eq!(Arch::detect("darwin", "arm64"), None);
        assert_eq!(Arch::detect("darwin", "aarch64"), None);
        // unrecognized os/arch -> None
        assert_eq!(Arch::detect("windows", "amd64"), None);
        assert_eq!(Arch::detect("linux", "mips"), None);
    }

    #[test]
    fn arch_from_config_maps_os_and_arch() {
        // os defaults to linux when absent
        assert_eq!(
            Arch::from_config(&json!({"architecture": "amd64"})),
            Some(Arch::LinuxX86_64)
        );
        assert_eq!(
            Arch::from_config(&json!({"architecture": "arm64"})),
            Some(Arch::LinuxAarch64)
        );
        // darwin is no longer supported -> REJECTED (None)
        assert_eq!(
            Arch::from_config(&json!({"os": "darwin", "architecture": "arm64"})),
            None
        );
        // Finding 9 — a PRESENT but unsupported os is REJECTED (None), not treated as Linux.
        assert_eq!(
            Arch::from_config(&json!({"os": "windows", "architecture": "amd64"})),
            None
        );
        assert_eq!(
            Arch::from_config(&json!({"os": "freebsd", "architecture": "aarch64"})),
            None
        );
        // an empty os string still defaults to linux (back-compat with configs that omit it)
        assert_eq!(
            Arch::from_config(&json!({"os": "", "architecture": "amd64"})),
            Some(Arch::LinuxX86_64)
        );
        // missing/unknown architecture -> None
        assert_eq!(Arch::from_config(&json!({"os": "linux"})), None);
        assert_eq!(Arch::from_config(&json!({"architecture": "riscv64"})), None);
    }

    #[test]
    fn oci_target_isa_os_roundtrip() {
        let cases = [
            (
                Arch::LinuxAarch64,
                ("linux", "arm64"),
                "linux_aarch64",
                "aarch64",
                "linux",
            ),
            (
                Arch::LinuxX86_64,
                ("linux", "amd64"),
                "linux_x86_64",
                "x86_64",
                "linux",
            ),
        ];
        for (a, oci, target, isa, os) in cases {
            assert_eq!(a.oci(), oci);
            assert_eq!(a.target(), target);
            assert_eq!(a.isa(), isa);
            assert_eq!(a.os(), os);
            // detect(os, isa) recovers the same Arch (the on-disk sidecar round-trip)
            assert_eq!(Arch::detect(a.os(), a.isa()), Some(a));
        }
    }
}
