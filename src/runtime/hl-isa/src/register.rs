use crate::GuestArchitecture;

/// Architecture-neutral identity for core state used by loading, memory, and snapshots.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CoreRegister {
    /// Integer general-purpose register by architectural index.
    GeneralPurpose(u8),
    /// Architectural stack pointer.
    StackPointer,
    /// Next instruction address.
    ProgramCounter,
    /// Primary userspace thread-pointer base.
    ThreadPointer,
    /// Secondary userspace thread-pointer base, present only on x86-64.
    SecondaryThreadPointer,
    /// Integer condition/status flags represented by the CPU layout.
    Flags,
    /// Base 128-bit vector register by architectural index.
    Vector(u8),
}

/// Byte location of one register in the retained native CPU-state prefix.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RegisterLayout {
    offset: u32,
    size: u16,
}

impl RegisterLayout {
    const fn new(offset: u32, size: u16) -> Self {
        Self { offset, size }
    }

    /// Byte offset from the start of the architecture's CPU state.
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// Stored width in bytes.
    pub const fn size(self) -> u16 {
        self.size
    }
}

impl GuestArchitecture {
    /// Resolves a neutral core register into the baked native CPU-state prefix.
    ///
    /// Dynamic execution-only tails such as AVX-512 upper lanes and engine
    /// scratch state intentionally have no identifier here.
    pub const fn register_layout(self, register: CoreRegister) -> Option<RegisterLayout> {
        match (self, register) {
            (Self::Aarch64, CoreRegister::GeneralPurpose(index)) if index < 31 => {
                Some(RegisterLayout::new(index as u32 * 8, 8))
            }
            (Self::Aarch64, CoreRegister::StackPointer) => Some(RegisterLayout::new(248, 8)),
            (Self::Aarch64, CoreRegister::ProgramCounter) => Some(RegisterLayout::new(256, 8)),
            (Self::Aarch64, CoreRegister::ThreadPointer) => Some(RegisterLayout::new(264, 8)),
            (Self::Aarch64, CoreRegister::SecondaryThreadPointer) => None,
            (Self::Aarch64, CoreRegister::Flags) => Some(RegisterLayout::new(1024, 8)),
            (Self::Aarch64, CoreRegister::Vector(index)) if index < 32 => {
                Some(RegisterLayout::new(384 + index as u32 * 16, 16))
            }
            (Self::X86_64, CoreRegister::GeneralPurpose(index)) if index < 16 => {
                Some(RegisterLayout::new(index as u32 * 8, 8))
            }
            // RSP is architectural GPR index four in the x86 CPU layout.
            (Self::X86_64, CoreRegister::StackPointer) => Some(RegisterLayout::new(32, 8)),
            (Self::X86_64, CoreRegister::ProgramCounter) => Some(RegisterLayout::new(128, 8)),
            (Self::X86_64, CoreRegister::ThreadPointer) => Some(RegisterLayout::new(144, 8)),
            (Self::X86_64, CoreRegister::SecondaryThreadPointer) => Some(RegisterLayout::new(152, 8)),
            (Self::X86_64, CoreRegister::Flags) => Some(RegisterLayout::new(136, 8)),
            (Self::X86_64, CoreRegister::Vector(index)) if index < 16 => {
                Some(RegisterLayout::new(400 + index as u32 * 16, 16))
            }
            _ => None,
        }
    }
}
