use hl_isa::GuestArchitecture;

pub const SECCOMP_MAXIMUM_INSTRUCTIONS: usize = 4096;
const SCRATCH_WORDS: u32 = 16;
const DATA_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BpfInstruction {
    pub code: u16,
    pub jump_true: u8,
    pub jump_false: u8,
    pub value: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BpfProgram {
    instructions: Vec<BpfInstruction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmError {
    Empty,
    TooLong,
    InvalidOpcode,
    InvalidJump,
    InvalidScratch,
    InvalidLoad,
    DivisionByZero,
    Unterminated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Data {
    pub number: i32,
    pub architecture: u32,
    pub instruction_pointer: u64,
    pub arguments: [u64; 6],
}

impl Data {
    #[must_use]
    pub const fn audit_arch(architecture: GuestArchitecture) -> u32 {
        match architecture {
            GuestArchitecture::X86_64 => 0xc000_003e,
            GuestArchitecture::Aarch64 => 0xc000_00b7,
        }
    }

    fn bytes(self) -> [u8; 64] {
        let mut bytes = [0; 64];
        bytes[..4].copy_from_slice(&self.number.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.architecture.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.instruction_pointer.to_le_bytes());
        for (index, argument) in self.arguments.into_iter().enumerate() {
            let offset = 16 + index * 8;
            bytes[offset..offset + 8].copy_from_slice(&argument.to_le_bytes());
        }
        bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    KillProcess { data: u16 },
    KillThread { data: u16 },
    Trap { data: u16 },
    Errno { data: u16 },
    UserNotification { data: u16 },
    Trace { data: u16 },
    Log { data: u16 },
    Allow { data: u16 },
}

impl Action {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        let data = raw as u16;
        match raw & 0xffff_0000 {
            0x8000_0000 => Self::KillProcess { data },
            0x0000_0000 => Self::KillThread { data },
            0x0003_0000 => Self::Trap { data },
            0x0005_0000 => Self::Errno { data },
            0x7fc0_0000 => Self::UserNotification { data },
            0x7ff0_0000 => Self::Trace { data },
            0x7ffc_0000 => Self::Log { data },
            0x7fff_0000 => Self::Allow { data },
            _ => Self::KillThread { data },
        }
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        let (action, data) = match self {
            Self::KillProcess { data } => (0x8000_0000, data),
            Self::KillThread { data } => (0x0000_0000, data),
            Self::Trap { data } => (0x0003_0000, data),
            Self::Errno { data } => (0x0005_0000, data),
            Self::UserNotification { data } => (0x7fc0_0000, data),
            Self::Trace { data } => (0x7ff0_0000, data),
            Self::Log { data } => (0x7ffc_0000, data),
            Self::Allow { data } => (0x7fff_0000, data),
        };
        action | data as u32
    }

    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::KillProcess { .. } => 0,
            Self::KillThread { .. } => 1,
            Self::Trap { .. } => 2,
            Self::Errno { .. } => 3,
            Self::UserNotification { .. } => 4,
            Self::Trace { .. } => 5,
            Self::Log { .. } => 6,
            Self::Allow { .. } => 7,
        }
    }
}

impl BpfProgram {
    pub fn new(instructions: Vec<BpfInstruction>) -> Result<Self, VmError> {
        if instructions.is_empty() {
            return Err(VmError::Empty);
        }
        if instructions.len() > SECCOMP_MAXIMUM_INSTRUCTIONS {
            return Err(VmError::TooLong);
        }
        for (index, instruction) in instructions.iter().copied().enumerate() {
            Self::verify_instruction(index, instructions.len(), instruction)?;
        }
        let last = instructions.last().expect("nonempty");
        if last.code & 7 != 6 {
            return Err(VmError::Unterminated);
        }
        Ok(Self { instructions })
    }

    #[must_use]
    pub fn instructions(&self) -> &[BpfInstruction] {
        &self.instructions
    }

    #[must_use]
    pub fn evaluate(&self, data: Data) -> Action {
        Action::from_raw(self.run(data))
    }

    fn run(&self, data: Data) -> u32 {
        let packet = data.bytes();
        let mut accumulator = 0_u32;
        let mut index = 0_u32;
        let mut scratch = [0_u32; SCRATCH_WORDS as usize];
        let mut pc = 0_usize;
        while pc < self.instructions.len() {
            let instruction = self.instructions[pc];
            let code = instruction.code;
            match code & 7 {
                0 => {
                    let Some(value) = Self::load(code, instruction.value, index, &packet, &scratch) else {
                        return 0;
                    };
                    accumulator = value;
                }
                1 => {
                    let Some(value) = Self::load_index(code, instruction.value, &packet, &scratch) else {
                        return 0;
                    };
                    index = value;
                }
                2 => scratch[instruction.value as usize] = accumulator,
                3 => scratch[instruction.value as usize] = index,
                4 => {
                    let source = Self::source(code, index, instruction.value);
                    let Some(value) = Self::arithmetic(code, accumulator, source) else {
                        return 0;
                    };
                    accumulator = value;
                }
                5 => {
                    let offset = Self::jump(code, accumulator, index, instruction);
                    pc += offset as usize;
                }
                6 => return Self::return_value(code, accumulator, instruction.value),
                7 if code & 0xf8 == 0 => index = accumulator,
                7 => accumulator = index,
                _ => return 0,
            }
            pc += 1;
        }
        0
    }

    fn verify_instruction(index: usize, length: usize, instruction: BpfInstruction) -> Result<(), VmError> {
        let code = instruction.code;
        if !Self::known_opcode(code) {
            return Err(VmError::InvalidOpcode);
        }
        match code & 7 {
            0 => Self::verify_load(code, instruction.value)?,
            1 => Self::verify_load_index(code, instruction.value)?,
            2 | 3 if instruction.value >= SCRATCH_WORDS => {
                return Err(VmError::InvalidScratch);
            }
            2 | 3 => {}
            4 => Self::verify_arithmetic(code, instruction.value)?,
            5 => Self::verify_jump(index, length, instruction)?,
            6 if !matches!(code & 0x18, 0 | 0x10) => {
                return Err(VmError::InvalidOpcode);
            }
            6 => {}
            7 if !matches!(code & 0xf8, 0 | 0x80) => {
                return Err(VmError::InvalidOpcode);
            }
            7 => {}
            _ => return Err(VmError::InvalidOpcode),
        }
        Ok(())
    }

    fn known_opcode(code: u16) -> bool {
        matches!(
            code,
            0x00 | 0x20
                | 0x28
                | 0x30
                | 0x40
                | 0x48
                | 0x50
                | 0x60
                | 0x80
                | 0x01
                | 0x61
                | 0x81
                | 0xb1
                | 0x02
                | 0x03
                | 0x04
                | 0x0c
                | 0x14
                | 0x1c
                | 0x24
                | 0x2c
                | 0x34
                | 0x3c
                | 0x44
                | 0x4c
                | 0x54
                | 0x5c
                | 0x64
                | 0x6c
                | 0x74
                | 0x7c
                | 0x84
                | 0x94
                | 0x9c
                | 0xa4
                | 0xac
                | 0x05
                | 0x15
                | 0x1d
                | 0x25
                | 0x2d
                | 0x35
                | 0x3d
                | 0x45
                | 0x4d
                | 0x06
                | 0x16
                | 0x07
                | 0x87
        )
    }

    fn verify_load(code: u16, value: u32) -> Result<(), VmError> {
        match code & 0xe0 {
            0 | 0x80 => Ok(()),
            0x20 => Self::verify_load_range(code, value),
            0x40 => Ok(()),
            0x60 if value < SCRATCH_WORDS => Ok(()),
            0x60 => Err(VmError::InvalidScratch),
            _ => Err(VmError::InvalidOpcode),
        }
    }

    fn verify_load_index(code: u16, value: u32) -> Result<(), VmError> {
        match code & 0xe0 {
            0 | 0x80 => Ok(()),
            0x60 if value < SCRATCH_WORDS => Ok(()),
            0x60 => Err(VmError::InvalidScratch),
            0xa0 if value < DATA_SIZE => Ok(()),
            0xa0 => Err(VmError::InvalidLoad),
            _ => Err(VmError::InvalidOpcode),
        }
    }

    fn verify_load_range(code: u16, offset: u32) -> Result<(), VmError> {
        let width = Self::width(code)?;
        if offset.checked_add(width).is_some_and(|end| end <= DATA_SIZE) {
            Ok(())
        } else {
            Err(VmError::InvalidLoad)
        }
    }

    fn verify_arithmetic(code: u16, value: u32) -> Result<(), VmError> {
        if !matches!(
            code & 0xf0,
            0 | 0x10 | 0x20 | 0x30 | 0x40 | 0x50 | 0x60 | 0x70 | 0x80 | 0x90 | 0xa0
        ) {
            return Err(VmError::InvalidOpcode);
        }
        if code & 8 == 0 && matches!(code & 0xf0, 0x30 | 0x90) && value == 0 {
            return Err(VmError::DivisionByZero);
        }
        Ok(())
    }

    fn verify_jump(index: usize, length: usize, instruction: BpfInstruction) -> Result<(), VmError> {
        let remaining = length - index - 1;
        if instruction.code & 0xf0 == 0 {
            if instruction.value as usize >= remaining {
                return Err(VmError::InvalidJump);
            }
        } else if !matches!(instruction.code & 0xf0, 0x10 | 0x20 | 0x30 | 0x40)
            || instruction.jump_true as usize >= remaining
            || instruction.jump_false as usize >= remaining
        {
            return Err(VmError::InvalidJump);
        }
        Ok(())
    }

    fn load(code: u16, value: u32, index: u32, packet: &[u8; 64], scratch: &[u32; 16]) -> Option<u32> {
        match code & 0xe0 {
            0 => Some(value),
            0x80 => Some(DATA_SIZE),
            0x20 => Self::packet(packet, value, code),
            0x40 => Self::packet(packet, index.checked_add(value)?, code),
            0x60 => scratch.get(value as usize).copied(),
            _ => None,
        }
    }

    fn load_index(code: u16, value: u32, packet: &[u8; 64], scratch: &[u32; 16]) -> Option<u32> {
        match code & 0xe0 {
            0 => Some(value),
            0x80 => Some(DATA_SIZE),
            0x60 => scratch.get(value as usize).copied(),
            0xa0 => packet.get(value as usize).map(|byte| 4 * u32::from(byte & 15)),
            _ => None,
        }
    }

    fn packet(packet: &[u8; 64], offset: u32, code: u16) -> Option<u32> {
        let width = Self::width(code).ok()? as usize;
        let start = offset as usize;
        let bytes = packet.get(start..start.checked_add(width)?)?;
        Some(
            bytes
                .iter()
                .enumerate()
                .fold(0, |value, (shift, byte)| value | u32::from(*byte) << (shift * 8)),
        )
    }

    fn width(code: u16) -> Result<u32, VmError> {
        match code & 0x18 {
            0 => Ok(4),
            8 => Ok(2),
            16 => Ok(1),
            _ => Err(VmError::InvalidLoad),
        }
    }

    fn arithmetic(code: u16, accumulator: u32, source: u32) -> Option<u32> {
        match code & 0xf0 {
            0 => Some(accumulator.wrapping_add(source)),
            0x10 => Some(accumulator.wrapping_sub(source)),
            0x20 => Some(accumulator.wrapping_mul(source)),
            0x30 => accumulator.checked_div(source),
            0x40 => Some(accumulator | source),
            0x50 => Some(accumulator & source),
            0x60 => Some(accumulator.checked_shl(source).unwrap_or(0)),
            0x70 => Some(accumulator.checked_shr(source).unwrap_or(0)),
            0x80 => Some(0_u32.wrapping_sub(accumulator)),
            0x90 => accumulator.checked_rem(source),
            0xa0 => Some(accumulator ^ source),
            _ => None,
        }
    }

    const fn return_value(code: u16, accumulator: u32, immediate: u32) -> u32 {
        if code & 0x18 == 0x10 { accumulator } else { immediate }
    }

    const fn source(code: u16, index: u32, immediate: u32) -> u32 {
        if code & 8 != 0 { index } else { immediate }
    }

    const fn jump(code: u16, accumulator: u32, index: u32, instruction: BpfInstruction) -> u8 {
        if code & 0xf0 == 0 {
            return instruction.value as u8;
        }
        let compare = if code & 8 != 0 { index } else { instruction.value };
        let matched = match code & 0xf0 {
            0x10 => accumulator == compare,
            0x20 => accumulator > compare,
            0x30 => accumulator >= compare,
            0x40 => accumulator & compare != 0,
            _ => false,
        };
        if matched {
            instruction.jump_true
        } else {
            instruction.jump_false
        }
    }
}
