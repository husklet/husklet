use clap::ValueEnum;
use serde::Deserialize;

pub(crate) type Error = Box<dyn std::error::Error>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Target {
    Arm64,
    Amd64,
}

impl Target {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }

    pub(crate) const fn guest(self) -> hl_container::Guest {
        match self {
            Self::Arm64 => hl_container::Guest::Aarch64,
            Self::Amd64 => hl_container::Guest::X86_64,
        }
    }

    pub(crate) fn platform(self) -> hl_images::Platform {
        match self {
            Self::Arm64 => hl_images::Platform::linux_arm64(),
            Self::Amd64 => hl_images::Platform::linux_amd64(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    #[serde(default)]
    native: bool,
    #[serde(default)]
    diagnostics: bool,
}

impl Execution {
    pub(crate) fn container(self) -> Result<hl_container::Execution, Error> {
        if self.diagnostics && !self.native {
            return Err("native diagnostics require native execution".into());
        }
        Ok(if self.native {
            hl_container::Execution::native(self.diagnostics)
        } else {
            hl_container::Execution::default()
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Commands {
    arm64: String,
    amd64: String,
}

impl Commands {
    pub(crate) fn for_target(&self, target: Target) -> &str {
        match target {
            Target::Arm64 => &self.arm64,
            Target::Amd64 => &self.amd64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Commands, Execution, Target};
    use clap::ValueEnum;

    #[test]
    fn targets_preserve_guest_and_platform_routing() {
        assert!(Target::from_str("x86", false).is_err());
        assert_eq!(Target::from_str("arm64", false), Ok(Target::Arm64));
        assert_eq!(Target::from_str("amd64", false), Ok(Target::Amd64));
        assert_eq!(Target::Arm64.guest(), hl_container::Guest::Aarch64);
        assert_eq!(Target::Amd64.guest(), hl_container::Guest::X86_64);
        assert_eq!(Target::Arm64.platform(), hl_images::Platform::linux_arm64());
        assert_eq!(Target::Amd64.platform(), hl_images::Platform::linux_amd64());
    }

    #[test]
    fn execution_preserves_native_diagnostic_validation() {
        let enabled: Execution = serde_yaml::from_str("native: true\ndiagnostics: true\n").unwrap();
        let enabled = enabled.container().unwrap();
        assert!(enabled.is_native());
        assert!(enabled.diagnostics());
        let invalid: Execution = serde_yaml::from_str("diagnostics: true\n").unwrap();
        assert!(invalid.container().is_err());
    }

    #[test]
    fn commands_select_the_existing_isa_spelling() {
        let commands: Commands = serde_yaml::from_str("arm64: arm-cc\namd64: amd-cc\n").unwrap();
        assert_eq!(commands.for_target(Target::Arm64), "arm-cc");
        assert_eq!(commands.for_target(Target::Amd64), "amd-cc");
    }
}
