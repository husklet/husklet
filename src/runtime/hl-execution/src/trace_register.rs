use crate::{Aarch64CpuState, CpuState, FlagState, Nzcv};

pub const TRACE_REGISTER_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoppedRegisterImage {
    version: u32,
    registers: StoppedRegisters,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoppedRegisters {
    X86(X86Prstatus),
    Aarch64(Aarch64Prstatus),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X86Prstatus {
    words: [u64; 27],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aarch64Prstatus {
    words: [u64; 34],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceRegisterError {
    Length,
    Architecture,
    Version,
}

pub trait TraceSafepointPort: Send + Sync {
    fn publish(&self, image: StoppedRegisterImage) -> Result<(), TraceRegisterError>;
    fn restore(&self) -> Result<StoppedRegisterImage, TraceRegisterError>;
}

impl StoppedRegisterImage {
    #[must_use]
    pub const fn new(registers: StoppedRegisters) -> Self {
        Self {
            version: TRACE_REGISTER_VERSION,
            registers,
        }
    }

    pub fn restore(self) -> Result<StoppedRegisters, TraceRegisterError> {
        if self.version != TRACE_REGISTER_VERSION {
            return Err(TraceRegisterError::Version);
        }
        Ok(self.registers)
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn registers(&self) -> &StoppedRegisters {
        &self.registers
    }
}

impl X86Prstatus {
    pub const BYTES: usize = 27 * 8;

    #[must_use]
    pub fn capture(cpu: &CpuState, original_syscall: u64) -> Self {
        let r = &cpu.registers;
        let mut words = [0; 27];
        words[..16].copy_from_slice(&[
            r[15],
            r[14],
            r[13],
            r[12],
            r[5],
            r[3],
            r[11],
            r[10],
            r[9],
            r[8],
            r[0],
            r[1],
            r[2],
            r[6],
            r[7],
            original_syscall,
        ]);
        words[16] = cpu.rip;
        words[17] = 0x33;
        words[18] = u64::from(cpu.flags.bits()) | 2;
        words[19] = r[4];
        words[20] = 0x2b;
        words[21] = cpu.fs_base;
        words[22] = cpu.gs_base;
        Self { words }
    }

    pub fn apply(&self, cpu: &mut CpuState) {
        let g = &self.words;
        let r = &mut cpu.registers;
        r[15] = g[0];
        r[14] = g[1];
        r[13] = g[2];
        r[12] = g[3];
        r[5] = g[4];
        r[3] = g[5];
        r[11] = g[6];
        r[10] = g[7];
        r[9] = g[8];
        r[8] = g[9];
        r[0] = g[10];
        r[1] = g[11];
        r[2] = g[12];
        r[6] = g[13];
        r[7] = g[14];
        r[4] = g[19];
        cpu.rip = g[16];
        cpu.flags = FlagState::from_bits(g[18] as u16);
        cpu.fs_base = g[21];
        cpu.gs_base = g[22];
    }

    #[must_use]
    pub const fn words(&self) -> &[u64; 27] {
        &self.words
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TraceRegisterError> {
        if bytes.len() != Self::BYTES {
            return Err(TraceRegisterError::Length);
        }
        let mut words = [0; 27];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().map_err(|_| TraceRegisterError::Length)?);
        }
        Ok(Self { words })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}

impl Aarch64Prstatus {
    pub const BYTES: usize = 34 * 8;

    #[must_use]
    pub fn capture(cpu: &Aarch64CpuState) -> Self {
        let mut words = [0; 34];
        words[..31].copy_from_slice(&cpu.registers);
        words[31] = cpu.sp;
        words[32] = cpu.pc;
        words[33] = u64::from(cpu.nzcv.bits());
        Self { words }
    }

    pub fn apply(&self, cpu: &mut Aarch64CpuState) {
        cpu.registers.copy_from_slice(&self.words[..31]);
        cpu.sp = self.words[31];
        cpu.pc = self.words[32];
        cpu.nzcv = Nzcv::from_bits(self.words[33] as u32);
    }

    #[must_use]
    pub const fn words(&self) -> &[u64; 34] {
        &self.words
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TraceRegisterError> {
        if bytes.len() != Self::BYTES {
            return Err(TraceRegisterError::Length);
        }
        let mut words = [0; 34];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().map_err(|_| TraceRegisterError::Length)?);
        }
        Ok(Self { words })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Aarch64Prstatus, StoppedRegisterImage, StoppedRegisters, TRACE_REGISTER_VERSION, TraceRegisterError,
        X86Prstatus,
    };
    use crate::{Aarch64CpuState, CpuState, FlagState, Nzcv};

    #[test]
    fn x86_linux_order() {
        let mut cpu = CpuState::default();
        for (index, register) in cpu.registers.iter_mut().enumerate() {
            *register = index as u64 + 10;
        }
        cpu.rip = 0x1234;
        cpu.flags = FlagState::from_bits(0x8d5);
        cpu.fs_base = 0x55;
        cpu.gs_base = 0x66;
        let image = X86Prstatus::capture(&cpu, 59);
        assert_eq!(&image.words()[..4], &[25, 24, 23, 22]);
        assert_eq!(image.words()[15], 59);
        assert_eq!(image.words()[17], 0x33);
        assert_eq!(image.words()[20], 0x2b);
        assert_eq!(image.encode().len(), X86Prstatus::BYTES);
        let decoded = X86Prstatus::decode(&image.encode()).unwrap();
        let mut restored = CpuState::default();
        decoded.apply(&mut restored);
        assert_eq!(restored.registers, cpu.registers);
        assert_eq!(
            (restored.rip, restored.fs_base, restored.gs_base),
            (cpu.rip, cpu.fs_base, cpu.gs_base)
        );
    }

    #[test]
    fn aarch64_linux_order() {
        let mut cpu = Aarch64CpuState::default();
        for (index, register) in cpu.registers.iter_mut().enumerate() {
            *register = index as u64 + 100;
        }
        cpu.sp = 0x1000;
        cpu.pc = 0x2000;
        cpu.nzcv = Nzcv::from_bits(0xf000_0000);
        let image = Aarch64Prstatus::capture(&cpu);
        assert_eq!(image.words()[30], 130);
        assert_eq!(&image.words()[31..], &[0x1000, 0x2000, 0xf000_0000]);
        let mut restored = Aarch64CpuState::default();
        Aarch64Prstatus::decode(&image.encode()).unwrap().apply(&mut restored);
        assert_eq!(restored.registers, cpu.registers);
        assert_eq!((restored.sp, restored.pc, restored.nzcv), (cpu.sp, cpu.pc, cpu.nzcv));
    }

    #[test]
    fn exact_lengths() {
        assert_eq!(X86Prstatus::decode(&[0; 215]), Err(TraceRegisterError::Length));
        assert_eq!(Aarch64Prstatus::decode(&[0; 273]), Err(TraceRegisterError::Length));
    }

    #[test]
    fn image_version() {
        let registers = StoppedRegisters::X86(X86Prstatus::capture(&CpuState::default(), 0));
        let image = StoppedRegisterImage::new(registers.clone());
        assert_eq!(image.version(), TRACE_REGISTER_VERSION);
        assert_eq!(image.registers(), &registers);
        assert_eq!(image.restore(), Ok(registers));

        let stale = StoppedRegisterImage {
            version: TRACE_REGISTER_VERSION + 1,
            registers: StoppedRegisters::Aarch64(Aarch64Prstatus::capture(&Aarch64CpuState::default())),
        };
        assert_eq!(stale.restore(), Err(TraceRegisterError::Version));
    }
}
