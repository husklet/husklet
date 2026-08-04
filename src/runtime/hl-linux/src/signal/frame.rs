use hl_isa::GuestArchitecture;
use hl_task::{AlternateStack, SignalAction, SignalInfo, SignalMask};

pub const AARCH64_SIGNAL_FRAME_SIZE: usize = 4_688;
pub const X86_SIGNAL_FRAME_SIZE: usize = 2_056;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aarch64SignalMachine {
    pub registers: [u64; 31],
    pub vectors: [u128; 32],
    pub stack_pointer: u64,
    pub program_counter: u64,
    pub pstate: u64,
    pub fpcr: u32,
    pub fpsr: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X86SignalMachine {
    pub registers: [u64; 16],
    pub vectors: [u128; 16],
    pub vector_upper: [u128; 16],
    pub stack_pointer: u64,
    pub instruction_pointer: u64,
    pub rflags: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Machine {
    Aarch64(Aarch64SignalMachine),
    X86_64(X86SignalMachine),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    Architecture,
    Address,
    Alignment,
    Overflow,
    Malformed,
    UnsupportedState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameRequest {
    pub machine: Machine,
    pub information: SignalInfo,
    pub action: SignalAction,
    pub mask: SignalMask,
    pub alternate_stack: AlternateStack,
    pub sigreturn_pc: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameImage {
    pub write_address: u64,
    pub bytes: Vec<u8>,
    pub handler_machine: Machine,
    pub handler_mask: SignalMask,
    pub handler_alternate_stack: AlternateStack,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameContext {
    pub machine: Machine,
    pub mask: SignalMask,
    pub alternate_stack: AlternateStack,
}

pub struct FrameCodec;

impl FrameCodec {
    pub fn build(request: &FrameRequest) -> Result<FrameImage, FrameError> {
        match &request.machine {
            Machine::Aarch64(machine) => super::aarch64::Aarch64FrameCodec::build(request, machine),
            Machine::X86_64(machine) => super::x86::X86FrameCodec::build(request, machine),
        }
    }

    pub fn restore(
        architecture: GuestArchitecture,
        frame_address: u64,
        bytes: &[u8],
    ) -> Result<FrameContext, FrameError> {
        match architecture {
            GuestArchitecture::Aarch64 => super::aarch64::Aarch64FrameCodec::restore(frame_address, bytes),
            GuestArchitecture::X86_64 => super::x86::X86FrameCodec::restore(frame_address, bytes),
        }
    }
}

pub(crate) fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    put_u32(bytes, offset, value as u32);
}

pub(crate) fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u128(bytes: &mut [u8], offset: usize, value: u128) {
    bytes[offset..offset + 16].copy_from_slice(&value.to_le_bytes());
}

pub(crate) struct FrameReader;

impl FrameReader {
    pub(crate) fn u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("checked frame"))
    }

    pub(crate) fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("checked frame"))
    }

    pub(crate) fn u128(bytes: &[u8], offset: usize) -> u128 {
        u128::from_le_bytes(bytes[offset..offset + 16].try_into().expect("checked frame"))
    }
}

pub(crate) fn alternate_stack(
    pointer: u64,
    flags: u32,
    size: u64,
    restored_pointer: u64,
) -> Result<AlternateStack, FrameError> {
    match flags {
        0 | 1 if pointer != 0 && size != 0 => {
            let end = pointer.checked_add(size).ok_or(FrameError::Overflow)?;
            if restored_pointer >= pointer && restored_pointer < end {
                Ok(AlternateStack::Active { pointer, size })
            } else {
                Ok(AlternateStack::Enabled { pointer, size })
            }
        }
        0x8000_0000 if pointer != 0 && size != 0 => Ok(AlternateStack::Autodisarm { pointer, size }),
        2 if pointer == 0 => Ok(AlternateStack::Disabled),
        _ => Err(FrameError::Malformed),
    }
}
