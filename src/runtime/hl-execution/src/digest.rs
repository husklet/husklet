/// Seed used by the retained C cache digest and identity algorithms.
pub const DIGEST_SEED: u64 = 1_469_598_103_934_665_603;
const PRIME: u64 = 1_099_511_628_211;

/// Retained-C-compatible word-at-a-time cache digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactDigest(u64);

impl ArtifactDigest {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().expect("exact chunk"));
            self.0 = (self.0 ^ word).wrapping_mul(PRIME);
        }
        for byte in chunks.remainder() {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn bytes(seed: u64, bytes: &[u8]) -> u64 {
        let mut digest = Self::new(seed);
        digest.update(bytes);
        digest.value()
    }
}
