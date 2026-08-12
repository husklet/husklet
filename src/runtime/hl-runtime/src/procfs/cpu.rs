use hl_vfs::{ProcfsCpuModel, ProcfsCpuTicks};

#[derive(Clone, Copy, Debug, Default)]
struct CpuidRegisters {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

/// Stable x86 userspace feature surface exposed by the retained engine.
#[derive(Clone, Copy)]
struct GuestFeaturePolicy;

impl GuestFeaturePolicy {
    const fn interpreter() -> Self {
        Self
    }

    fn cpuid(self, leaf: u32, subleaf: u32) -> CpuidRegisters {
        let mut registers = CpuidRegisters::default();
        match leaf {
            0 => {
                registers = CpuidRegisters {
                    eax: 7,
                    ebx: 0x756e_6547,
                    ecx: 0x6c65_746e,
                    edx: 0x4965_6e69,
                };
            }
            1 => {
                registers.eax = 0x0002_06c2;
                registers.ecx = 0x0298_2203;
                registers.edx = 0x0788_a911;
            }
            7 if subleaf == 0 => {
                registers.ebx = 0x2000_0308;
                registers.edx = 1 << 4;
            }
            0x8000_0000 => registers.eax = 0x8000_0008,
            0x8000_0001 => {
                registers.ecx = 1;
                registers.edx = (1 << 11) | (1 << 20) | (1 << 27) | (1 << 29);
            }
            0x8000_0007 => registers.edx = 1 << 8,
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
}

pub trait CpuPort: Send + Sync {
    fn ticks(&self, online: usize) -> Vec<ProcfsCpuTicks>;
}

/// Derives guest-visible CPU discovery from the execution policy.
pub struct CpuPolicy;

impl CpuPolicy {
    #[must_use]
    pub fn model(architecture: hl_isa::GuestArchitecture, features: hl_loader::GuestFeatures) -> ProcfsCpuModel {
        match architecture {
            hl_isa::GuestArchitecture::Aarch64 => ProcfsCpuModel::Aarch64 {
                hardware: features.hardware,
                hardware_second: features.hardware_second,
            },
            hl_isa::GuestArchitecture::X86_64 => Self::x86(),
        }
    }

    fn x86() -> ProcfsCpuModel {
        let policy = GuestFeaturePolicy::interpreter();
        let root = policy.cpuid(0, 0);
        let leaf = policy.cpuid(1, 0);
        let mut vendor = Vec::with_capacity(12);
        vendor.extend_from_slice(&root.ebx.to_le_bytes());
        vendor.extend_from_slice(&root.edx.to_le_bytes());
        vendor.extend_from_slice(&root.ecx.to_le_bytes());
        let vendor = String::from_utf8(vendor).unwrap_or_default();
        let mut brand = Vec::with_capacity(48);
        for number in 0x8000_0002..=0x8000_0004 {
            let value = policy.cpuid(number, 0);
            for register in [value.eax, value.ebx, value.ecx, value.edx] {
                brand.extend_from_slice(&register.to_le_bytes());
            }
        }
        let name = String::from_utf8(brand)
            .unwrap_or_default()
            .trim_matches(char::from(0))
            .trim()
            .to_owned();
        let base_family = (leaf.eax >> 8) & 15;
        let family = base_family + u32::from(base_family == 15) * ((leaf.eax >> 20) & 255);
        let base_model = (leaf.eax >> 4) & 15;
        let model = base_model + u32::from(matches!(base_family, 6 | 15)) * (((leaf.eax >> 16) & 15) << 4);
        let flags = [
            (1, 0, 3, 0, "fpu"),
            (1, 0, 3, 4, "tsc"),
            (1, 0, 3, 8, "cx8"),
            (1, 0, 3, 11, "sep"),
            (1, 0, 3, 13, "pge"),
            (1, 0, 3, 15, "cmov"),
            (1, 0, 3, 19, "clflush"),
            (1, 0, 3, 23, "mmx"),
            (1, 0, 3, 24, "fxsr"),
            (1, 0, 3, 25, "sse"),
            (1, 0, 3, 26, "sse2"),
            (0x8000_0001, 0, 3, 11, "syscall"),
            (0x8000_0001, 0, 3, 20, "nx"),
            (0x8000_0001, 0, 3, 27, "rdtscp"),
            (0x8000_0001, 0, 3, 29, "lm"),
            (0x8000_0007, 0, 3, 8, "constant_tsc"),
            (0x8000_0007, 0, 3, 8, "nonstop_tsc"),
            (0x8000_0001, 0, 3, 29, "cpuid"),
            (0x8000_0001, 0, 3, 29, "nopl"),
            (1, 0, 2, 0, "pni"),
            (1, 0, 2, 1, "pclmulqdq"),
            (1, 0, 2, 9, "ssse3"),
            (1, 0, 2, 13, "cx16"),
            (1, 0, 2, 19, "sse4_1"),
            (1, 0, 2, 20, "sse4_2"),
            (1, 0, 2, 22, "movbe"),
            (1, 0, 2, 23, "popcnt"),
            (1, 0, 2, 25, "aes"),
            (0x8000_0001, 0, 2, 0, "lahf_lm"),
            (7, 0, 1, 3, "bmi1"),
            (7, 0, 1, 8, "bmi2"),
            (7, 0, 1, 9, "erms"),
            (7, 0, 1, 29, "sha_ni"),
            (7, 0, 3, 4, "fsrm"),
        ]
        .into_iter()
        .filter_map(|(leaf, subleaf, register, bit, name)| {
            let value = policy.cpuid(leaf, subleaf);
            let registers = [value.eax, value.ebx, value.ecx, value.edx];
            (registers[register] & (1 << bit) != 0).then_some(name)
        })
        .collect();
        ProcfsCpuModel::X86_64 {
            vendor,
            family,
            model,
            stepping: leaf.eax & 15,
            name,
            flags,
        }
    }
}

#[cfg(test)]
mod test {
    use super::{CpuPolicy, GuestFeaturePolicy};

    #[test]
    fn x86_flags_follow_cpuid() {
        let hl_vfs::ProcfsCpuModel::X86_64 { flags, .. } = CpuPolicy::x86() else {
            panic!("x86 policy returned another architecture");
        };
        let policy = GuestFeaturePolicy::interpreter();
        for (leaf, register, bit, name) in [(1, 2, 22, "movbe"), (0x8000_0001, 2, 0, "lahf_lm")] {
            let value = policy.cpuid(leaf, 0);
            let registers = [value.eax, value.ebx, value.ecx, value.edx];
            assert_eq!(flags.contains(&name), registers[register] & (1 << bit) != 0);
        }
    }
}
