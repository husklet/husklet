//! Native-engine host and guest capability model.

/// Operating systems for which the native engine has a host-services backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

impl HostOs {
    #[must_use]
    pub const fn from_cfg(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"linux" => Some(Self::Linux),
            b"macos" => Some(Self::Macos),
            b"windows" => Some(Self::Windows),
            _ => None,
        }
    }

    #[must_use]
    pub const fn cfg_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

/// CPUs for which the native engine can run as a host process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostArch {
    Aarch64,
    X86_64,
}

impl HostArch {
    #[must_use]
    pub const fn from_cfg(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"aarch64" => Some(Self::Aarch64),
            b"x86_64" => Some(Self::X86_64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn cfg_name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
}

/// A host platform supplied by this package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTarget {
    pub os: HostOs,
    pub arch: HostArch,
}

impl HostTarget {
    #[must_use]
    pub const fn from_cfg(target_os: &str, target_arch: &str) -> Option<Self> {
        let Some(os) = HostOs::from_cfg(target_os) else {
            return None;
        };
        let Some(arch) = HostArch::from_cfg(target_arch) else {
            return None;
        };
        let target = Self { os, arch };
        if target.planned() { Some(target) } else { None }
    }

    #[must_use]
    pub const fn supported(self) -> bool {
        matches!(
            (self.os, self.arch),
            (HostOs::Linux, HostArch::Aarch64 | HostArch::X86_64) | (HostOs::Macos, HostArch::Aarch64)
        )
    }

    /// Intended product matrix. A planned target is not advertised as
    /// supported until its complete Cargo/DLL build and runtime gate pass.
    #[must_use]
    pub const fn planned(self) -> bool {
        self.supported() || matches!((self.os, self.arch), (HostOs::Windows, HostArch::X86_64))
    }
}

#[cfg(test)]
mod tests {
    use super::{HostArch, HostOs, HostTarget};

    #[test]
    fn host_matrix_is_explicit_and_complete() {
        for os in ["linux", "macos", "windows", "freebsd"] {
            for arch in ["aarch64", "x86_64", "riscv64"] {
                let parsed = HostTarget::from_cfg(os, arch);
                assert_eq!(
                    parsed.is_some(),
                    matches!(
                        (os, arch),
                        ("linux", "aarch64" | "x86_64") | ("macos", "aarch64") | ("windows", "x86_64")
                    ),
                    "{arch}-{os}"
                );
            }
        }
    }

    #[test]
    fn unsupported_pair_is_not_constructed() {
        assert_eq!(HostTarget::from_cfg("macos", "x86_64"), None);
        assert!(
            !HostTarget {
                os: HostOs::Windows,
                arch: HostArch::Aarch64
            }
            .supported()
        );
        let windows = HostTarget::from_cfg("windows", "x86_64").unwrap();
        assert!(windows.planned());
        assert!(!windows.supported());
    }
}
