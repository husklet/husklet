use hl_task::CpuAffinity;

use crate::Errno;

pub const AFFINITY_BYTES: usize = CpuAffinity::MAX_CPUS / 8;

pub struct AffinityMask;

impl AffinityMask {
    #[must_use]
    pub fn encode(affinity: CpuAffinity) -> [u8; AFFINITY_BYTES] {
        let mut bytes = [0_u8; AFFINITY_BYTES];
        for (index, word) in affinity.words().iter().enumerate() {
            let start = index * 8;
            bytes[start..start + 8].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    pub fn decode(input: &[u8], online: usize) -> Result<CpuAffinity, Errno> {
        let mut words = [0_u64; AFFINITY_BYTES / 8];
        for (index, chunk) in input[..input.len().min(AFFINITY_BYTES)].chunks(8).enumerate() {
            let mut bytes = [0_u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            words[index] = u64::from_le_bytes(bytes);
        }
        CpuAffinity::intersect(words, CpuAffinity::online(online)).ok_or(Errno::EINVAL)
    }

    #[must_use]
    pub fn range(online: usize) -> String {
        let online = online.clamp(1, CpuAffinity::MAX_CPUS);
        if online == 1 {
            "0\n".to_owned()
        } else {
            format!("0-{}\n", online - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_affinity() {
        let online = CpuAffinity::online(10);
        let mask = AffinityMask::encode(online);
        assert_eq!(&mask[..3], &[0xff, 0x03, 0]);
        assert_eq!(online.first(), 0);

        let mut wanted = [0_u8; AFFINITY_BYTES];
        wanted[0] = 0x08;
        wanted[1] = 0x80;
        let selected = AffinityMask::decode(&wanted, 10).unwrap();
        let mask = AffinityMask::encode(selected);
        assert_eq!(&mask[..2], &[0x08, 0]);
        assert_eq!(selected.first(), 3);

        wanted.fill(0);
        wanted[4] = 1;
        assert_eq!(AffinityMask::decode(&wanted, 10), Err(Errno::EINVAL));
        assert_eq!(selected.first(), 3);
        assert_eq!(AffinityMask::range(1), "0\n");
        assert_eq!(AffinityMask::range(10), "0-9\n");
    }
}
