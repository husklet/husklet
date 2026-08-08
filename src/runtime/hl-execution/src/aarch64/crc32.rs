use crate::{Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrcPolynomial {
    Ieee,
    Castagnoli,
}

pub(crate) struct Crc;

impl Crc {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let (polynomial, bytes) = match word & 0xffe0_fc00 {
            0x1ac0_4000 => (CrcPolynomial::Ieee, 1),
            0x1ac0_4400 => (CrcPolynomial::Ieee, 2),
            0x1ac0_4800 => (CrcPolynomial::Ieee, 4),
            0x9ac0_4c00 => (CrcPolynomial::Ieee, 8),
            0x1ac0_5000 => (CrcPolynomial::Castagnoli, 1),
            0x1ac0_5400 => (CrcPolynomial::Castagnoli, 2),
            0x1ac0_5800 => (CrcPolynomial::Castagnoli, 4),
            0x9ac0_5c00 => (CrcPolynomial::Castagnoli, 8),
            _ => return None,
        };
        Some(Aarch64Instruction::Crc32 {
            polynomial,
            bytes,
            checksum: (word >> 5 & 31) as u8,
            value: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn execute(cpu: &mut Aarch64CpuState, ir: &Aarch64Ir) -> Option<Aarch64ExecutionExit> {
        let Aarch64Instruction::Crc32 {
            polynomial,
            bytes,
            checksum,
            value,
            destination,
        } = ir.instruction
        else {
            return None;
        };
        let result = Self::update(polynomial, cpu.register(checksum) as u32, cpu.register(value), bytes);
        cpu.set_narrow_register(destination, result);
        cpu.pc = cpu.pc.wrapping_add(4);
        Some(Aarch64ExecutionExit::Continue)
    }

    fn update(polynomial: CrcPolynomial, mut checksum: u32, value: u64, bytes: u8) -> u32 {
        let polynomial = match polynomial {
            CrcPolynomial::Ieee => 0xedb8_8320,
            CrcPolynomial::Castagnoli => 0x82f6_3b78,
        };
        for byte in value.to_le_bytes().into_iter().take(usize::from(bytes)) {
            checksum ^= u32::from(byte);
            for _ in 0..8 {
                checksum = Self::bit(checksum, polynomial);
            }
        }
        checksum
    }

    fn bit(checksum: u32, polynomial: u32) -> u32 {
        (checksum >> 1) ^ if checksum & 1 != 0 { polynomial } else { 0 }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64DecodeError, Aarch64Decoder, Aarch64Interpreter, Nzcv, PcCoordinatePort};

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    fn reference(polynomial: CrcPolynomial, checksum: u32, value: u64, bytes: u8) -> u32 {
        let polynomial = match polynomial {
            CrcPolynomial::Ieee => 0xedb8_8320,
            CrcPolynomial::Castagnoli => 0x82f6_3b78,
        };
        let mut table = [0_u32; 256];
        for index in 0_u32..256 {
            table[index as usize] = reference_entry(index, polynomial);
        }
        value
            .to_le_bytes()
            .into_iter()
            .take(usize::from(bytes))
            .fold(checksum, |crc, byte| crc >> 8 ^ table[((crc as u8) ^ byte) as usize])
    }

    fn reference_entry(mut entry: u32, polynomial: u32) -> u32 {
        for _ in 0..8 {
            entry = (entry >> 1) ^ if entry & 1 != 0 { polynomial } else { 0 };
        }
        entry
    }

    #[test]
    fn decode_family() {
        for (base, polynomial, bytes) in [
            (0x1ac0_4000, CrcPolynomial::Ieee, 1),
            (0x1ac0_4400, CrcPolynomial::Ieee, 2),
            (0x1ac0_4800, CrcPolynomial::Ieee, 4),
            (0x9ac0_4c00, CrcPolynomial::Ieee, 8),
            (0x1ac0_5000, CrcPolynomial::Castagnoli, 1),
            (0x1ac0_5400, CrcPolynomial::Castagnoli, 2),
            (0x1ac0_5800, CrcPolynomial::Castagnoli, 4),
            (0x9ac0_5c00, CrcPolynomial::Castagnoli, 8),
        ] {
            for index in 0_u32..32 * 32 * 32 {
                let checksum = index / 1024;
                let value = index / 32 % 32;
                let destination = index % 32;
                let word = base | value << 16 | checksum << 5 | destination;
                assert_eq!(
                    Aarch64Decoder::decode(word).unwrap().instruction,
                    Aarch64Instruction::Crc32 {
                        polynomial,
                        bytes,
                        checksum: checksum as u8,
                        value: value as u8,
                        destination: destination as u8,
                    }
                );
            }
        }
        for word in [0x9ac0_4000, 0x1ac0_4c00, 0x1ac0_5c00, 0x9ac0_5000] {
            assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Unsupported));
        }
    }

    #[test]
    fn differential_values() {
        let mut random = 0x9e37_79b9_7f4a_7c15_u64;
        for (polynomial, bytes) in [
            (CrcPolynomial::Ieee, 1_u8),
            (CrcPolynomial::Ieee, 2),
            (CrcPolynomial::Ieee, 4),
            (CrcPolynomial::Ieee, 8),
            (CrcPolynomial::Castagnoli, 1),
            (CrcPolynomial::Castagnoli, 2),
            (CrcPolynomial::Castagnoli, 4),
            (CrcPolynomial::Castagnoli, 8),
        ] {
            for _ in 0..256 {
                random = random
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let checksum = (random >> 17) as u32;
                random = random.rotate_left(29) ^ 0xa5a5_5a5a_d3c4_b2e1;
                assert_eq!(
                    Crc::update(polynomial, checksum, random, bytes),
                    reference(polynomial, checksum, random, bytes)
                );
            }
        }
    }

    #[test]
    fn architectural_state() {
        for (word, bytes, polynomial) in [
            (0x1ac2_4020, 1, CrcPolynomial::Ieee),
            (0x1ac2_4420, 2, CrcPolynomial::Ieee),
            (0x1ac2_4820, 4, CrcPolynomial::Ieee),
            (0x9ac2_4c20, 8, CrcPolynomial::Ieee),
            (0x1ac2_5020, 1, CrcPolynomial::Castagnoli),
            (0x1ac2_5420, 2, CrcPolynomial::Castagnoli),
            (0x1ac2_5820, 4, CrcPolynomial::Castagnoli),
            (0x9ac2_5c20, 8, CrcPolynomial::Castagnoli),
        ] {
            let mut cpu = Aarch64CpuState {
                pc: 0x500,
                nzcv: Nzcv::from_bits(0xf000_0000),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.registers[0] = u64::MAX;
            cpu.registers[1] = 0x89ab_cdef;
            cpu.registers[2] = 0xfedc_ba98_7654_3210;
            let expected = reference(polynomial, cpu.registers[1] as u32, cpu.registers[2], bytes);
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.registers[0], u64::from(expected));
            assert_eq!(cpu.pc, 0x504);
            assert_eq!(cpu.nzcv.bits(), 0xf000_0000);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }
}
