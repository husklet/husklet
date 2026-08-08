#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Nzcv(u32);

impl Nzcv {
    pub const NEGATIVE: u32 = 1 << 31;
    pub const ZERO: u32 = 1 << 30;
    pub const CARRY: u32 = 1 << 29;
    pub const OVERFLOW: u32 = 1 << 28;
    pub const MASK: u32 = Self::NEGATIVE | Self::ZERO | Self::CARRY | Self::OVERFLOW;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::MASK)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn negative(self) -> bool {
        self.0 & Self::NEGATIVE != 0
    }

    #[must_use]
    pub const fn zero(self) -> bool {
        self.0 & Self::ZERO != 0
    }

    #[must_use]
    pub const fn carry(self) -> bool {
        self.0 & Self::CARRY != 0
    }

    #[must_use]
    pub const fn overflow(self) -> bool {
        self.0 & Self::OVERFLOW != 0
    }

    pub(crate) fn set(&mut self, negative: bool, zero: bool, carry: bool, overflow: bool) {
        self.0 = u32::from(negative) << 31 | u32::from(zero) << 30 | u32::from(carry) << 29 | u32::from(overflow) << 28;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuState {
    pub registers: [u64; 31],
    /// Architectural V0..V31 state. Lane zero occupies the least-significant bits.
    pub vectors: [u128; 32],
    pub sp: u64,
    pub pc: u64,
    pub nzcv: Nzcv,
    pub tls: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub exclusive: Option<crate::ExclusiveReservation>,
}
pub type Aarch64CpuState = CpuState;

/// The half of the architectural state a staged scalar step can reach. Staging
/// through this type is what makes such a step physically unable to name a
/// vector register, and drops 512 bytes from the per-instruction copy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScalarStage {
    pub(crate) registers: [u64; 31],
    pub(crate) sp: u64,
    pub(crate) pc: u64,
    pub(crate) nzcv: Nzcv,
    pub(crate) tls: u64,
    pub(crate) fpcr: u64,
    pub(crate) fpsr: u64,
    pub(crate) exclusive: Option<crate::ExclusiveReservation>,
}

/// The scalar fields shared by the live state and a staging scratch, so a
/// helper can write either without knowing which it has.
pub(crate) trait ScalarAccess {
    fn registers(&self) -> &[u64; 31];
    fn registers_mut(&mut self) -> &mut [u64; 31];
    fn sp(&self) -> u64;
    fn set_sp(&mut self, value: u64);
    fn pc(&self) -> u64;
    fn set_pc(&mut self, value: u64);
    fn nzcv(&self) -> Nzcv;
    fn nzcv_mut(&mut self) -> &mut Nzcv;
    fn tls(&self) -> u64;
    fn set_tls(&mut self, value: u64);
    fn fpcr(&self) -> u64;
    fn set_fpcr(&mut self, value: u64);
    fn fpsr(&self) -> u64;
    fn set_fpsr(&mut self, value: u64);
    fn exclusive(&self) -> Option<crate::ExclusiveReservation>;

    fn write(&mut self, register: u8, value: u64) {
        if let Some(destination) = self.registers_mut().get_mut(usize::from(register)) {
            *destination = value;
        }
    }

    fn write_narrow(&mut self, register: u8, value: u32) {
        self.write(register, u64::from(value));
    }

    fn write_destination(&mut self, register: u8, value: u64) {
        if register == 31 {
            self.set_sp(value);
        } else {
            self.write(register, value);
        }
    }

    fn write_narrow_destination(&mut self, register: u8, value: u32) {
        self.write_destination(register, u64::from(value));
    }
}

macro_rules! scalar_access {
    ($type:ty) => {
        impl ScalarAccess for $type {
            fn registers(&self) -> &[u64; 31] {
                &self.registers
            }
            fn registers_mut(&mut self) -> &mut [u64; 31] {
                &mut self.registers
            }
            fn sp(&self) -> u64 {
                self.sp
            }
            fn set_sp(&mut self, value: u64) {
                self.sp = value;
            }
            fn pc(&self) -> u64 {
                self.pc
            }
            fn set_pc(&mut self, value: u64) {
                self.pc = value;
            }
            fn nzcv(&self) -> Nzcv {
                self.nzcv
            }
            fn nzcv_mut(&mut self) -> &mut Nzcv {
                &mut self.nzcv
            }
            fn tls(&self) -> u64 {
                self.tls
            }
            fn set_tls(&mut self, value: u64) {
                self.tls = value;
            }
            fn fpcr(&self) -> u64 {
                self.fpcr
            }
            fn set_fpcr(&mut self, value: u64) {
                self.fpcr = value;
            }
            fn fpsr(&self) -> u64 {
                self.fpsr
            }
            fn set_fpsr(&mut self, value: u64) {
                self.fpsr = value;
            }
            fn exclusive(&self) -> Option<crate::ExclusiveReservation> {
                self.exclusive
            }
        }
    };
}

scalar_access!(CpuState);
scalar_access!(ScalarStage);

impl Aarch64CpuState {
    pub fn vector(&self, register: u8) -> u128 {
        self.vectors.get(usize::from(register)).copied().unwrap_or(0)
    }

    pub fn set_vector(&mut self, register: u8, value: u128) {
        if let Some(destination) = self.vectors.get_mut(usize::from(register)) {
            *destination = value;
        }
    }

    pub(crate) fn vector_lane(&self, register: u8, lane_bits: u8, lane: u8) -> u64 {
        let shift = u32::from(lane_bits) * u32::from(lane);
        let mask = if lane_bits == 64 {
            u64::MAX
        } else {
            (1_u64 << lane_bits) - 1
        };
        ((self.vector(register) >> shift) as u64) & mask
    }

    pub(crate) fn set_vector_lane(&mut self, register: u8, lane_bits: u8, lane: u8, value: u64) {
        let shift = u32::from(lane_bits) * u32::from(lane);
        let mask = if lane_bits == 64 {
            u128::from(u64::MAX)
        } else {
            (1_u128 << lane_bits) - 1
        };
        let old = self.vector(register);
        self.set_vector(register, (old & !(mask << shift)) | (u128::from(value) & mask) << shift);
    }

    pub(crate) fn write_vector_width(&mut self, register: u8, value: u128, wide: bool) {
        self.set_vector(register, if wide { value } else { value & u128::from(u64::MAX) });
    }

    /// Drops the local monitor during fork, exec, migration, or explicit reset.
    pub fn clear_exclusive_reservation(&mut self) {
        self.exclusive = None;
    }

    pub fn register(&self, register: u8) -> u64 {
        self.registers.get(usize::from(register)).copied().unwrap_or(0)
    }

    pub fn set_register(&mut self, register: u8, value: u64) {
        if let Some(destination) = self.registers.get_mut(usize::from(register)) {
            *destination = value;
        }
    }

    pub(crate) fn register_or_sp(&self, register: u8) -> u64 {
        if register == 31 {
            self.sp
        } else {
            self.register(register)
        }
    }

    pub(crate) fn set_narrow_register(&mut self, register: u8, value: u32) {
        self.set_register(register, u64::from(value));
    }

    /// Snapshots only the scalar half, so a staged step that cannot name a
    /// vector register does not copy the 512-byte vector file to reach it.
    pub(crate) fn stage_scalar(&self) -> ScalarStage {
        ScalarStage {
            registers: self.registers,
            sp: self.sp,
            pc: self.pc,
            nzcv: self.nzcv,
            tls: self.tls,
            fpcr: self.fpcr,
            fpsr: self.fpsr,
            exclusive: self.exclusive,
        }
    }

    /// Commits a staged step that never wrote the vector file, skipping its 512-byte copy-back.
    pub(crate) fn commit_scalar<S: ScalarAccess>(&mut self, staged: &S) {
        self.registers = *staged.registers();
        self.sp = staged.sp();
        self.pc = staged.pc();
        self.nzcv = staged.nzcv();
        self.tls = staged.tls();
        self.fpcr = staged.fpcr();
        self.fpsr = staged.fpsr();
        self.exclusive = staged.exclusive();
    }
}
