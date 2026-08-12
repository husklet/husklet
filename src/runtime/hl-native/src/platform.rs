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
        if target.supported() { Some(target) } else { None }
    }

    /// Both guest ISAs are present on every supported host. The target C code
    /// chooses its JIT, transliterator, or interpreter from the host CPU.
    #[must_use]
    pub const fn supports_guest(self, guest: GuestIsa) -> bool {
        let _ = (self, guest);
        true
    }

    /// Execution selected by the production build (`HL_TRANSLIT_DEFAULT=0`).
    /// AArch64 hosts execute both guest ISAs through their native-code JITs;
    /// x86-64 hosts use the portable interpreters by default.
    #[must_use]
    pub const fn default_execution(self, guest: GuestIsa) -> ExecutionMode {
        let _ = guest;
        match self.arch {
            HostArch::Aarch64 => ExecutionMode::Jit,
            HostArch::X86_64 => ExecutionMode::Interpreter,
        }
    }

    /// Reports optional execution machinery compiled for a host/guest pair.
    #[must_use]
    pub const fn supports_execution(self, guest: GuestIsa, mode: ExecutionMode) -> bool {
        match (self.arch, guest, mode) {
            (HostArch::Aarch64, _, ExecutionMode::Jit)
            | (HostArch::X86_64, _, ExecutionMode::Interpreter)
            | (HostArch::X86_64, GuestIsa::X86_64, ExecutionMode::Transliterator) => true,
            _ => false,
        }
    }

    #[must_use]
    pub const fn supported(self) -> bool {
        matches!(
            (self.os, self.arch),
            (HostOs::Linux, HostArch::Aarch64 | HostArch::X86_64)
                | (HostOs::Macos, HostArch::Aarch64)
                | (HostOs::Windows, HostArch::X86_64)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestIsa {
    Aarch64,
    X86_64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Jit,
    Interpreter,
    Transliterator,
}

pub const SUPPORTED_HOSTS: &[HostTarget] = &[
    HostTarget { os: HostOs::Linux, arch: HostArch::Aarch64 },
    HostTarget { os: HostOs::Linux, arch: HostArch::X86_64 },
    HostTarget { os: HostOs::Macos, arch: HostArch::Aarch64 },
    HostTarget { os: HostOs::Windows, arch: HostArch::X86_64 },
];

/// Returns whether a native source belongs to the selected host operating
/// system. Sources outside `host/<platform>/` are portable engine sources and
/// remain selected on every host.
pub fn source_matches(target_os: &str, source: &str) -> bool {
    const HOST_PREFIX: &str = "src/native/host/";
    let Some(host_relative) = source.strip_prefix(HOST_PREFIX) else {
        return true;
    };
    let Some((platform, _)) = host_relative.split_once('/') else {
        return true;
    };
    !matches!(platform, "linux" | "macos" | "windows") || platform == target_os
}
#[cfg(test)]
mod tests {
    use super::{ExecutionMode, GuestIsa, HostArch, HostOs, HostTarget, SUPPORTED_HOSTS};

    #[test]
    fn host_matrix_is_explicit_and_complete() {
        for os in ["linux", "macos", "windows", "freebsd"] {
            for arch in ["aarch64", "x86_64", "riscv64"] {
                let parsed = HostTarget::from_cfg(os, arch);
                assert_eq!(
                    parsed.is_some(),
                    SUPPORTED_HOSTS.iter().any(|target| {
                        target.os.cfg_name() == os && target.arch.cfg_name() == arch
                    }),
                    "{arch}-{os}"
                );
            }
        }
    }

    #[test]
    fn every_host_supplies_both_guest_isas() {
        for target in SUPPORTED_HOSTS {
            assert!(target.supports_guest(GuestIsa::Aarch64));
            assert!(target.supports_guest(GuestIsa::X86_64));
        }
    }

    #[test]
    fn host_cpu_selects_execution_machinery() {
        for target in SUPPORTED_HOSTS {
            for guest in [GuestIsa::Aarch64, GuestIsa::X86_64] {
                let expected = match target.arch {
                    HostArch::Aarch64 => ExecutionMode::Jit,
                    HostArch::X86_64 => ExecutionMode::Interpreter,
                };
                assert_eq!(target.default_execution(guest), expected);
                assert!(target.supports_execution(guest, expected));
            }
        }
        let linux_x86 = HostTarget::from_cfg("linux", "x86_64").unwrap();
        assert!(linux_x86.supports_execution(GuestIsa::X86_64, ExecutionMode::Transliterator));
        assert!(!linux_x86.supports_execution(GuestIsa::Aarch64, ExecutionMode::Transliterator));
    }

    #[test]
    fn unsupported_pair_is_not_constructed() {
        assert_eq!(HostTarget::from_cfg("macos", "x86_64"), None);
        assert!(!HostTarget { os: HostOs::Windows, arch: HostArch::Aarch64 }.supported());
    }

    #[test]
    fn platform_source_closures_do_not_mix_host_implementations() {
        for target in ["linux", "macos", "windows"] {
            for platform in ["linux", "macos", "windows"] {
                let source = format!("src/native/host/{platform}/host.c");
                assert_eq!(super::source_matches(target, &source), target == platform);
            }
            assert!(super::source_matches(target, "src/native/engine/runtime.c"));
            assert!(super::source_matches(target, "src/native/host/sync.c"));
        }
    }
}
