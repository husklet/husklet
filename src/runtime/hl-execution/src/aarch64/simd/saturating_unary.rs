// Reserved-encoding predicates mirror the manual's condition tables.
#![allow(clippy::nonminimal_bool)]

use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction};

pub(crate) struct Saturation;

impl Saturation {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let (scalar, accumulate) = match word & 0x9f3f_fc00 {
            0x0e20_3800 => (false, true),
            0x1e20_3800 => (true, true),
            0x0e20_7800 => (false, false),
            0x1e20_7800 => (true, false),
            _ => return None,
        };
        let size = (word >> 22 & 3) as u8;
        let wide = word >> 30 & 1 != 0;
        if scalar && !wide || !scalar && size == 3 && !wide {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let lane_bits = 8 << size;
        let source = (word >> 5 & 31) as u8;
        let destination = (word & 31) as u8;
        let lanes = if scalar {
            1
        } else {
            (if wide { 128 } else { 64 }) / lane_bits
        };
        if accumulate {
            return Some(Ok(Aarch64Instruction::SimdSaturatingAccumulate {
                unsigned_destination: word >> 29 & 1 != 0,
                lane_bits,
                source,
                destination,
                lanes,
            }));
        }
        Some(Ok(Aarch64Instruction::SimdSaturatingUnary {
            negate: word >> 29 & 1 != 0,
            lane_bits,
            source,
            destination,
            lanes,
        }))
    }

    pub(crate) fn execute(cpu: &Aarch64CpuState, negate: bool, lane_bits: u8, source: u8, lanes: u8) -> (u128, bool) {
        let sign = 1_u128 << (lane_bits - 1);
        let mask = sign * 2 - 1;
        let mut value = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let raw = u128::from(cpu.vector_lane(source, lane_bits, lane));
            let result = if raw == sign {
                saturated = true;
                sign - 1
            } else if negate || raw & sign != 0 {
                raw.wrapping_neg() & mask
            } else {
                raw
            };
            value |= result << (u32::from(lane) * u32::from(lane_bits));
        }
        (value, saturated)
    }

    pub(crate) fn apply(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        negate: bool,
        lane_bits: u8,
        source: u8,
        destination: u8,
        lanes: u8,
    ) {
        let (value, saturated) = Self::execute(cpu, negate, lane_bits, source, lanes);
        staged.set_vector(destination, value);
        staged.fpsr |= u64::from(saturated) << 27;
    }

    pub(crate) fn accumulate(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        unsigned_destination: bool,
        lane_bits: u8,
        source: u8,
        destination: u8,
        lanes: u8,
    ) {
        let sign = 1_u128 << (lane_bits - 1);
        let mask = sign * 2 - 1;
        let mut value = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let source = u128::from(cpu.vector_lane(source, lane_bits, lane));
            let destination = u128::from(cpu.vector_lane(destination, lane_bits, lane));
            let source = if unsigned_destination {
                Self::signed(source, sign, mask)
            } else {
                source as i128
            };
            let destination = if unsigned_destination {
                destination as i128
            } else {
                Self::signed(destination, sign, mask)
            };
            let exact = destination + source;
            let minimum = if unsigned_destination { 0 } else { -(sign as i128) };
            let maximum = if unsigned_destination {
                mask as i128
            } else {
                sign as i128 - 1
            };
            let result = exact.clamp(minimum, maximum);
            saturated |= result != exact;
            value |= (result as u128 & mask) << (u32::from(lane) * u32::from(lane_bits));
        }
        staged.set_vector(destination, value);
        staged.fpsr |= u64::from(saturated) << 27;
    }

    fn signed(value: u128, sign: u128, mask: u128) -> i128 {
        if value & sign == 0 {
            value as i128
        } else {
            (value | !mask) as i128
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction,
        Aarch64Interpreter, Nzcv, PcCoordinatePort,
    };

    struct Coordinates;

    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    #[derive(Clone, Copy)]
    struct Shape {
        scalar: bool,
        size: u32,
        wide: bool,
        lane_bits: u8,
        lanes: u8,
    }

    impl Shape {
        const ALL: [Self; 11] = [
            Self::new(false, 0, false, 8, 8),
            Self::new(false, 1, false, 16, 4),
            Self::new(false, 2, false, 32, 2),
            Self::new(false, 0, true, 8, 16),
            Self::new(false, 1, true, 16, 8),
            Self::new(false, 2, true, 32, 4),
            Self::new(false, 3, true, 64, 2),
            Self::new(true, 0, false, 8, 1),
            Self::new(true, 1, false, 16, 1),
            Self::new(true, 2, false, 32, 1),
            Self::new(true, 3, false, 64, 1),
        ];

        const fn new(scalar: bool, size: u32, wide: bool, lane_bits: u8, lanes: u8) -> Self {
            Self {
                scalar,
                size,
                wide,
                lane_bits,
                lanes,
            }
        }

        fn word(self, negate: bool, source: u32, destination: u32) -> u32 {
            let base = if self.scalar { 0x5e20_7800 } else { 0x0e20_7800 };
            base | u32::from(negate) << 29 | u32::from(self.wide) << 30 | self.size << 22 | source << 5 | destination
        }

        fn accumulate_word(self, unsigned_destination: bool, source: u32, destination: u32) -> u32 {
            let base = if self.scalar { 0x5e20_3800 } else { 0x0e20_3800 };
            base | u32::from(unsigned_destination) << 29
                | u32::from(self.wide) << 30
                | self.size << 22
                | source << 5
                | destination
        }

        fn assert_encoding(self, negate: bool) {
            for source in 0_u32..32 {
                for destination in 0_u32..32 {
                    let word = self.word(negate, source, destination);
                    assert_eq!(
                        Aarch64Decoder::decode(word).unwrap().instruction,
                        Aarch64Instruction::SimdSaturatingUnary {
                            negate,
                            lane_bits: self.lane_bits,
                            source: source as u8,
                            destination: destination as u8,
                            lanes: self.lanes
                        }
                    );
                }
            }
        }

        fn assert_execution(self, negate: bool, flags: u32) {
            let sign = 1_u128 << (self.lane_bits - 1);
            let mask = sign * 2 - 1;
            let samples = [sign, mask, sign - 1, 1];
            let mut source = 0_u128;
            let mut expected = 0_u128;
            for lane in 0..self.lanes {
                let raw = samples[usize::from(lane) % samples.len()];
                source |= raw << (u32::from(lane) * u32::from(self.lane_bits));
                let result = if raw == sign {
                    sign - 1
                } else if negate || raw & sign != 0 {
                    raw.wrapping_neg() & mask
                } else {
                    raw
                };
                expected |= result << (u32::from(lane) * u32::from(self.lane_bits));
            }
            let word = self.word(negate, 31, 31);
            let mut cpu = Aarch64CpuState {
                pc: 0x400b08,
                nzcv: Nzcv::from_bits(flags),
                fpcr: 0x3344,
                fpsr: 0x95,
                ..Default::default()
            };
            cpu.set_vector(31, source | if self.scalar { u128::MAX << self.lane_bits } else { 0 });
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(31), expected, "{word:#010x}");
            assert_eq!((cpu.pc, cpu.nzcv.bits(), cpu.fpcr), (0x400b0c, flags, 0x3344));
            assert_eq!(cpu.fpsr, 0x0800_0095);
        }

        fn assert_accumulate(self, unsigned_destination: bool, flags: u32) {
            let sign = 1_u128 << (self.lane_bits - 1);
            let mask = sign * 2 - 1;
            let destinations = if unsigned_destination {
                [mask - 1, 0, 1, mask]
            } else {
                [sign - 2, sign, 1, mask]
            };
            let sources = if unsigned_destination {
                [2, mask, sign, 1]
            } else {
                [2, 1, mask, 0]
            };
            let mut destination = 0_u128;
            let mut source = 0_u128;
            let mut expected = 0_u128;
            for lane in 0..self.lanes {
                let index = usize::from(lane) % destinations.len();
                let shift = u32::from(lane) * u32::from(self.lane_bits);
                destination |= destinations[index] << shift;
                source |= sources[index] << shift;
                expected |= reference_accumulate(
                    destinations[index],
                    sources[index],
                    self.lane_bits,
                    unsigned_destination,
                ) << shift;
            }
            let word = self.accumulate_word(unsigned_destination, 30, 31);
            let mut cpu = Aarch64CpuState {
                pc: 0x400bc8,
                nzcv: Nzcv::from_bits(flags),
                fpcr: 0x3344,
                fpsr: 0x95,
                ..Default::default()
            };
            cpu.set_vector(30, source);
            cpu.set_vector(
                31,
                destination | if self.scalar { u128::MAX << self.lane_bits } else { 0 },
            );
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(31), expected, "{word:#010x}");
            assert_eq!((cpu.pc, cpu.nzcv.bits(), cpu.fpcr), (0x400bcc, flags, 0x3344));
            assert_eq!(cpu.fpsr, 0x0800_0095);
        }

        fn assert_accumulate_encodings(self, unsigned_destination: bool) {
            for source in 0_u32..32 {
                for destination in 0_u32..32 {
                    let word = self.accumulate_word(unsigned_destination, source, destination);
                    assert_eq!(
                        Aarch64Decoder::decode(word).unwrap().instruction,
                        Aarch64Instruction::SimdSaturatingAccumulate {
                            unsigned_destination,
                            lane_bits: self.lane_bits,
                            source: source as u8,
                            destination: destination as u8,
                            lanes: self.lanes
                        }
                    );
                }
            }
        }

        fn assert_accumulate_alias(self, unsigned_destination: bool) {
            let sign = 1_u128 << (self.lane_bits - 1);
            let mut aliases = 0_u128;
            for lane in 0..self.lanes {
                aliases |= sign << (u32::from(lane) * u32::from(self.lane_bits));
            }
            let word = self.accumulate_word(unsigned_destination, 31, 31);
            let mut cpu = Aarch64CpuState {
                pc: 0x800,
                fpsr: 1 << 27,
                ..Default::default()
            };
            cpu.set_vector(31, aliases | if self.scalar { u128::MAX << self.lane_bits } else { 0 });
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(31), 0, "{word:#010x}");
            assert_eq!(cpu.fpsr, 1 << 27);
        }
    }

    fn reference_accumulate(destination: u128, source: u128, bits: u8, unsigned_destination: bool) -> u128 {
        let sign = 1_u128 << (bits - 1);
        let mask = sign * 2 - 1;
        let signed = |value: u128| {
            if value & sign == 0 {
                value as i128
            } else {
                value as i128 - (mask as i128 + 1)
            }
        };
        let exact = if unsigned_destination {
            destination as i128 + signed(source)
        } else {
            signed(destination) + source as i128
        };
        let minimum = if unsigned_destination { 0 } else { -(sign as i128) };
        let maximum = if unsigned_destination {
            mask as i128
        } else {
            sign as i128 - 1
        };
        exact.clamp(minimum, maximum) as u128 & mask
    }

    #[test]
    fn complete_encoding_family() {
        for negate in [false, true] {
            for shape in Shape::ALL {
                shape.assert_encoding(negate);
            }
        }
        for word in [0x0ee0_7800, 0x2ee0_7800] {
            assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
        }
    }

    #[test]
    fn complete_accumulate_family() {
        for unsigned_destination in [false, true] {
            for shape in Shape::ALL {
                shape.assert_accumulate_encodings(unsigned_destination);
            }
        }
        assert_eq!(
            Aarch64Decoder::decode(0x4e60_3bf7).unwrap().instruction,
            Aarch64Instruction::SimdSaturatingAccumulate {
                unsigned_destination: false,
                lane_bits: 16,
                source: 31,
                destination: 23,
                lanes: 8
            }
        );
        for word in [0x0ee0_3800, 0x2ee0_3800] {
            assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
        }
    }

    #[test]
    fn saturation_aliasing_state() {
        let flags = Nzcv::NEGATIVE | Nzcv::CARRY;
        for negate in [false, true] {
            for shape in Shape::ALL {
                shape.assert_execution(negate, flags);
            }
        }
    }

    #[test]
    fn accumulate_aliases_state() {
        let flags = Nzcv::NEGATIVE | Nzcv::CARRY;
        for unsigned_destination in [false, true] {
            for shape in Shape::ALL {
                shape.assert_accumulate(unsigned_destination, flags);
                shape.assert_accumulate_alias(unsigned_destination);
            }
        }
    }

    #[test]
    fn nonsaturating_preserves_qc() {
        for initial_qc in [false, true] {
            let mut cpu = Aarch64CpuState {
                pc: 0x700,
                fpsr: u64::from(initial_qc) << 27,
                ..Default::default()
            };
            cpu.set_vector(1, 0x01ff_7f02_03fe_7e04);
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e20_7820),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), 0x0101_7f02_0302_7e04);
            assert_eq!(cpu.fpsr, u64::from(initial_qc) << 27);
        }
    }

    #[test]
    fn reserved_state_rollback() {
        for word in [0x0ee0_7800, 0x2ee0_7800, 0x0ee0_3800, 0x2ee0_3800] {
            let mut cpu = Aarch64CpuState {
                pc: 0x900,
                nzcv: Nzcv::from_bits(0xf000_0000),
                fpcr: 0x1234,
                fpsr: 0x5678,
                vectors: [u128::MAX; 32],
                ..Default::default()
            };
            let before = cpu.clone();
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::UndefinedInstruction {
                    instruction: 0x900,
                    word
                }
            );
            assert_eq!(cpu, before);
        }
    }
}
