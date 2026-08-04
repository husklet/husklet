use crate::{
    AccessKind, BitScanOperation, BranchCondition, ByteRegister, Division, DivisionError, Flag, FlagState, MemoryFault,
    Multiplication, ScalarRegister, ScalarWidth,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtendedClass {
    Empty,
    Zero,
    Denormal,
    Normal,
    Infinity,
    QuietNan,
    SignalingNan,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtendedReal {
    bits: u128,
}

impl ExtendedReal {
    pub const INDEFINITE: Self = Self {
        bits: 0xffff_c000_0000_0000_0000,
    };

    #[must_use]
    pub const fn from_bits(bits: u128) -> Self {
        Self {
            bits: bits & ((1_u128 << 80) - 1),
        }
    }

    #[must_use]
    pub const fn bits(self) -> u128 {
        self.bits
    }

    #[must_use]
    pub const fn class(self) -> ExtendedClass {
        let exponent = (self.bits >> 64) as u16 & 0x7fff;
        let significand = self.bits as u64;
        let integer = significand >> 63;
        let fraction = significand & 0x7fff_ffff_ffff_ffff;
        if exponent == 0 {
            return if significand == 0 {
                ExtendedClass::Zero
            } else if integer == 0 {
                ExtendedClass::Denormal
            } else {
                ExtendedClass::Unsupported
            };
        }
        if exponent == 0x7fff {
            if integer == 0 {
                return ExtendedClass::Unsupported;
            }
            if fraction == 0 {
                return ExtendedClass::Infinity;
            }
            return if fraction >> 62 != 0 {
                ExtendedClass::QuietNan
            } else {
                ExtendedClass::SignalingNan
            };
        }
        if integer == 0 {
            ExtendedClass::Unsupported
        } else {
            ExtendedClass::Normal
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuState {
    pub registers: [u64; 16],
    /// Architectural XMM0..XMM15 state used by the safe execution backend.
    pub vectors: [u128; 16],
    /// Architectural YMM0..YMM15 bits 255:128. Legacy SSE preserves these
    /// lanes, while 128-bit VEX destinations clear them.
    pub vector_upper: [u128; 16],
    pub flags: FlagState,
    pub rip: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub direction: bool,
    /// Software RFLAGS.AC substrate. Long-mode POPFQ may change it; 16-bit POPF
    /// cannot name it and therefore preserves it.
    pub alignment_check: bool,
    /// Software RFLAGS.ID substrate used by userspace CPUID-availability probes.
    pub id_flag: bool,
    pub x87_control: u16,
    pub x87_status: u16,
    pub x87_values: [ExtendedReal; 8],
    pub x87_classes: [ExtendedClass; 8],
    pub mxcsr: u32,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            registers: [0; 16],
            vectors: [0; 16],
            vector_upper: [0; 16],
            flags: FlagState::default(),
            rip: 0,
            fs_base: 0,
            gs_base: 0,
            direction: false,
            alignment_check: false,
            id_flag: false,
            x87_control: 0x037f,
            x87_status: 0,
            x87_values: [ExtendedReal::from_bits(0); 8],
            x87_classes: [ExtendedClass::Empty; 8],
            mxcsr: 0x1f80,
        }
    }
}

impl CpuState {
    pub(crate) fn read_mmx(&self, register: u8) -> u64 {
        self.x87_values[usize::from(register & 7)].bits() as u64
    }

    pub(crate) fn write_mmx(&mut self, register: u8, value: u64) {
        let index = usize::from(register & 7);
        self.x87_values[index] = ExtendedReal::from_bits((0xffff_u128 << 64) | u128::from(value));
        self.x87_classes[index] = ExtendedClass::Normal;
    }

    pub(crate) fn empty_mmx(&mut self) {
        self.x87_classes.fill(ExtendedClass::Empty);
    }

    pub(crate) fn apply_bit_scan(
        mut self,
        operation: BitScanOperation,
        destination: ScalarRegister,
        width: ScalarWidth,
        source: u64,
    ) -> Self {
        let integer_width = match width {
            ScalarWidth::Word => crate::IntegerWidth::Word,
            ScalarWidth::Dword => crate::IntegerWidth::Dword,
            ScalarWidth::Qword => crate::IntegerWidth::Qword,
            ScalarWidth::Byte => unreachable!(),
        };
        let scan = match operation {
            BitScanOperation::Forward => crate::BitScan::forward(integer_width, source),
            BitScanOperation::Reverse => crate::BitScan::reverse(integer_width, source),
            BitScanOperation::TrailingZeroCount => crate::BitScan::trailing_zero_count(integer_width, source),
            BitScanOperation::LeadingZeroCount => crate::BitScan::leading_zero_count(integer_width, source),
        };
        self.flags = self.flags.with(Flag::Zero, scan.zero);
        if matches!(
            operation,
            BitScanOperation::TrailingZeroCount | BitScanOperation::LeadingZeroCount
        ) {
            self.flags = self.flags.with(Flag::Carry, scan.carry);
        }
        if let Some(result) = scan.result {
            self.write_register(destination, width, result);
        }
        self
    }

    pub(crate) fn write_product(&mut self, width: ScalarWidth, product: Multiplication) {
        if width == ScalarWidth::Byte {
            self.registers[0] = (self.registers[0] & !0xffff) | (product.low & 0xff) | ((product.high & 0xff) << 8);
            return;
        }
        self.write_register(ScalarRegister::General(0), width, product.low);
        self.write_register(ScalarRegister::General(2), width, product.high);
    }

    pub(crate) fn write_division(&mut self, width: ScalarWidth, result: Division) {
        if width == ScalarWidth::Byte {
            self.registers[0] =
                (self.registers[0] & !0xffff) | (result.quotient & 0xff) | ((result.remainder & 0xff) << 8);
            return;
        }
        self.write_register(ScalarRegister::General(0), width, result.quotient);
        self.write_register(ScalarRegister::General(2), width, result.remainder);
    }

    pub(crate) fn condition(&self, condition: BranchCondition) -> bool {
        let cf = self.flags.contains(Flag::Carry);
        let pf = self.flags.contains(Flag::Parity);
        let zf = self.flags.contains(Flag::Zero);
        let sf = self.flags.contains(Flag::Sign);
        let of = self.flags.contains(Flag::Overflow);
        match condition.0 & 15 {
            0 => of,
            1 => !of,
            2 => cf,
            3 => !cf,
            4 => zf,
            5 => !zf,
            6 => cf || zf,
            7 => !cf && !zf,
            8 => sf,
            9 => !sf,
            10 => pf,
            11 => !pf,
            12 => sf != of,
            13 => sf == of,
            14 => zf || sf != of,
            _ => !zf && sf == of,
        }
    }

    pub(crate) fn read_register(&self, register: ScalarRegister, width: ScalarWidth) -> u64 {
        match register {
            ScalarRegister::General(index) => self.registers[index as usize] & Self::mask(width),
            ScalarRegister::Byte(ByteRegister::Low(index)) => self.registers[index as usize] & 0xff,
            ScalarRegister::Byte(ByteRegister::High(index)) => (self.registers[index as usize] >> 8) & 0xff,
        }
    }

    pub(crate) fn write_register(&mut self, register: ScalarRegister, width: ScalarWidth, value: u64) {
        let ScalarRegister::General(index) = register else {
            let (index, shift) = match register {
                ScalarRegister::Byte(ByteRegister::Low(index)) => (index, 0),
                ScalarRegister::Byte(ByteRegister::High(index)) => (index, 8),
                ScalarRegister::General(_) => unreachable!(),
            };
            let mask = 0xff_u64 << shift;
            self.registers[index as usize] = (self.registers[index as usize] & !mask) | ((value & 0xff) << shift);
            return;
        };
        let current = self.registers[index as usize];
        self.registers[index as usize] = match width {
            ScalarWidth::Byte => (current & !0xff) | (value & 0xff),
            ScalarWidth::Word => (current & !0xffff) | (value & 0xffff),
            ScalarWidth::Dword => value & 0xffff_ffff,
            ScalarWidth::Qword => value,
        };
    }

    const fn mask(width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Byte => 0xff,
            ScalarWidth::Word => 0xffff,
            ScalarWidth::Dword => 0xffff_ffff,
            ScalarWidth::Qword => u64::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionExit {
    Continue,
    Syscall {
        instruction: u64,
        next: u64,
    },
    TimestampCounter {
        instruction: u64,
        next: u64,
        auxiliary: bool,
    },
    UndefinedInstruction {
        instruction: u64,
    },
    DivideError {
        instruction: u64,
        error: DivisionError,
    },
    Breakpoint {
        instruction: u64,
        next: u64,
    },
    MemoryFault(MemoryFault),
    OperandFault(crate::FaultAccess),
    NonCanonical {
        instruction: u64,
        address: u64,
        access: AccessKind,
    },
    AlignmentFault {
        instruction: u64,
        address: u64,
        access: AccessKind,
    },
    Yield {
        instruction: u64,
        completed: u64,
    },
}
