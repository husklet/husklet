use std::{error::Error, fmt};

/// Byte order exposed by a guest architecture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Endianness {
    /// Least-significant byte is stored first.
    Little,
}

/// Linux guest architecture selected by the public engine configuration.
///
/// Values are pinned to `hl_guest_isa` in `include/hl/engine.h`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u32)]
pub enum GuestArchitecture {
    /// 64-bit Arm Linux ABI.
    Aarch64 = 1,
    /// 64-bit x86 Linux ABI.
    X86_64 = 2,
}

impl GuestArchitecture {
    /// Linux ELF `e_machine` value accepted for this guest.
    #[must_use]
    pub const fn elf_machine(self) -> u16 {
        match self {
            Self::Aarch64 => 183,
            Self::X86_64 => 62,
        }
    }

    /// Size of the Linux guest `struct stat` encoded by the engine.
    #[must_use]
    pub const fn linux_stat_size(self) -> usize {
        match self {
            Self::Aarch64 => 128,
            Self::X86_64 => 144,
        }
    }

    /// Smallest aligned instruction address.
    #[must_use]
    pub const fn instruction_alignment(self) -> u8 {
        match self {
            Self::Aarch64 => 4,
            Self::X86_64 => 1,
        }
    }

    /// Guest byte order. Both currently supported Linux ABIs are little-endian.
    #[must_use]
    pub const fn endianness(self) -> Endianness {
        Endianness::Little
    }

    /// Number of bits in an architectural integer word.
    #[must_use]
    pub const fn word_bits(self) -> u8 {
        64
    }
}

impl TryFrom<u32> for GuestArchitecture {
    type Error = InvalidArchitecture;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Aarch64),
            2 => Ok(Self::X86_64),
            invalid => Err(InvalidArchitecture(invalid)),
        }
    }
}

impl From<GuestArchitecture> for u32 {
    fn from(value: GuestArchitecture) -> Self {
        value as Self
    }
}

/// CPU architecture on which the engine process executes.
///
/// Values match `HL_HOST_CPU_ISA_*` in `src/host/host_cpu.h`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u32)]
pub enum HostArchitecture {
    /// 64-bit Arm host CPU.
    Aarch64 = 1,
    /// 64-bit x86 host CPU.
    X86_64 = 2,
}

impl HostArchitecture {
    /// Architecture selected by the Rust compilation target.
    #[cfg(target_arch = "aarch64")]
    pub const CURRENT: Self = Self::Aarch64;

    /// Architecture selected by the Rust compilation target.
    #[cfg(target_arch = "x86_64")]
    pub const CURRENT: Self = Self::X86_64;
}

/// One supported host/guest architecture combination.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArchitecturePair {
    host: HostArchitecture,
    guest: GuestArchitecture,
}

impl ArchitecturePair {
    /// Validates a host/guest pair against the compiled engine matrix.
    #[must_use]
    pub const fn new(host: HostArchitecture, guest: GuestArchitecture) -> Self {
        Self { host, guest }
    }

    /// Host CPU architecture.
    #[must_use]
    pub const fn host(self) -> HostArchitecture {
        self.host
    }

    /// Guest Linux architecture.
    #[must_use]
    pub const fn guest(self) -> GuestArchitecture {
        self.guest
    }

    /// Whether host and guest use the same instruction set.
    #[must_use]
    pub const fn is_same_architecture(self) -> bool {
        matches!(
            (self.host, self.guest),
            (HostArchitecture::Aarch64, GuestArchitecture::Aarch64)
                | (HostArchitecture::X86_64, GuestArchitecture::X86_64)
        )
    }
}

/// Complete host/guest matrix implemented by the two target definitions.
pub const SUPPORTED_PAIRS: [ArchitecturePair; 4] = [
    ArchitecturePair::new(HostArchitecture::Aarch64, GuestArchitecture::Aarch64),
    ArchitecturePair::new(HostArchitecture::Aarch64, GuestArchitecture::X86_64),
    ArchitecturePair::new(HostArchitecture::X86_64, GuestArchitecture::Aarch64),
    ArchitecturePair::new(HostArchitecture::X86_64, GuestArchitecture::X86_64),
];

/// Numeric architecture identifier is not part of the public engine ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidArchitecture(pub u32);

impl fmt::Display for InvalidArchitecture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unsupported guest architecture {}", self.0)
    }
}

impl Error for InvalidArchitecture {}
