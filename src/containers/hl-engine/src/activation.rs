//! Owned activation inputs that select an engine binary.
//!
//! ISA and host streams are activation concerns. They intentionally do not
//! appear in the architecture-neutral launch-config wire.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_public_isa() {
        assert_eq!(GuestIsa::from_abi(1), Some(GuestIsa::Aarch64));
        assert_eq!(GuestIsa::from_abi(2), Some(GuestIsa::X86_64));
        assert_eq!(GuestIsa::from_abi(0), None);
        assert_ne!(GuestIsa::Aarch64.engine_stem(), GuestIsa::X86_64.engine_stem());
    }
}
