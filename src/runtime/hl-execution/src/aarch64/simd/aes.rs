use crate::{Aarch64CpuState, Aarch64Instruction, AesOperation};

pub(crate) struct Aes;

impl Aes {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let operation = match word & 0xffff_fc00 {
            0x4e28_4800 => AesOperation::Encrypt,
            0x4e28_5800 => AesOperation::Decrypt,
            0x4e28_6800 => AesOperation::MixColumns,
            0x4e28_7800 => AesOperation::InverseMixColumns,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdAes {
            operation,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: AesOperation,
        source: u8,
        destination: u8,
    ) {
        let source = cpu.vector(source).to_le_bytes();
        let mut state = cpu.vector(destination).to_le_bytes();
        match operation {
            AesOperation::Encrypt => {
                for index in 0..16 {
                    state[index] = Self::sub(state[index] ^ source[index]);
                }
                state = Self::shift(state, false);
            }
            AesOperation::Decrypt => {
                for index in 0..16 {
                    state[index] = Self::inverse_sub(state[index] ^ source[index]);
                }
                state = Self::shift(state, true);
            }
            AesOperation::MixColumns => state = Self::mix(source, false),
            AesOperation::InverseMixColumns => state = Self::mix(source, true),
        }
        staged.set_vector(destination, u128::from_le_bytes(state));
    }

    fn sub(value: u8) -> u8 {
        let inverse = Self::inverse(value);
        inverse
            ^ inverse.rotate_left(1)
            ^ inverse.rotate_left(2)
            ^ inverse.rotate_left(3)
            ^ inverse.rotate_left(4)
            ^ 0x63
    }

    fn inverse_sub(value: u8) -> u8 {
        Self::inverse(value.rotate_left(1) ^ value.rotate_left(3) ^ value.rotate_left(6) ^ 0x05)
    }

    fn inverse(value: u8) -> u8 {
        if value == 0 {
            return 0;
        }
        let mut result = 1_u8;
        let mut base = value;
        let mut exponent = 254_u8;
        while exponent != 0 {
            if exponent & 1 != 0 {
                result = Self::product(result, base);
            }
            base = Self::product(base, base);
            exponent >>= 1;
        }
        result
    }

    fn product(mut left: u8, mut right: u8) -> u8 {
        let mut result = 0_u8;
        for _ in 0..8 {
            if right & 1 != 0 {
                result ^= left;
            }
            left = (left << 1) ^ if left & 0x80 != 0 { 0x1b } else { 0 };
            right >>= 1;
        }
        result
    }

    fn shift(state: [u8; 16], inverse: bool) -> [u8; 16] {
        let mut result = [0_u8; 16];
        let rotation = if inverse { [0, 3, 2, 1] } else { [0, 1, 2, 3] };
        for column in 0..4 {
            for row in 0..4 {
                let source = (column + rotation[row]) & 3;
                result[row + 4 * column] = state[row + 4 * source];
            }
        }
        result
    }

    fn mix(state: [u8; 16], inverse: bool) -> [u8; 16] {
        let mut result = [0_u8; 16];
        let coefficients = if inverse { [14, 11, 13, 9] } else { [2, 3, 1, 1] };
        for column in 0..4 {
            for row in 0..4 {
                let offset = 4 * column;
                result[row + offset] = Self::product(state[offset], coefficients[(4 - row) & 3])
                    ^ Self::product(state[offset + 1], coefficients[(5 - row) & 3])
                    ^ Self::product(state[offset + 2], coefficients[(6 - row) & 3])
                    ^ Self::product(state[offset + 3], coefficients[(7 - row) & 3]);
            }
        }
        result
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn apply(cpu: &mut Aarch64CpuState, operation: AesOperation, source: u8, destination: u8) {
        let mut staged = cpu.clone();
        Aes::execute(cpu, &mut staged, operation, source, destination);
        *cpu = staged;
    }

    #[test]
    fn register_encodings() {
        for (base, operation) in [
            (0x4e28_4800, AesOperation::Encrypt),
            (0x4e28_5800, AesOperation::Decrypt),
            (0x4e28_6800, AesOperation::MixColumns),
            (0x4e28_7800, AesOperation::InverseMixColumns),
        ] {
            for index in 0_u32..1024 {
                let source = index / 32;
                let destination = index % 32;
                assert_eq!(
                    Aes::decode(base | source << 5 | destination),
                    Some(Aarch64Instruction::SimdAes {
                        operation,
                        source: source as u8,
                        destination: destination as u8,
                    })
                );
            }
        }
        for word in [0x0e28_4800, 0x4e28_4400, 0x4e29_4800, 0xce28_4800] {
            assert_eq!(Aes::decode(word), None);
        }
    }

    #[test]
    fn round_vector() {
        let input = [
            0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0,
        ];
        let key = [
            0xd6, 0xaa, 0x74, 0xfd, 0xd2, 0xaf, 0x72, 0xfa, 0xda, 0xa6, 0x78, 0xf1, 0xd6, 0xab, 0x76, 0xfe,
        ];
        let expected = [
            0x89, 0xd8, 0x10, 0xe8, 0x85, 0x5a, 0xce, 0x68, 0x2d, 0x18, 0x43, 0xd8, 0xcb, 0x12, 0x8f, 0xe4,
        ];
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(0, u128::from_le_bytes(input));
        cpu.set_vector(1, 0);
        apply(&mut cpu, AesOperation::Encrypt, 1, 0);
        apply(&mut cpu, AesOperation::MixColumns, 0, 0);
        assert_eq!((cpu.vector(0) ^ u128::from_le_bytes(key)).to_le_bytes(), expected);
    }

    #[test]
    fn inverse_operations() {
        for value in [0_u128, 1, u128::MAX, 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff] {
            let mut cpu = Aarch64CpuState::default();
            cpu.set_vector(0, value);
            cpu.set_vector(1, 0);
            apply(&mut cpu, AesOperation::Encrypt, 1, 0);
            apply(&mut cpu, AesOperation::Decrypt, 1, 0);
            assert_eq!(cpu.vector(0), value);
            apply(&mut cpu, AesOperation::MixColumns, 0, 0);
            apply(&mut cpu, AesOperation::InverseMixColumns, 0, 0);
            assert_eq!(cpu.vector(0), value);
        }
    }
}
