use crate::{Aarch64CpuState, Aarch64Instruction, Sha1Operation};

pub(crate) struct Sha1Unit;

impl Sha1Unit {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let operation = match word & 0xffe0_fc00 {
            0x5e00_0000 => Sha1Operation::Choose,
            0x5e00_1000 => Sha1Operation::Parity,
            0x5e00_2000 => Sha1Operation::Majority,
            0x5e00_3000 => Sha1Operation::ScheduleZero,
            _ if word & 0xffff_fc00 == 0x5e28_0800 => Sha1Operation::Hash,
            _ if word & 0xffff_fc00 == 0x5e28_1800 => Sha1Operation::ScheduleOne,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdSha1 {
            operation,
            first: (word >> 5 & 31) as u8,
            second: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: Sha1Operation,
        first: u8,
        second: u8,
        destination: u8,
    ) {
        let destination_value = Self::lanes(cpu.vector(destination));
        let first_value = Self::lanes(cpu.vector(first));
        let second_value = Self::lanes(cpu.vector(second));
        let result = match operation {
            Sha1Operation::Choose => Self::rounds(destination_value, first_value[0], second_value, |b, c, d| {
                (b & c) ^ (!b & d)
            }),
            Sha1Operation::Parity => Self::rounds(destination_value, first_value[0], second_value, |b, c, d| b ^ c ^ d),
            Sha1Operation::Majority => Self::rounds(destination_value, first_value[0], second_value, |b, c, d| {
                (b & c) ^ (b & d) ^ (c & d)
            }),
            Sha1Operation::Hash => [first_value[0].rotate_left(30), 0, 0, 0],
            Sha1Operation::ScheduleZero => Self::schedule_zero(destination_value, first_value, second_value),
            Sha1Operation::ScheduleOne => Self::schedule_one(destination_value, first_value),
        };
        staged.set_vector(destination, Self::vector(result));
    }

    fn rounds(mut abcd: [u32; 4], mut e: u32, words: [u32; 4], choose: impl Fn(u32, u32, u32) -> u32) -> [u32; 4] {
        let [mut a, mut b, mut c, mut d] = abcd;
        for word in words {
            let next = a
                .rotate_left(5)
                .wrapping_add(choose(b, c, d))
                .wrapping_add(e)
                .wrapping_add(word);
            [e, d, c, b, a] = [d, c, b.rotate_left(30), a, next];
        }
        abcd = [a, b, c, d];
        abcd
    }

    fn schedule_zero(destination: [u32; 4], first: [u32; 4], second: [u32; 4]) -> [u32; 4] {
        [
            destination[0] ^ destination[2] ^ second[0],
            destination[1] ^ destination[3] ^ second[1],
            destination[2] ^ first[0] ^ second[2],
            destination[3] ^ first[1] ^ second[3],
        ]
    }

    fn schedule_one(destination: [u32; 4], source: [u32; 4]) -> [u32; 4] {
        let mut result = [0_u32; 4];
        result[0] = (destination[0] ^ source[1]).rotate_left(1);
        result[1] = (destination[1] ^ source[2]).rotate_left(1);
        result[2] = (destination[2] ^ source[3]).rotate_left(1);
        result[3] = (destination[3] ^ result[0]).rotate_left(1);
        result
    }

    fn lanes(value: u128) -> [u32; 4] {
        let bytes = value.to_le_bytes();
        std::array::from_fn(|index| u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap()))
    }

    fn vector(lanes: [u32; 4]) -> u128 {
        let mut bytes = [0_u8; 16];
        for (index, lane) in lanes.into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&lane.to_le_bytes());
        }
        u128::from_le_bytes(bytes)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64ExecutionExit, Aarch64Interpreter, Nzcv, PcCoordinatePort};

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    #[test]
    fn register_encodings() {
        for (base, operation) in [
            (0x5e00_0000, Sha1Operation::Choose),
            (0x5e00_1000, Sha1Operation::Parity),
            (0x5e00_2000, Sha1Operation::Majority),
            (0x5e00_3000, Sha1Operation::ScheduleZero),
        ] {
            for index in 0_u32..32 * 32 * 32 {
                let first = index / 1024;
                let second = index / 32 % 32;
                let destination = index % 32;
                assert_eq!(
                    Sha1Unit::decode(base | second << 16 | first << 5 | destination),
                    Some(Aarch64Instruction::SimdSha1 {
                        operation,
                        first: first as u8,
                        second: second as u8,
                        destination: destination as u8
                    })
                );
            }
        }
        for (base, operation) in [
            (0x5e28_0800, Sha1Operation::Hash),
            (0x5e28_1800, Sha1Operation::ScheduleOne),
        ] {
            for index in 0_u32..1024 {
                let first = index / 32;
                let destination = index % 32;
                assert_eq!(
                    Sha1Unit::decode(base | first << 5 | destination),
                    Some(Aarch64Instruction::SimdSha1 {
                        operation,
                        first: first as u8,
                        second: 8,
                        destination: destination as u8
                    })
                );
            }
        }
        for word in [0x1e00_0000, 0x5e00_0400, 0x5e20_0000, 0x5e28_0c00, 0x5e28_1c00] {
            assert_eq!(Sha1Unit::decode(word), None);
        }
    }

    #[test]
    fn state_invariants() {
        for word in [
            0x5e02_0020,
            0x5e02_1020,
            0x5e02_2020,
            0x5e02_3020,
            0x5e28_0820,
            0x5e28_1820,
            0x5e00_0000,
            0x5e00_1000,
            0x5e00_2000,
            0x5e00_3000,
            0x5e28_0800,
            0x5e28_1800,
        ] {
            let mut cpu = Aarch64CpuState {
                pc: 0x700,
                nzcv: Nzcv::from_bits(0xb000_0000),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(0, 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
            cpu.set_vector(1, 0x1021_3243_5465_7687_98a9_bacb_dced_fe0f);
            cpu.set_vector(2, 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef);
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.pc, 0x704);
            assert_eq!(cpu.nzcv.bits(), 0xb000_0000);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }

    #[test]
    fn compression_vectors() {
        for (message, expected) in [
            (&b""[..], "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (&b"abc"[..], "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                &b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..],
                "0098ba824b5c16427bd7a1122a5a442a25ec644d",
            ),
        ] {
            assert_eq!(hex(&digest(message)), expected);
        }
    }

    fn digest(message: &[u8]) -> [u8; 20] {
        let mut padded = message.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&(message.len() as u64 * 8).to_be_bytes());
        let mut state = [0x67452301_u32, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
        for block in padded.chunks_exact(64) {
            compress(&mut state, block);
        }
        let mut result = [0_u8; 20];
        for (index, value) in state.into_iter().enumerate() {
            result[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        result
    }

    fn compress(state: &mut [u32; 5], block: &[u8]) {
        let choose = |b: u32, c: u32, d: u32| (b & c) ^ (!b & d);
        let parity = |b: u32, c: u32, d: u32| b ^ c ^ d;
        let majority = |b: u32, c: u32, d: u32| (b & c) ^ (b & d) ^ (c & d);
        let mut words = [0_u32; 80];
        for index in 0..16 {
            words[index] = u32::from_be_bytes(block[index * 4..index * 4 + 4].try_into().unwrap());
        }
        for base in (16..80).step_by(4) {
            let destination = words[base - 16..base - 12].try_into().unwrap();
            let first = words[base - 12..base - 8].try_into().unwrap();
            let second = words[base - 8..base - 4].try_into().unwrap();
            let source = words[base - 4..base].try_into().unwrap();
            let scheduled = Sha1Unit::schedule_one(Sha1Unit::schedule_zero(destination, first, second), source);
            words[base..base + 4].copy_from_slice(&scheduled);
        }
        let mut abcd: [u32; 4] = state[..4].try_into().unwrap();
        let mut e = state[4];
        for base in (0..80).step_by(4) {
            let (constant, operation) = match base {
                0..=16 => (0x5a827999, Sha1Operation::Choose),
                20..=36 => (0x6ed9eba1, Sha1Operation::Parity),
                40..=56 => (0x8f1bbcdc, Sha1Operation::Majority),
                _ => (0xca62c1d6, Sha1Operation::Parity),
            };
            let scheduled = [
                words[base].wrapping_add(constant),
                words[base + 1].wrapping_add(constant),
                words[base + 2].wrapping_add(constant),
                words[base + 3].wrapping_add(constant),
            ];
            let old_a = abcd[0];
            abcd = match operation {
                Sha1Operation::Choose => Sha1Unit::rounds(abcd, e, scheduled, choose),
                Sha1Operation::Parity => Sha1Unit::rounds(abcd, e, scheduled, parity),
                Sha1Operation::Majority => Sha1Unit::rounds(abcd, e, scheduled, majority),
                _ => unreachable!(),
            };
            e = old_a.rotate_left(30);
        }
        for index in 0..4 {
            state[index] = state[index].wrapping_add(abcd[index]);
        }
        state[4] = state[4].wrapping_add(e);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
