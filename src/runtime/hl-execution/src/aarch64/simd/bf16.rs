use crate::{Aarch64CpuState, Aarch64Instruction};

pub(crate) struct Bf16;

impl Bf16 {
    pub(crate) fn decode_scalar(word: u32) -> Option<Aarch64Instruction> {
        (word & 0xffff_fc00 == 0x1e63_4000).then(|| Self::instruction(word, false, false))
    }

    pub(crate) fn decode_vector(word: u32) -> Option<Aarch64Instruction> {
        (word & 0xbfff_fc00 == 0x0ea1_6800).then(|| Self::instruction(word, true, word >> 30 & 1 != 0))
    }

    fn instruction(word: u32, vector: bool, high: bool) -> Aarch64Instruction {
        Aarch64Instruction::Bf16Convert {
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            vector,
            high,
        }
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        source: u8,
        destination: u8,
        vector: bool,
        high: bool,
    ) {
        let lanes = if vector { 4 } else { 1 };
        let mut value = if high {
            cpu.vector(destination) & u128::from(u64::MAX)
        } else {
            0
        };
        let offset = if high { 64 } else { 0 };
        for lane in 0..lanes {
            let converted = Self::convert(cpu.vector_lane(source, 32, lane), cpu.fpcr as u32);
            value |= u128::from(converted) << (offset + lane * 16);
        }
        staged.set_vector(destination, value);
    }

    fn convert(bits: u64, fpcr: u32) -> u16 {
        let bits = bits as u32;
        let exponent = bits >> 23 & 0xff;
        let fraction = bits & 0x7f_ffff;
        if exponent == 0xff && fraction != 0 {
            return 0x7fc0;
        }
        let sign = bits >> 31 != 0;
        if fpcr >> 24 & 1 != 0 && exponent == 0 && fraction != 0 {
            return if sign { 0x8000 } else { 0 };
        }
        let upper = (bits >> 16) as u16;
        let discarded = bits & 0xffff;
        let increment = match fpcr >> 22 & 3 {
            0 => discarded > 0x8000 || discarded == 0x8000 && upper & 1 != 0,
            1 => !sign && discarded != 0,
            2 => sign && discarded != 0,
            _ => false,
        };
        upper.wrapping_add(u16::from(increment))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat};

    #[test]
    fn exhaustive_encodings() {
        for encoded in 0_u32..32 * 32 {
            let source = (encoded / 32) as u8;
            let destination = (encoded % 32) as u8;
            assert_eq!(
                Bf16::decode_scalar(0x1e63_4000 | encoded),
                Some(Aarch64Instruction::Bf16Convert {
                    source,
                    destination,
                    vector: false,
                    high: false
                })
            );
            for (base, high) in [(0x0ea1_6800, false), (0x4ea1_6800, true)] {
                assert_eq!(
                    Bf16::decode_vector(base | encoded),
                    Some(Aarch64Instruction::Bf16Convert {
                        source,
                        destination,
                        vector: true,
                        high
                    })
                );
            }
        }
        for word in [0x1e62_4000, 0x0ea1_6400, 0x2ea1_6800] {
            assert_eq!(Bf16::decode_scalar(word), None);
            assert_eq!(Bf16::decode_vector(word), None);
        }
    }

    #[test]
    fn boundaries() {
        let cases = [
            (0x3f80_0000, 0x3f80),
            (0x3f81_8000, 0x3f82),
            (0x3f80_8000, 0x3f80),
            (0x3f80_8001, 0x3f81),
            (0x7f80_0001, 0x7fc0),
            (0xffc1_2345, 0x7fc0),
            (0x7f7f_ffff, 0x7f80),
            (0x8000_0000, 0x8000),
            (0x0001_8000, 0x0002),
        ];
        for (input, expected) in cases {
            assert_eq!(Bf16::convert(input, 0), expected);
        }
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(31, u128::MAX << 32 | 0x3f80_0000);
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x1e63_43ff),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), 0x3f80);
    }

    #[test]
    fn modes_aliases() {
        let input = 0x3f80_0001_u32;
        assert_eq!(Bf16::convert(input.into(), 0), 0x3f80);
        assert_eq!(Bf16::convert(input.into(), 1 << 22), 0x3f81);
        assert_eq!(Bf16::convert(input.into(), 2 << 22), 0x3f80);
        assert_eq!(Bf16::convert(1, 1 << 24), 0);
        let source = 0x4080_0000_4040_0000_4000_0000_3f80_0000_u128;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(0, source);
        execute(&mut cpu, 0x0ea1_6800);
        assert_eq!(cpu.vector(0), 0x4080_4040_4000_3f80);
        cpu.pc = 0;
        cpu.set_vector(0, source);
        execute(&mut cpu, 0x4ea1_6800);
        assert_eq!(cpu.vector(0), 0x4080_4040_4000_3f80_4000_0000_3f80_0000);
    }

    #[test]
    fn random_reference() {
        let mut state = 0x9e37_79b9_u32;
        for fpcr in [0, 1 << 22, 2 << 22, 3 << 22, 1 << 24] {
            for _ in 0..10_000 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                assert_eq!(Bf16::convert(state.into(), fpcr), reference(state, fpcr));
            }
        }
    }

    fn reference(bits: u32, fpcr: u32) -> u16 {
        if bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0 {
            return 0x7fc0;
        }
        if fpcr & 1 << 24 != 0 && bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0 {
            return (bits >> 16) as u16 & 0x8000;
        }
        let kept = bits >> 16;
        let lost = bits & 0xffff;
        let negative = bits >> 31 != 0;
        let add = match fpcr >> 22 & 3 {
            0 => lost > 0x8000 || lost == 0x8000 && kept & 1 != 0,
            1 => !negative && lost != 0,
            2 => negative && lost != 0,
            _ => false,
        };
        kept.wrapping_add(u32::from(add)) as u16
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        assert_eq!(
            Aarch64FpExecutor::execute_word(cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
    }
}
