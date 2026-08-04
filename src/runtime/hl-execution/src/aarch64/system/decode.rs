use crate::{Aarch64DecodeError, Aarch64Instruction, BarrierKind, SystemRegister};

pub(crate) struct Decoder;

impl Decoder {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        // Architectural hints occupy one system-instruction space with Rt fixed
        // to XZR. At EL0 they have no observable effect unless an advertised
        // optional feature assigns one. The engine does not advertise BTI or
        // pointer authentication, so accepting the complete hint space also
        // covers newer forward-compatible hints without special cases.
        if word & 0xffff_f01f == 0xd503_201f {
            return Some(Ok(Aarch64Instruction::Nop));
        }
        let barrier = word & 0xffff_f0ff;
        if barrier == 0xd503_305f {
            return Some(Ok(Aarch64Instruction::ClearExclusive));
        }
        let kind = match barrier {
            0xd503_30bf => Some(BarrierKind::DataMemory),
            0xd503_309f => Some(BarrierKind::DataSynchronization),
            0xd503_30df => Some(BarrierKind::InstructionSynchronization),
            _ => None,
        };
        if let Some(kind) = kind {
            return Some(Ok(Aarch64Instruction::Barrier {
                kind,
                option: (word >> 8 & 15) as u8,
            }));
        }
        // Translated execution reads guest instructions as coherent data and
        // never instruction-fetches from the guest mapping, so cleaning that
        // mapping to the point of unification needs no host operation.
        if word & 0xffff_ffe0 == 0xd50b_7b20 {
            return Some(Ok(Aarch64Instruction::Nop));
        }
        if word & 0xffff_ffe0 == 0xd50b_7420 {
            return Some(Ok(Aarch64Instruction::CacheZero {
                source: (word & 31) as u8,
            }));
        }
        if word & 0xffff_ffe0 == 0xd50b_7520 {
            return Some(Ok(Aarch64Instruction::InstructionCache {
                source: (word & 31) as u8,
            }));
        }
        if word & 0xffd0_0000 != 0xd510_0000 {
            return None;
        }
        Some(Self::register(word))
    }

    fn register(word: u32) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let read = word >> 21 & 1 != 0;
        if read && word & 0xffff_0000 == 0xd538_0000 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let encoding = word & 0xffff_ffe0;
        let register = match encoding {
            0xd53b_0020 => SystemRegister::CacheType,
            0xd53b_00e0 => SystemRegister::ZeroBlock,
            0xd53b_4200 | 0xd51b_4200 => SystemRegister::Nzcv,
            0xd53b_4220 | 0xd51b_4220 => SystemRegister::InterruptMask,
            0xd53b_4400 | 0xd51b_4400 => SystemRegister::Fpcr,
            0xd53b_4420 | 0xd51b_4420 => SystemRegister::Fpsr,
            0xd53b_d040 | 0xd51b_d040 => SystemRegister::ThreadPointer,
            0xd53b_d060 => SystemRegister::ReadonlyThreadPointer,
            0xd53b_e020 | 0xd53b_e040 | 0xd53b_e0c0 => SystemRegister::CounterValue,
            0xd53b_e000 => SystemRegister::CounterFrequency,
            _ => return Err(Aarch64DecodeError::Unsupported),
        };
        if read {
            Ok(Aarch64Instruction::SystemRead {
                destination: (word & 31) as u8,
                register,
            })
        } else if matches!(
            register,
            SystemRegister::CacheType
                | SystemRegister::ZeroBlock
                | SystemRegister::ReadonlyThreadPointer
                | SystemRegister::CounterValue
                | SystemRegister::CounterFrequency
        ) {
            Err(Aarch64DecodeError::Reserved)
        } else {
            Ok(Aarch64Instruction::SystemWrite {
                source: (word & 31) as u8,
                register,
            })
        }
    }
}
