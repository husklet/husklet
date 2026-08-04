use std::{error::Error, fmt};

use hl_isa::GuestArchitecture;

use crate::{AARCH64_CACHE_ABI, ArtifactDigest, CacheCompatibility, DIGEST_SEED, X86_64_CACHE_ABI};

const CACHE_SIZE: u64 = 64 << 20;
const MAP_MAX: u64 = 1 << 19;
const RELOC_MAX: u64 = 1 << 20;
const PEND_MAX: u64 = 1 << 16;
const LIB_MAX: u64 = 512;
const T2_MAX: u64 = 8192;
const TXPG_MAX: u64 = 1 << 18;
const PROV_MAX: u64 = 1 << 16;
const IMG_BASE: u64 = 0x0000_0400_0000_0000;
const INTERP_BASE: u64 = 0x0000_0480_0000_0000;
const LIB_BASE: u64 = 0x0000_0500_0000_0000;
const LIB_END: u64 = LIB_BASE + (1 << 38);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheExpectations {
    pub cpu_size: u64,
    pub map_entries: u64,
    pub ibtc_entries: u64,
    pub binary_identity: u64,
    pub entry: u64,
    pub live_arena_rx: u64,
    pub forward_skip: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedCache<'a> {
    architecture: GuestArchitecture,
    arena: &'a [u8],
    sections: Vec<&'a [u8]>,
}

impl<'a> RetainedCache<'a> {
    pub fn parse(
        architecture: GuestArchitecture,
        bytes: &'a [u8],
        expected: CacheExpectations,
    ) -> Result<Self, RetainedCacheError> {
        match architecture {
            GuestArchitecture::Aarch64 => Self::parse_aarch64(bytes, expected),
            GuestArchitecture::X86_64 => Self::parse_x86(bytes, expected),
        }
    }

    pub const fn architecture(&self) -> GuestArchitecture {
        self.architecture
    }
    pub fn arena(&self) -> &'a [u8] {
        self.arena
    }
    pub fn sections(&self) -> &[&'a [u8]] {
        &self.sections
    }

    fn parse_x86(bytes: &'a [u8], expected: CacheExpectations) -> Result<Self, RetainedCacheError> {
        let words = Validator::header(bytes, 18)?;
        Validator::common(&words, 0x3130_4350_544a_4c48, 9, X86_64_CACHE_ABI, expected)?;
        let arena_size = Validator::bounded(words[10], CACHE_SIZE)?;
        Validator::bounds(&[
            (words[11], MAP_MAX),
            (words[12], PEND_MAX),
            (words[13], RELOC_MAX),
            (words[14], LIB_MAX),
        ])?;
        let sizes = [
            (words[13], 8),
            (words[11], 40),
            (words[12], 24),
            (words[14], 24),
            (arena_size, 1),
        ];
        let sections = Validator::split(bytes, 144, &sizes)?;
        Validator::checksum(&sections, words[15])?;
        Validator::validate_x86_relocs(sections[0], arena_size)?;
        Validator::validate_pair_offsets(sections[1], arena_size, 40, 24, 32, 1)?;
        Validator::validate_offsets(sections[2], arena_size, 24, 1)?;
        Validator::validate_libs(sections[3])?;
        Ok(Self {
            architecture: GuestArchitecture::X86_64,
            arena: sections[4],
            sections,
        })
    }

    fn parse_aarch64(bytes: &'a [u8], expected: CacheExpectations) -> Result<Self, RetainedCacheError> {
        let words = Validator::header(bytes, 22)?;
        Validator::common(&words, 0x3441_4350_544a_4c48, 11, AARCH64_CACHE_ABI, expected)?;
        let arena_size = Validator::bounded(words[10], CACHE_SIZE)?;
        if arena_size & 3 != 0 {
            return Err(RetainedCacheError::Header);
        }
        Validator::bounds(&[
            (words[11], RELOC_MAX),
            (words[12], MAP_MAX),
            (words[13], PEND_MAX),
            (words[14], T2_MAX),
            (words[15], TXPG_MAX),
            (words[16], PROV_MAX),
            (words[17], LIB_MAX),
        ])?;
        let sizes = [
            (words[11], 8),
            (words[12], 40),
            (words[13], 40),
            (words[14], 16),
            (words[15], 8),
            (words[16], 24),
            (words[17], 24),
            (arena_size, 1),
        ];
        let sections = Validator::split(bytes, 176, &sizes)?;
        Validator::checksum(&sections, words[18])?;
        Validator::validate_aarch64_relocs(sections[0], sections[7], words[21], expected.live_arena_rx)?;
        Validator::validate_maps(sections[1], arena_size)?;
        Validator::validate_aarch64_pends(sections[2], arena_size, expected.forward_skip)?;
        Validator::validate_provenance(sections[5], arena_size)?;
        Validator::validate_libs(sections[6])?;
        Ok(Self {
            architecture: GuestArchitecture::Aarch64,
            arena: sections[7],
            sections,
        })
    }
}

struct Validator;

impl Validator {
    fn common(
        words: &[u64],
        magic: u64,
        version: u64,
        abi: u64,
        e: CacheExpectations,
    ) -> Result<(), RetainedCacheError> {
        let stored = CacheCompatibility {
            format: words[1],
            translator_abi: words[2],
        };
        let current = CacheCompatibility {
            format: version,
            translator_abi: abi,
        };
        if words[0] != magic
            || !stored.is_compatible(current)
            || words[3] != e.cpu_size
            || words[4] != e.map_entries
            || words[5] != e.ibtc_entries
            || words[6] != IMG_BASE
            || words[7] != INTERP_BASE
            || words[8] != e.binary_identity
            || words[9] != e.entry
        {
            return Err(RetainedCacheError::Header);
        }
        Ok(())
    }

    fn header(bytes: &[u8], count: usize) -> Result<Vec<u64>, RetainedCacheError> {
        let header = bytes.get(..count * 8).ok_or(RetainedCacheError::Truncated)?;
        Ok(header
            .chunks_exact(8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("chunk")))
            .collect())
    }

    fn bounded(value: u64, maximum: u64) -> Result<u64, RetainedCacheError> {
        if value > maximum {
            Err(RetainedCacheError::Limit)
        } else {
            Ok(value)
        }
    }

    fn bounds(values: &[(u64, u64)]) -> Result<(), RetainedCacheError> {
        for &(value, maximum) in values {
            Self::bounded(value, maximum)?;
        }
        Ok(())
    }

    fn split<'a>(
        bytes: &'a [u8],
        mut offset: usize,
        sizes: &[(u64, usize)],
    ) -> Result<Vec<&'a [u8]>, RetainedCacheError> {
        let mut result = Vec::with_capacity(sizes.len());
        for &(count, width) in sizes {
            let size = usize::try_from(count)
                .ok()
                .and_then(|n| n.checked_mul(width))
                .ok_or(RetainedCacheError::Limit)?;
            let end = offset.checked_add(size).ok_or(RetainedCacheError::Limit)?;
            result.push(bytes.get(offset..end).ok_or(RetainedCacheError::Truncated)?);
            offset = end;
        }
        if offset != bytes.len() {
            return Err(RetainedCacheError::Trailing);
        }
        Ok(result)
    }

    fn checksum(sections: &[&[u8]], expected: u64) -> Result<(), RetainedCacheError> {
        let mut digest = ArtifactDigest::new(DIGEST_SEED);
        for section in sections {
            digest.update(section);
        }
        if digest.value() != expected {
            Err(RetainedCacheError::Checksum)
        } else {
            Ok(())
        }
    }

    fn read64(record: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(record[offset..offset + 8].try_into().expect("validated record"))
    }

    fn window(arena: u64, offset: u64, width: u64, alignment: u64) -> bool {
        offset % alignment == 0 && offset.checked_add(width).is_some_and(|end| end <= arena)
    }

    fn validate_offsets(bytes: &[u8], arena: u64, stride: usize, alignment: u64) -> Result<(), RetainedCacheError> {
        let width = if stride == 24 { 4 } else { 16 };
        for record in bytes.chunks_exact(stride) {
            if !Self::window(arena, Self::read64(record, 0), width, alignment) {
                return Err(RetainedCacheError::Record);
            }
        }
        Ok(())
    }

    fn validate_x86_relocs(bytes: &[u8], arena: u64) -> Result<(), RetainedCacheError> {
        if bytes.chunks_exact(8).all(|record| {
            let offset = u64::from(u32::from_le_bytes(record[..4].try_into().expect("record")));
            Self::window(arena, offset, 16, 1)
        }) {
            Ok(())
        } else {
            Err(RetainedCacheError::Record)
        }
    }

    fn validate_pair_offsets(
        bytes: &[u8],
        arena: u64,
        stride: usize,
        first: usize,
        second: usize,
        alignment: u64,
    ) -> Result<(), RetainedCacheError> {
        if bytes.chunks_exact(stride).all(|r| {
            Self::window(arena, Self::read64(r, first), 1, alignment)
                && Self::window(arena, Self::read64(r, second), 1, alignment)
        }) {
            Ok(())
        } else {
            Err(RetainedCacheError::Record)
        }
    }

    fn validate_maps(bytes: &[u8], arena: u64) -> Result<(), RetainedCacheError> {
        if bytes.chunks_exact(40).all(|r| {
            let gpc = Self::read64(r, 0);
            Self::window(arena, Self::read64(r, 24), 1, 4)
                && Self::window(arena, Self::read64(r, 32), 1, 4)
                && Self::read64(r, 8) <= gpc
                && gpc < Self::read64(r, 16)
        }) {
            Ok(())
        } else {
            Err(RetainedCacheError::Record)
        }
    }

    fn validate_aarch64_pends(bytes: &[u8], arena: u64, forward_skip: bool) -> Result<(), RetainedCacheError> {
        for r in bytes.chunks_exact(40) {
            let kind = u32::from_le_bytes(r[24..28].try_into().expect("record"));
            let fwd = u32::from_le_bytes(r[28..32].try_into().expect("record"));
            if !Self::window(arena, Self::read64(r, 0), 4, 4) || kind > 2 || fwd > 1 {
                return Err(RetainedCacheError::Record);
            }
            if kind == 2 {
                Self::validate_conditional_pending(r, fwd, forward_skip)?;
            }
        }
        Ok(())
    }

    fn validate_conditional_pending(record: &[u8], fwd: u32, forward_skip: bool) -> Result<(), RetainedCacheError> {
        let original = u32::from_le_bytes(record[32..36].try_into().expect("record"));
        let source = Self::read64(record, 16);
        let target = Self::read64(record, 8);
        let opcode = original & 0xff00_0010 == 0x5400_0000
            || original & 0x7e00_0000 == 0x3400_0000
            || original & 0x7e00_0000 == 0x3600_0000;
        if !opcode || source & 3 != 0 || (fwd != 0) != (forward_skip && target > source) {
            return Err(RetainedCacheError::Record);
        }
        Ok(())
    }

    fn validate_provenance(bytes: &[u8], arena: u64) -> Result<(), RetainedCacheError> {
        if bytes.chunks_exact(24).all(|r| {
            let size = u64::from(u32::from_le_bytes(r[16..20].try_into().expect("record")));
            let reserved = u32::from_le_bytes(r[20..24].try_into().expect("record"));
            reserved == 0 && size != 0 && Self::window(arena, Self::read64(r, 0), size, 4)
        }) {
            Ok(())
        } else {
            Err(RetainedCacheError::Record)
        }
    }

    fn validate_libs(bytes: &[u8]) -> Result<(), RetainedCacheError> {
        let records: Vec<_> = bytes.chunks_exact(24).collect();
        for (index, record) in records.iter().enumerate() {
            let base = Self::read64(record, 0);
            let len = Self::read64(record, 8);
            let id = Self::read64(record, 16);
            let end = base.checked_add(len).ok_or(RetainedCacheError::Record)?;
            if id == 0 || len == 0 || base & 0x1f_ffff != 0 || base < LIB_BASE || end > LIB_END {
                return Err(RetainedCacheError::Record);
            }
            Self::validate_nonoverlap(base, end, &records[..index])?;
        }
        Ok(())
    }

    fn validate_nonoverlap(base: u64, end: u64, records: &[&[u8]]) -> Result<(), RetainedCacheError> {
        for other in records {
            let other_base = Self::read64(other, 0);
            let other_end = other_base
                .checked_add(Self::read64(other, 8))
                .ok_or(RetainedCacheError::Record)?;
            if end > other_base && base < other_end {
                return Err(RetainedCacheError::Record);
            }
        }
        Ok(())
    }

    fn validate_aarch64_relocs(
        bytes: &[u8],
        arena: &[u8],
        saved_rx: u64,
        live_rx: u64,
    ) -> Result<(), RetainedCacheError> {
        for record in bytes.chunks_exact(8) {
            let off = u64::from(u32::from_le_bytes(record[..4].try_into().expect("record")));
            let info = u32::from_le_bytes(record[4..].try_into().expect("record"));
            let kind = info & 0xff;
            let rd = (info >> 8) & 0xff;
            let slot = info >> 16;
            let width = if kind == 6 { 4 } else { 16 };
            let alignment = if kind == 4 { 8 } else { 4 };
            if !Self::window(arena.len() as u64, off, width, alignment)
                || !matches!(kind, 1..=6)
                || (kind != 4 && rd > 30)
                || (kind == 3 && slot >= T2_MAX as u32)
            {
                return Err(RetainedCacheError::Record);
            }
            if kind == 6 {
                Self::validate_adrp(arena, off, rd, saved_rx, live_rx)?;
            }
        }
        Ok(())
    }

    fn validate_adrp(
        arena: &[u8],
        offset: u64,
        register: u32,
        saved_rx: u64,
        live_rx: u64,
    ) -> Result<(), RetainedCacheError> {
        let at = offset as usize;
        let word = u32::from_le_bytes(arena[at..at + 4].try_into().expect("window"));
        if word & 0x9f00_0000 != 0x9000_0000 || word & 31 != register {
            return Err(RetainedCacheError::Record);
        }
        let immediate = ((word >> 29) & 3) | (((word >> 5) & 0x7ffff) << 2);
        let pages = ((immediate << 11) as i32 >> 11) as i64;
        let saved_page = saved_rx.wrapping_add(offset) & !0xfff;
        let target = saved_page.wrapping_add((pages * 4096) as u64);
        let live_page = live_rx.wrapping_add(offset) & !0xfff;
        let delta = target.wrapping_sub(live_page) as i64;
        if delta & 0xfff != 0 || !(-0x1_0000_0000..=0xffff_f000).contains(&delta) {
            return Err(RetainedCacheError::Record);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedCacheError {
    Truncated,
    Header,
    Limit,
    Trailing,
    Checksum,
    Record,
}
impl fmt::Display for RetainedCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for RetainedCacheError {}
