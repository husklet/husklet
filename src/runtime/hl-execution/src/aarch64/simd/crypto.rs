use crate::Aarch64Instruction;

pub(crate) struct Crypto;

impl Crypto {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        crate::aarch64_simd_aes::Aes::decode(word)
            .or_else(|| crate::aarch64_simd_sha1::Sha1Unit::decode(word))
            .or_else(|| crate::aarch64_simd_sha256::Sha256Unit::decode(word))
    }
}
