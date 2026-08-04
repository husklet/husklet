use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction, SimdUnary};

pub(crate) struct Scalar;

impl Scalar {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if word & 0xffe0_fc00 == 0x5e00_0400 {
            let immediate = (word >> 16 & 31) as u8;
            if immediate == 0 || immediate.trailing_zeros() > 3 {
                return Some(Err(Aarch64DecodeError::Reserved));
            }
            let shift = immediate.trailing_zeros() as u8;
            return Some(Ok(Aarch64Instruction::SimdScalarMove {
                lane_bits: 8 << shift,
                lane: immediate >> (shift + 1),
                source: (word >> 5 & 31) as u8,
                destination: (word & 31) as u8,
            }));
        }
        let opcode = word >> 12 & 31;
        if word & 0xdf3e_0c00 == 0x5e20_0800 && matches!(opcode, 8..=10) {
            let unsigned = word >> 29 & 1 != 0;
            let size = word >> 22 & 3;
            if size != 3 || opcode == 10 && unsigned {
                return Some(Err(Aarch64DecodeError::Reserved));
            }
            let operation = match (opcode, unsigned) {
                (8, false) => SimdUnary::CompareGreaterZero,
                (8, true) => SimdUnary::CompareGreaterEqualZero,
                (9, false) => SimdUnary::CompareEqualZero,
                (9, true) => SimdUnary::CompareLessEqualZero,
                (10, false) => SimdUnary::CompareLessZero,
                _ => unreachable!("reserved scalar compare was rejected"),
            };
            return Some(Ok(Aarch64Instruction::SimdUnary {
                operation,
                lane_bits: 64,
                source: (word >> 5 & 31) as u8,
                destination: (word & 31) as u8,
                wide: false,
            }));
        }
        if matches!(word & 0xffe0_fc00, 0x5ee0_8400 | 0x7ee0_8400) {
            return Some(Ok(Aarch64Instruction::SimdAddSubtract {
                subtract: word >> 29 & 1 != 0,
                saturating: false,
                unsigned: false,
                lane_bits: 64,
                left: (word >> 5 & 31) as u8,
                right: (word >> 16 & 31) as u8,
                destination: (word & 31) as u8,
                wide: false,
            }));
        }
        let left = match word & 0xff80_fc00 {
            0x5f00_5400 => true,
            0x7f00_0400 => false,
            _ => return None,
        };
        let immediate = (word >> 16 & 0x7f) as u8;
        if immediate < 64 {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let amount = if left { immediate - 64 } else { 128 - immediate };
        if !left && amount == 0 {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        Some(Ok(Aarch64Instruction::SimdScalarShift {
            amount,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            left,
        }))
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        amount: u8,
        source: u8,
        destination: u8,
        left: bool,
    ) {
        let value = cpu.vector_lane(source, 64, 0);
        let shifted = if left {
            value << amount
        } else if amount == 64 {
            0
        } else {
            value >> amount
        };
        staged.set_vector(destination, u128::from(shifted));
    }

    pub(crate) fn move_lane(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        lane_bits: u8,
        lane: u8,
        source: u8,
        destination: u8,
    ) {
        staged.set_vector(destination, u128::from(cpu.vector_lane(source, lane_bits, lane)));
    }
}

#[cfg(test)]
mod test {
    use crate::{
        Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction,
        Aarch64Interpreter, Nzcv, PcCoordinatePort, SimdUnary,
    };

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    #[test]
    fn compare_zero_family() {
        let operations = [
            (0x5ee0_8800, SimdUnary::CompareGreaterZero),
            (0x7ee0_8800, SimdUnary::CompareGreaterEqualZero),
            (0x5ee0_9800, SimdUnary::CompareEqualZero),
            (0x7ee0_9800, SimdUnary::CompareLessEqualZero),
            (0x5ee0_a800, SimdUnary::CompareLessZero),
        ];
        for (base, operation) in operations {
            for source in 0_u32..32 {
                for destination in 0_u32..32 {
                    let word = base | source << 5 | destination;
                    assert_eq!(
                        Aarch64Decoder::decode(word).unwrap().instruction,
                        Aarch64Instruction::SimdUnary {
                            operation,
                            lane_bits: 64,
                            source: source as u8,
                            destination: destination as u8,
                            wide: false,
                        }
                    );
                }
            }
        }
        for size in 0_u32..3 {
            for base in [0x5e20_8800, 0x7e20_8800, 0x5e20_9800, 0x7e20_9800, 0x5e20_a800] {
                assert_eq!(
                    Aarch64Decoder::decode(base | size << 22),
                    Err(Aarch64DecodeError::Reserved)
                );
            }
        }
        assert_eq!(Aarch64Decoder::decode(0x7ee0_a800), Err(Aarch64DecodeError::Reserved));
    }

    #[test]
    fn compare_zero_state() {
        let cases = [
            (0x5ee0_8820, 1_u64, u64::MAX),
            (0x7ee0_8820, 0, u64::MAX),
            (0x5ee0_9820, 0, u64::MAX),
            (0x7ee0_9820, u64::MAX, u64::MAX),
            (0x5ee0_a820, u64::MAX, u64::MAX),
            (0x7ee0_8820, u64::MAX, 0),
        ];
        for (word, source, expected) in cases {
            let mut cpu = Aarch64CpuState {
                pc: 0x400,
                nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(1, u128::MAX << 64 | u128::from(source));
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), u128::from(expected));
            assert_eq!(cpu.pc, 0x404);
            assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }

    #[test]
    fn immediate_family() {
        for index in 0_u32..64 * 32 * 32 {
            let amount = index / 1024 + 1;
            let source = index / 32 % 32;
            let destination = index % 32;
            let word = 0x7f00_0400 | (128 - amount) << 16 | source << 5 | destination;
            assert_eq!(
                Aarch64Decoder::decode(word).unwrap().instruction,
                Aarch64Instruction::SimdScalarShift {
                    amount: amount as u8,
                    source: source as u8,
                    destination: destination as u8,
                    left: false,
                }
            );
        }
        for index in 0_u32..64 * 32 * 32 {
            let amount = index / 1024;
            let source = index / 32 % 32;
            let destination = index % 32;
            let word = 0x5f00_5400 | (64 + amount) << 16 | source << 5 | destination;
            assert_eq!(
                Aarch64Decoder::decode(word).unwrap().instruction,
                Aarch64Instruction::SimdScalarShift {
                    amount: amount as u8,
                    source: source as u8,
                    destination: destination as u8,
                    left: true,
                }
            );
        }
        for immediate in 0_u32..64 {
            let word = 0x7f00_0400 | immediate << 16;
            assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
            let word = 0x5f00_5400 | immediate << 16;
            assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
        }
    }

    #[test]
    fn shift_state() {
        let aliases = [(0_u32, 1_u32), (31, 31), (5, 5)];
        for index in 0_u32..64 * aliases.len() as u32 {
            let amount = index / aliases.len() as u32 + 1;
            let (source, destination) = aliases[index as usize % aliases.len()];
            let word = 0x7f00_0400 | (128 - amount) << 16 | source << 5 | destination;
            let value = 0xfedc_ba98_7654_3210_u64;
            let mut cpu = Aarch64CpuState {
                pc: 0x200,
                nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(source as u8, u128::MAX << 64 | u128::from(value));
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            let expected = value.checked_shr(amount).unwrap_or(0);
            assert_eq!(cpu.vector(destination as u8), u128::from(expected));
            assert_eq!(cpu.pc, 0x204);
            assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
        let mut cpu = Aarch64CpuState {
            pc: 0x1007_77c,
            ..Default::default()
        };
        cpu.set_vector(31, u128::MAX << 64 | 0x1234_5678_9abc_def0);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x5f43_57ff),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), 0x91a2_b3c4_d5e6_f780);
        assert_eq!(cpu.pc, 0x1007_780);
    }

    #[test]
    fn add_subtract_family() {
        for subtract in [false, true] {
            let base = if subtract { 0x7ee0_8400 } else { 0x5ee0_8400 };
            for index in 0_u32..32 * 32 * 32 {
                let left = index / 1024;
                let right = index / 32 % 32;
                let destination = index % 32;
                let word = base | right << 16 | left << 5 | destination;
                assert_eq!(
                    Aarch64Decoder::decode(word).unwrap().instruction,
                    Aarch64Instruction::SimdAddSubtract {
                        subtract,
                        saturating: false,
                        unsigned: false,
                        lane_bits: 64,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8,
                        wide: false,
                    }
                );
            }
        }
        for word in [0x1ee0_8400, 0x4ee0_8400, 0x5ee0_8000, 0x5e60_8400] {
            assert!(!matches!(
                Aarch64Decoder::decode(word).map(|ir| ir.instruction),
                Ok(Aarch64Instruction::SimdAddSubtract {
                    lane_bits: 64,
                    wide: false,
                    ..
                })
            ));
        }
    }

    #[test]
    fn add_subtract_state() {
        for (word, left, right, expected) in [(0x5ee2_8420, u64::MAX, 1_u64, 0_u64), (0x7ee2_8420, 0, 1, u64::MAX)] {
            let mut cpu = Aarch64CpuState {
                pc: 0x300,
                nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(1, u128::MAX << 64 | u128::from(left));
            cpu.set_vector(2, u128::from(right));
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), u128::from(expected));
            assert_eq!(cpu.pc, 0x304);
            assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }

    #[test]
    fn move_lane_family() {
        for (lane_bits, lanes, shift) in [(8_u8, 16_u32, 0_u32), (16, 8, 1), (32, 4, 2), (64, 2, 3)] {
            for index in 0_u32..lanes * 32 * 32 {
                let lane = index / 1024;
                let source = index / 32 % 32;
                let destination = index % 32;
                let immediate = (lane * 2 + 1) << shift;
                let word = 0x5e00_0400 | immediate << 16 | source << 5 | destination;
                assert_eq!(
                    Aarch64Decoder::decode(word).unwrap().instruction,
                    Aarch64Instruction::SimdScalarMove {
                        lane_bits,
                        lane: lane as u8,
                        source: source as u8,
                        destination: destination as u8
                    }
                );
            }
        }
        for immediate in [0_u32, 16] {
            assert_eq!(
                Aarch64Decoder::decode(0x5e00_0400 | immediate << 16),
                Err(Aarch64DecodeError::Reserved)
            );
        }
    }

    #[test]
    fn move_lane_state() {
        let value = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128;
        for (word, expected) in [
            (0x5e1f_0420, 0_u64),
            (0x5e1e_0420, 0x0011),
            (0x5e1c_0420, 0x0011_2233),
            (0x5e18_0420, 0x0011_2233_4455_6677),
        ] {
            let mut cpu = Aarch64CpuState {
                pc: 0x400,
                nzcv: Nzcv::from_bits(Nzcv::CARRY),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(1, value);
            cpu.set_vector(0, u128::MAX);
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), u128::from(expected));
            assert_eq!(cpu.pc, 0x404);
            assert_eq!(cpu.nzcv.bits(), Nzcv::CARRY);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }
}
