// The SHA specifications name these working variables a..h and w; keep them.
#![allow(clippy::many_single_char_names, clippy::format_collect)]

use crate::{Aarch64CpuState, Aarch64Instruction, Sha256Operation};

pub(crate) struct Sha256Unit;

impl Sha256Unit {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let operation = match word & 0xffe0_fc00 {
            0x5e00_4000 => Sha256Operation::Hash,
            0x5e00_5000 => Sha256Operation::HashSecond,
            0x5e00_6000 => Sha256Operation::ScheduleOne,
            _ if word & 0xffff_fc00 == 0x5e28_2800 => Sha256Operation::ScheduleZero,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdSha256 {
            operation,
            first: (word >> 5 & 31) as u8,
            second: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: Sha256Operation,
        first: u8,
        second: u8,
        destination: u8,
    ) {
        let destination_value = Self::lanes(cpu.vector(destination));
        let first_value = Self::lanes(cpu.vector(first));
        let second_value = Self::lanes(cpu.vector(second));
        let result = match operation {
            Sha256Operation::Hash => Self::rounds(destination_value, first_value, second_value).0,
            Sha256Operation::HashSecond => Self::rounds(first_value, destination_value, second_value).1,
            Sha256Operation::ScheduleZero => Self::schedule_zero(destination_value, first_value),
            Sha256Operation::ScheduleOne => Self::schedule_one(destination_value, first_value, second_value),
        };
        staged.set_vector(destination, Self::vector(result));
    }

    fn rounds(mut abcd: [u32; 4], mut efgh: [u32; 4], words: [u32; 4]) -> ([u32; 4], [u32; 4]) {
        let [mut a, mut b, mut c, mut d] = abcd;
        let [mut e, mut f, mut g, mut h] = efgh;
        for word in words {
            let choose = (e & f) ^ (!e & g);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let sigma_one = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let sigma_zero = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let first = h.wrapping_add(sigma_one).wrapping_add(choose).wrapping_add(word);
            let second = sigma_zero.wrapping_add(majority);
            [h, g, f, e] = [g, f, e, d.wrapping_add(first)];
            [d, c, b, a] = [c, b, a, first.wrapping_add(second)];
        }
        abcd = [a, b, c, d];
        efgh = [e, f, g, h];
        (abcd, efgh)
    }

    fn schedule_zero(destination: [u32; 4], source: [u32; 4]) -> [u32; 4] {
        let sigma = |value: u32| value.rotate_right(7) ^ value.rotate_right(18) ^ value >> 3;
        [
            destination[0].wrapping_add(sigma(destination[1])),
            destination[1].wrapping_add(sigma(destination[2])),
            destination[2].wrapping_add(sigma(destination[3])),
            destination[3].wrapping_add(sigma(source[0])),
        ]
    }

    fn schedule_one(destination: [u32; 4], first: [u32; 4], second: [u32; 4]) -> [u32; 4] {
        let sigma = |value: u32| value.rotate_right(17) ^ value.rotate_right(19) ^ value >> 10;
        let mut result = [0_u32; 4];
        result[0] = destination[0].wrapping_add(first[1]).wrapping_add(sigma(second[2]));
        result[1] = destination[1].wrapping_add(first[2]).wrapping_add(sigma(second[3]));
        result[2] = destination[2].wrapping_add(first[3]).wrapping_add(sigma(result[0]));
        result[3] = destination[3].wrapping_add(second[0]).wrapping_add(sigma(result[1]));
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

    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    #[test]
    fn register_encodings() {
        for (base, operation) in [
            (0x5e00_4000, Sha256Operation::Hash),
            (0x5e00_5000, Sha256Operation::HashSecond),
            (0x5e00_6000, Sha256Operation::ScheduleOne),
        ] {
            for index in 0_u32..32 * 32 * 32 {
                let first = index / 1024;
                let second = index / 32 % 32;
                let destination = index % 32;
                assert_eq!(
                    Sha256Unit::decode(base | second << 16 | first << 5 | destination),
                    Some(Aarch64Instruction::SimdSha256 {
                        operation,
                        first: first as u8,
                        second: second as u8,
                        destination: destination as u8
                    })
                );
            }
        }
        for index in 0_u32..1024 {
            let first = index / 32;
            let destination = index % 32;
            assert_eq!(
                Sha256Unit::decode(0x5e28_2800 | first << 5 | destination),
                Some(Aarch64Instruction::SimdSha256 {
                    operation: Sha256Operation::ScheduleZero,
                    first: first as u8,
                    second: 8,
                    destination: destination as u8
                })
            );
        }
        for word in [0x1e00_4000, 0x5e00_4400, 0x5e20_4000, 0x5e28_2c00] {
            assert_eq!(Sha256Unit::decode(word), None);
        }
    }

    #[test]
    fn state_invariants() {
        for word in [
            0x5e02_4020,
            0x5e02_5020,
            0x5e02_6020,
            0x5e28_2820,
            0x5e00_4000,
            0x5e00_5000,
            0x5e00_6000,
            0x5e28_2800,
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
            (
                &b""[..],
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                &b"abc"[..],
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                &b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..],
                "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
            ),
        ] {
            assert_eq!(hex(&digest(message)), expected);
        }
    }

    fn digest(message: &[u8]) -> [u8; 32] {
        let mut padded = message.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&(message.len() as u64 * 8).to_be_bytes());
        let mut state = [
            0x6a09e667_u32,
            0xbb67ae85,
            0x3c6ef372,
            0xa54ff53a,
            0x510e527f,
            0x9b05688c,
            0x1f83d9ab,
            0x5be0cd19,
        ];
        for block in padded.chunks_exact(64) {
            compress(&mut state, block);
        }
        let mut result = [0_u8; 32];
        for (index, value) in state.into_iter().enumerate() {
            result[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        result
    }

    fn compress(state: &mut [u32; 8], block: &[u8]) {
        let mut words = [0_u32; 64];
        for index in 0..16 {
            words[index] = u32::from_be_bytes(block[index * 4..index * 4 + 4].try_into().unwrap());
        }
        for base in (16..64).step_by(4) {
            let prior = words[base - 16..base - 12].try_into().unwrap();
            let first = words[base - 12..base - 8].try_into().unwrap();
            let second = words[base - 8..base - 4].try_into().unwrap();
            let third = words[base - 4..base].try_into().unwrap();
            let scheduled = Sha256Unit::schedule_one(Sha256Unit::schedule_zero(prior, first), second, third);
            words[base..base + 4].copy_from_slice(&scheduled);
        }
        let mut abcd = state[..4].try_into().unwrap();
        let mut efgh = state[4..].try_into().unwrap();
        for base in (0..64).step_by(4) {
            let scheduled = std::array::from_fn(|lane| words[base + lane].wrapping_add(ROUND[base + lane]));
            (abcd, efgh) = Sha256Unit::rounds(abcd, efgh, scheduled);
        }
        for index in 0..4 {
            state[index] = state[index].wrapping_add(abcd[index]);
            state[index + 4] = state[index + 4].wrapping_add(efgh[index]);
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
