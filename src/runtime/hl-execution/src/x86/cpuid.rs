#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostCapabilities {
    pub integer: bool,
    pub floating_point: bool,
    pub timestamp: bool,
    pub compare_exchange: bool,
    pub conditional_move: bool,
    pub mmx: bool,
    pub fxsave: bool,
    pub sse: bool,
    pub sse2: bool,
    pub population_count: bool,
    pub level_two: bool,
    pub crypto: bool,
    pub rep: bool,
    pub bmi1: bool,
    pub bmi2: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuidRegisters {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestFeaturePolicy {
    capabilities: HostCapabilities,
}

impl GuestFeaturePolicy {
    #[must_use]
    pub fn new(capabilities: HostCapabilities) -> Option<Self> {
        capabilities.integer.then_some(Self { capabilities })
    }

    #[must_use]
    pub const fn interpreter() -> Self {
        Self {
            capabilities: HostCapabilities {
                integer: true,
                floating_point: true,
                timestamp: true,
                compare_exchange: true,
                conditional_move: true,
                mmx: true,
                fxsave: true,
                sse: true,
                sse2: true,
                population_count: true,
                level_two: true,
                crypto: true,
                rep: true,
                bmi1: true,
                bmi2: true,
            },
        }
    }

    #[must_use]
    pub fn cpuid(self, leaf: u32, subleaf: u32) -> CpuidRegisters {
        let mut registers = CpuidRegisters::default();
        match leaf {
            0 => {
                registers = CpuidRegisters {
                    eax: 7,
                    ebx: 0x756e_6547,
                    ecx: 0x6c65_746e,
                    edx: 0x4965_6e69,
                }
            }
            1 => registers = self.leaf_one(),
            7 if subleaf == 0 => {
                if self.capabilities.crypto {
                    registers.ebx |= (1 << 8) | (1 << 29);
                }
                if self.capabilities.bmi1 {
                    registers.ebx |= 1 << 3;
                }
                if self.capabilities.bmi2 {
                    registers.ebx |= 1 << 8;
                }
                if self.capabilities.rep {
                    registers.ebx |= 1 << 9;
                    registers.edx |= 1 << 4;
                }
            }
            0x8000_0000 => registers.eax = 0x8000_0008,
            0x8000_0001 => {
                registers.edx = (1 << 11) | (1 << 20) | (1 << 29);
                if self.capabilities.timestamp {
                    registers.edx |= 1 << 27;
                }
                if self.capabilities.level_two {
                    registers.ecx = 1;
                }
            }
            0x8000_0007 if self.capabilities.timestamp => registers.edx = 1 << 8,
            0x8000_0002..=0x8000_0004 => {
                let mut brand = [0_u8; 48];
                brand[..23].copy_from_slice(b"hl JIT x86-64 processor");
                let offset = ((leaf - 0x8000_0002) * 16) as usize;
                registers.eax = u32::from_le_bytes(brand[offset..offset + 4].try_into().unwrap());
                registers.ebx = u32::from_le_bytes(brand[offset + 4..offset + 8].try_into().unwrap());
                registers.ecx = u32::from_le_bytes(brand[offset + 8..offset + 12].try_into().unwrap());
                registers.edx = u32::from_le_bytes(brand[offset + 12..offset + 16].try_into().unwrap());
            }
            0x8000_0008 => registers.eax = 0x3027,
            _ => {}
        }
        registers
    }

    fn leaf_one(self) -> CpuidRegisters {
        let mut registers = CpuidRegisters {
            eax: 0x0002_06c2,
            edx: (1 << 11) | (1 << 13) | (1 << 19),
            ..CpuidRegisters::default()
        };
        for (enabled, bit) in [
            (self.capabilities.floating_point, 0),
            (self.capabilities.timestamp, 4),
            (self.capabilities.compare_exchange, 8),
            (self.capabilities.conditional_move, 15),
            (self.capabilities.mmx, 23),
            (self.capabilities.fxsave, 24),
            (self.capabilities.sse, 25),
            (self.capabilities.sse2, 26),
        ] {
            if enabled {
                registers.edx |= 1 << bit;
            }
        }
        if self.capabilities.level_two {
            registers.ecx |= (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20);
        }
        if self.capabilities.population_count {
            registers.ecx |= 1 << 23;
        }
        if self.capabilities.crypto {
            registers.ecx |= (1 << 1) | (1 << 25);
        }
        registers
    }

    #[must_use]
    pub const fn bmi1(self) -> bool {
        self.capabilities.bmi1
    }

    pub fn xgetbv(self, index: u32) -> Result<u64, XgetbvError> {
        let _ = index;
        Err(XgetbvError::UndefinedInstruction)
    }

    pub fn apply(self, cpu: &mut crate::ScalarState) {
        let registers = self.cpuid(cpu.registers[0] as u32, cpu.registers[1] as u32);
        [cpu.registers[0], cpu.registers[3], cpu.registers[1], cpu.registers[2]] = [
            u64::from(registers.eax),
            u64::from(registers.ebx),
            u64::from(registers.ecx),
            u64::from(registers.edx),
        ];
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XgetbvError {
    UndefinedInstruction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_baseline() {
        let leaf = GuestFeaturePolicy::interpreter().cpuid(1, 0);
        let baseline = [0, 4, 8, 11, 13, 15, 19, 23, 24, 25, 26]
            .into_iter()
            .fold(0_u32, |bits, bit| bits | 1 << bit);
        assert_eq!(leaf.edx & baseline, baseline);
        assert_eq!(leaf.ecx, 0x0298_2203);
        assert_ne!(
            GuestFeaturePolicy::interpreter().cpuid(0x8000_0001, 0).edx & (1 << 27),
            0
        );
        assert_eq!(GuestFeaturePolicy::interpreter().cpuid(0x8000_0007, 0).edx, 1 << 8);
        assert_eq!(leaf.edx, 0x0788_a911);
        assert_eq!(GuestFeaturePolicy::interpreter().cpuid(7, 0).ebx, 0x2000_0308);
        assert_eq!(GuestFeaturePolicy::interpreter().cpuid(7, 0).edx, 1 << 4);
        assert_eq!(
            GuestFeaturePolicy::interpreter().xgetbv(0),
            Err(XgetbvError::UndefinedInstruction)
        );
    }

    #[test]
    fn population_count_is_an_independent_capability() {
        let enabled = GuestFeaturePolicy::new(HostCapabilities {
            integer: true,
            population_count: true,
            ..HostCapabilities::default()
        })
        .unwrap()
        .cpuid(1, 0);
        assert_eq!(enabled.ecx, 1 << 23);
        assert_eq!(enabled.ebx, 0);
        assert_eq!(enabled.edx, (1 << 11) | (1 << 13) | (1 << 19));

        let disabled = GuestFeaturePolicy::new(HostCapabilities {
            integer: true,
            ..HostCapabilities::default()
        })
        .unwrap()
        .cpuid(1, 0);
        assert_eq!(disabled.ecx, 0);
    }

    #[test]
    fn bmi_feature_coupling() {
        let leaf = GuestFeaturePolicy::interpreter().cpuid(7, 0);
        assert_ne!(leaf.ebx & (1 << 3), 0);
        assert_ne!(leaf.ebx & (1 << 8), 0);
        let disabled = GuestFeaturePolicy::new(HostCapabilities {
            integer: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(disabled.cpuid(7, 0).ebx & ((1 << 3) | (1 << 8)), 0);
    }
}
