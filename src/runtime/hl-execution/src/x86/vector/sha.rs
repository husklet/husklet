use crate::x86::X86ShaOperation;
use crate::{DecodedInstruction, ScalarInstruction, ScalarIrError, VectorSource};

pub(crate) struct Sha;

impl Sha {
    pub(crate) fn decode(decoded: &DecodedInstruction, map: u8) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let operation = match (map, decoded.opcode) {
            (2, 0xc8) => X86ShaOperation::Sha1Next,
            (2, 0xc9) => X86ShaOperation::Sha1Message1,
            (2, 0xca) => X86ShaOperation::Sha1Message2,
            (2, 0xcb) => X86ShaOperation::Sha256Rounds2,
            (2, 0xcc) => X86ShaOperation::Sha256Message1,
            (2, 0xcd) => X86ShaOperation::Sha256Message2,
            (3, 0xcc) => X86ShaOperation::Sha1Rounds4(decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8 & 3),
            _ => return Err(ScalarIrError::Unsupported),
        };
        let source = if decoded.raw_mod == Some(3) {
            VectorSource::Register(decoded.register_operand.ok_or(ScalarIrError::Invalid)?)
        } else {
            VectorSource::Memory(decoded.address.ok_or(ScalarIrError::Invalid)?)
        };
        Ok(ScalarInstruction::Sha {
            operation,
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            source,
        })
    }

    pub(crate) fn execute(state: u128, source: u128, implicit: u128, operation: X86ShaOperation) -> u128 {
        let d = Self::words(state);
        let s = Self::words(source);
        let output = match operation {
            X86ShaOperation::Sha1Next => [s[0], s[1], s[2], s[3].wrapping_add(d[3].rotate_left(30))],
            X86ShaOperation::Sha1Message1 => [s[2] ^ d[0], s[3] ^ d[1], d[0] ^ d[2], d[1] ^ d[3]],
            X86ShaOperation::Sha1Message2 => {
                let high = (d[3] ^ s[2]).rotate_left(1);
                [
                    (d[0] ^ high).rotate_left(1),
                    (d[1] ^ s[0]).rotate_left(1),
                    (d[2] ^ s[1]).rotate_left(1),
                    high,
                ]
            }
            X86ShaOperation::Sha256Message1 => {
                let input = [d[0], d[1], d[2], d[3], s[0]];
                std::array::from_fn(|i| {
                    let x = input[i + 1];
                    d[i].wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                })
            }
            X86ShaOperation::Sha256Message2 => {
                let sigma = |x: u32| x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10);
                let w16 = d[0].wrapping_add(sigma(s[2]));
                let w17 = d[1].wrapping_add(sigma(s[3]));
                [w16, w17, d[2].wrapping_add(sigma(w16)), d[3].wrapping_add(sigma(w17))]
            }
            X86ShaOperation::Sha256Rounds2 => Self::sha256_rounds(d, s, Self::words(implicit)),
            X86ShaOperation::Sha1Rounds4(select) => Self::sha1_rounds(d, s, select),
        };
        u128::from_le_bytes(output.map(u32::to_le_bytes).concat().try_into().unwrap())
    }

    fn words(value: u128) -> [u32; 4] {
        let bytes = value.to_le_bytes();
        std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
    }

    fn sha1_rounds(d: [u32; 4], s: [u32; 4], select: u8) -> [u32; 4] {
        let k = [0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6][usize::from(select)];
        let (mut a, mut b, mut c, mut dd, mut e) = (d[3], d[2], d[1], d[0], 0u32);
        for w in [s[3], s[2], s[1], s[0]] {
            let f = match select {
                0 => (b & c) | (!b & dd),
                2 => (b & c) | (b & dd) | (c & dd),
                _ => b ^ c ^ dd,
            };
            let t = f
                .wrapping_add(a.rotate_left(5))
                .wrapping_add(w)
                .wrapping_add(k)
                .wrapping_add(e);
            (e, dd, c, b, a) = (dd, c, b.rotate_left(30), a, t);
        }
        [dd, c, b, a]
    }

    fn sha256_rounds(d: [u32; 4], s: [u32; 4], wk: [u32; 4]) -> [u32; 4] {
        let (mut a, mut b, mut c, mut dd) = (s[3], s[2], d[3], d[2]);
        let (mut e, mut f, mut g, mut h) = (s[1], s[0], d[1], d[0]);
        for word in wk.into_iter().take(2) {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t1 = h.wrapping_add(sum1).wrapping_add(choice).wrapping_add(word);
            (h, g, f, e, dd, c, b, a) = (
                g,
                f,
                e,
                t1.wrapping_add(dd),
                c,
                b,
                a,
                t1.wrapping_add(sum0).wrapping_add(majority),
            );
        }
        [f, e, b, a]
    }
}
