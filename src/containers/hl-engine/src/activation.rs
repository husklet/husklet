//! Owned activation inputs that select an engine binary.
//!
//! ISA and host streams are activation concerns. They intentionally do not
//! appear in the architecture-neutral launch-config wire.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum GuestIsa {
    Aarch64 = 1,
    X86_64 = 2,
}

impl GuestIsa {
    #[must_use]
    pub const fn from_abi(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Aarch64),
            2 => Some(Self::X86_64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn engine_stem(self) -> &'static str {
        match self {
            Self::Aarch64 => "hl-aarch64",
            Self::X86_64 => "hl-x86_64",
        }
    }
}

/// Zero is the ABI spelling of “inherit the application stream”.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ActivationDescriptor(u64);

impl ActivationDescriptor {
    pub const INHERIT: Self = Self(0);
    pub const MAXIMUM: u64 = i32::MAX as u64;

    pub const fn new(value: u64) -> Result<Self, ActivationError> {
        if value > Self::MAXIMUM {
            Err(ActivationError::DescriptorOutOfRange)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub const fn abi_value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ActivationStreams {
    pub input: ActivationDescriptor,
    pub output: ActivationDescriptor,
    pub error: ActivationDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationRequest {
    pub executable: PathBuf,
    pub guest_isa: GuestIsa,
    pub config_path: PathBuf,
    pub streams: ActivationStreams,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationError {
    PathNotAbsolute,
    DescriptorOutOfRange,
}

impl ActivationRequest {
    pub fn new(
        executable: impl Into<PathBuf>,
        guest_isa: GuestIsa,
        config_path: impl Into<PathBuf>,
        streams: ActivationStreams,
    ) -> Result<Self, ActivationError> {
        let executable = executable.into();
        let config_path = config_path.into();
        if !Self::is_absolute_nonempty(&executable) || !Self::is_absolute_nonempty(&config_path) {
            return Err(ActivationError::PathNotAbsolute);
        }
        Ok(Self {
            executable,
            guest_isa,
            config_path,
            streams,
        })
    }

    fn is_absolute_nonempty(path: &std::path::Path) -> bool {
        path.is_absolute() && path.as_os_str().len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn both_public_isa() {
        assert_eq!(GuestIsa::from_abi(1), Some(GuestIsa::Aarch64));
        assert_eq!(GuestIsa::from_abi(2), Some(GuestIsa::X86_64));
        assert_eq!(GuestIsa::from_abi(0), None);
        assert_ne!(GuestIsa::Aarch64.engine_stem(), GuestIsa::X86_64.engine_stem());
    }

    #[test]
    fn stream_zero_inherits() {
        assert_eq!(ActivationDescriptor::INHERIT.abi_value(), 0);
        assert_eq!(
            ActivationDescriptor::new(i32::MAX as u64).unwrap().abi_value(),
            i32::MAX as u64
        );
        assert_eq!(
            ActivationDescriptor::new(i32::MAX as u64 + 1),
            Err(ActivationError::DescriptorOutOfRange)
        );
    }

    #[test]
    fn activation_owns_absolute() {
        let request = ActivationRequest::new(
            "/opt/hl-engine",
            GuestIsa::Aarch64,
            "/tmp/launch",
            ActivationStreams::default(),
        )
        .unwrap();
        assert_eq!(request.executable, Path::new("/opt/hl-engine"));
        assert_eq!(
            ActivationRequest::new(
                "hl-engine",
                GuestIsa::X86_64,
                "/tmp/launch",
                ActivationStreams::default()
            ),
            Err(ActivationError::PathNotAbsolute)
        );
    }
}
