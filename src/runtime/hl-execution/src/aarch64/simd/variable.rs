use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction};

pub(crate) struct VariableShift;

impl VariableShift {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if word & 0x9f20_0400 != 0x0e20_0400 {
            return None;
        }
        let opcode = word >> 11 & 31;
        if !(8..=11).contains(&opcode) {
            return None;
        }
        let wide = word >> 30 & 1 != 0;
        let size = (word >> 22 & 3) as u8;
        if size == 3 && !wide {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        Some(Ok(Aarch64Instruction::SimdVariable {
            signed: word >> 29 & 1 == 0,
            saturating: opcode & 1 != 0,
            rounding: opcode & 2 != 0,
            lane_bits: 8 << size,
            source: (word >> 5 & 31) as u8,
            counts: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            wide,
        }))
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        signed: bool,
        saturating: bool,
        rounding: bool,
        source: u8,
        counts: u8,
        lane_bits: u8,
        wide: bool,
    ) -> (u128, bool) {
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let mask = Self::mask(lane_bits);
        let mut result = 0;
        let mut saturated = false;
        for lane in 0..lanes {
            let value = u128::from(cpu.vector_lane(source, lane_bits, lane));
            let count = cpu.vector_lane(counts, lane_bits, lane) as u8 as i8;
            let (shifted, clamped) = Self::lane(value, count, lane_bits, mask, signed, saturating, rounding);
            result |= shifted << (u32::from(lane) * u32::from(lane_bits));
            saturated |= clamped;
        }
        (result, saturated)
    }

    fn lane(
        value: u128,
        count: i8,
        bits: u8,
        mask: u128,
        signed: bool,
        saturating: bool,
        rounding: bool,
    ) -> (u128, bool) {
        if count < 0 {
            return (
                Self::right(value, count.unsigned_abs(), bits, mask, signed, rounding),
                false,
            );
        }

        let amount = count as u32;
        if !saturating {
            return (
                if amount >= u32::from(bits) {
                    0
                } else {
                    value << amount & mask
                },
                false,
            );
        }
        if signed {
            return Self::signed_left(value, amount, bits, mask);
        }
        let maximum = mask;
        let exact = if amount >= u32::from(bits) {
            if value == 0 { 0 } else { maximum + 1 }
        } else {
            value << amount
        };
        (exact.min(maximum), exact > maximum)
    }

    fn signed_left(value: u128, amount: u32, bits: u8, mask: u128) -> (u128, bool) {
        let shift = 128 - u32::from(bits);
        let extended = (value << shift) as i128 >> shift;
        let minimum = -(1_i128 << (bits - 1));
        let maximum = (1_i128 << (bits - 1)) - 1;
        let exact = if amount < u32::from(bits) {
            extended << amount
        } else if extended == 0 {
            0
        } else if extended < 0 {
            minimum - 1
        } else {
            maximum + 1
        };
        let clamped = exact.clamp(minimum, maximum);
        (clamped as u128 & mask, clamped != exact)
    }

    fn right(value: u128, amount: u8, bits: u8, mask: u128, signed: bool, rounding: bool) -> u128 {
        let amount = u32::from(amount);
        if rounding {
            return Self::rounded_right(value, amount, bits, mask, signed);
        }
        if !signed {
            return if amount >= u32::from(bits) { 0 } else { value >> amount };
        }
        let shift = 128 - u32::from(bits);
        let extended = (value << shift) as i128 >> shift;
        if amount >= u32::from(bits) {
            return if extended < 0 { mask } else { 0 };
        }
        (extended >> amount) as u128 & mask
    }

    fn rounded_right(value: u128, amount: u32, bits: u8, mask: u128, signed: bool) -> u128 {
        if amount == 0 {
            return value;
        }
        if amount > u32::from(bits) {
            return 0;
        }
        let bias = 1_i128 << (amount - 1);
        if signed {
            let shift = 128 - u32::from(bits);
            let extended = (value << shift) as i128 >> shift;
            return ((extended + bias) >> amount) as u128 & mask;
        }
        ((value + bias as u128) >> amount) & mask
    }

    fn mask(bits: u8) -> u128 {
        if bits == 64 {
            u128::from(u64::MAX)
        } else {
            (1_u128 << bits) - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VariableShift;
    use crate::{
        Aarch64CpuState, Aarch64DecodeError, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Interpreter, Nzcv,
        PcCoordinatePort,
    };

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    #[test]
    fn decodes_family() {
        for shape in 0_u32..64 {
            let opcode = 8 + (shape & 3);
            let unsigned = shape >> 2 & 1 != 0;
            let size = shape >> 3 & 3;
            let wide = shape >> 5 != 0;
            let base = 0x0e20_0400 | opcode << 11 | u32::from(unsigned) << 29 | size << 22 | u32::from(wide) << 30;
            assert_shape(base, opcode, unsigned, size, wide);
        }
    }

    #[test]
    fn rejects_adjacent_encodings() {
        for word in [0x4e20_3c00, 0x4e20_6400] {
            assert_eq!(VariableShift::decode(word), None);
        }
        assert_eq!(
            VariableShift::decode(0x0ee0_4c00),
            Some(Err(Aarch64DecodeError::Reserved))
        );
    }

    #[test]
    fn boundaries_and_aliases() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(30, 2 | (u128::from(u64::MAX) << 64));
        cpu.set_vector(31, 63 | (u128::from((-64_i8) as u8) << 64));
        let (value, saturated) = VariableShift::execute(&cpu, false, true, false, 30, 31, 64, true);
        assert_eq!(value, u128::from(u64::MAX));
        assert!(saturated);

        cpu.set_vector(1, 0x40ff_8001_7fff_0001);
        cpu.set_vector(2, 0x01ff_0101_01ff_0807);
        let source = cpu.vector(1);
        let counts = cpu.vector(2);
        let unsigned = VariableShift::execute(&cpu, false, true, false, 1, 2, 8, false);
        let signed = VariableShift::execute(&cpu, true, true, false, 1, 2, 8, false);
        assert_eq!((cpu.vector(1), cpu.vector(2)), (source, counts));
        assert!(unsigned.1 && signed.1);
        assert_ne!(unsigned.0, signed.0);
    }

    #[test]
    fn right_qc() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(1, 0x8000_0000_0000_0000);
        cpu.set_vector(2, u128::from((-127_i8) as u8));
        assert_eq!(
            VariableShift::execute(&cpu, true, true, false, 1, 2, 64, true),
            (u128::from(u64::MAX), false)
        );
        cpu.set_vector(1, 0);
        cpu.set_vector(2, 127);
        assert_eq!(
            VariableShift::execute(&cpu, false, true, false, 1, 2, 64, true),
            (0, false)
        );
    }

    #[test]
    fn rounding_edges() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(1, 0x80_7f_fd_ff);
        cpu.set_vector(2, 0xf7_f8_ff_ff);
        let signed = VariableShift::execute(&cpu, true, true, true, 1, 2, 8, false);
        assert_eq!(signed, (0x00_00_ff_00, false));
        cpu.set_vector(1, 0xff_ff);
        cpu.set_vector(2, 0xf7_f8);
        let unsigned = VariableShift::execute(&cpu, false, true, true, 1, 2, 8, false);
        assert_eq!(unsigned, (0x00_01, false));
    }

    fn assert_shape(base: u32, opcode: u32, unsigned: bool, size: u32, wide: bool) {
        for encoded in [0, 1, 0x421, 0x7fff] {
            let word = base | (encoded & 31) | (encoded >> 5 & 31) << 5 | (encoded >> 10 & 31) << 16;
            let decoded = VariableShift::decode(word).unwrap();
            if size == 3 && !wide {
                assert_eq!(decoded, Err(Aarch64DecodeError::Reserved));
            } else {
                assert!(matches!(decoded, Ok(Aarch64Instruction::SimdVariable {
                    signed, saturating, rounding, lane_bits, wide: decoded_wide, ..
                }) if signed != unsigned && saturating == (opcode & 1 != 0)
                    && rounding == (opcode & 2 != 0)
                    && lane_bits == 8 << size && decoded_wide == wide));
            }
        }
    }

    #[test]
    fn frontier_commits_once() {
        let flags = Nzcv::NEGATIVE | Nzcv::CARRY;
        let mut cpu = Aarch64CpuState {
            registers: [0x55aa; 31],
            sp: 0x8000,
            pc: 0x4020,
            tls: 0x1234,
            nzcv: Nzcv::from_bits(flags),
            fpcr: 0x1122,
            fpsr: 0x41,
            ..Default::default()
        };
        cpu.set_vector(30, 2 | (u128::from(u64::MAX) << 64));
        cpu.set_vector(31, 63 | (u128::from((-64_i8) as u8) << 64));
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6eff_4fdf),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), u128::from(u64::MAX));
        assert_eq!((cpu.pc, cpu.nzcv.bits(), cpu.fpsr), (0x4024, flags, 0x0800_0041));
        assert_eq!(
            (cpu.registers, cpu.sp, cpu.tls, cpu.fpcr),
            ([0x55aa; 31], 0x8000, 0x1234, 0x1122)
        );
    }

    #[test]
    fn reserved_rolls_back() {
        let mut cpu = Aarch64CpuState {
            pc: 0x9000,
            fpsr: 0x8877,
            ..Default::default()
        };
        cpu.set_vector(0, u128::MAX);
        let before = cpu.clone();
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0ee0_4c00),
            Aarch64ExecutionExit::UndefinedInstruction {
                instruction: 0x9000,
                word: 0x0ee0_4c00
            }
        );
        assert_eq!(cpu, before);
    }
}
