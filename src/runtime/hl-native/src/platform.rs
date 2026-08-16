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
}

/// CPUs for which the native engine can run as a host process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostArch {
    Aarch64,
    X86_64,
}

/// Guest instruction sets compiled into a native-engine host artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestIsa {
    Aarch64,
    X86_64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildPlan {
    pub guests: &'static [GuestIsa],
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
        self.supported()
            || matches!(
                (self.os, self.arch),
                (HostOs::Macos | HostOs::Windows, HostArch::X86_64)
            )
    }

    /// Guest translators that can be linked for this host artifact.
    #[must_use]
    pub const fn build_plan(self) -> BuildPlan {
        const BOTH: &[GuestIsa] = &[GuestIsa::Aarch64, GuestIsa::X86_64];
        const X86_64: &[GuestIsa] = &[GuestIsa::X86_64];

        match (self.os, self.arch) {
            // Darwin/x86 cannot compile the AArch64 target's signal context.
            (HostOs::Macos, HostArch::X86_64) => BuildPlan { guests: X86_64 },
            _ => BuildPlan { guests: BOTH },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GuestIsa, HostArch, HostOs, HostTarget};

    #[test]
    fn host_matrix_is_explicit_and_complete() {
        for os in ["linux", "macos", "windows", "freebsd"] {
            for arch in ["aarch64", "x86_64", "riscv64"] {
                let parsed = HostTarget::from_cfg(os, arch);
                assert_eq!(
                    parsed.is_some(),
                    matches!(
                        (os, arch),
                        ("linux" | "macos", "aarch64" | "x86_64") | ("windows", "x86_64")
                    ),
                    "{arch}-{os}"
                );
            }
        }
    }

    #[test]
    fn unsupported_pair_is_not_constructed() {
        let macos = HostTarget::from_cfg("macos", "x86_64").unwrap();
        assert!(macos.planned());
        assert!(!macos.supported());
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

    #[test]
    fn guest_capabilities_are_declared_by_host_target() {
        assert_eq!(
            HostTarget::from_cfg("linux", "x86_64").unwrap().build_plan().guests,
            &[GuestIsa::Aarch64, GuestIsa::X86_64]
        );
        assert_eq!(
            HostTarget::from_cfg("macos", "x86_64").unwrap().build_plan().guests,
            &[GuestIsa::X86_64]
        );
    }
}
