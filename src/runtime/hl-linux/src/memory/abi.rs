use hl_isa::{AddressRange, GuestAddress, GuestArchitecture, GuestPageSize};
use hl_memory::{Placement, Protection};

use crate::{GuestMarshaller, GuestMemory, MarshalError};

pub(crate) const PAGE: u64 = GuestPageSize::LINUX.bytes();
const PROTECTION_MASK: u32 = 0x7;
const MAP_SHARED: u32 = 0x1;
const MAP_PRIVATE: u32 = 0x2;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_DENYWRITE: u32 = 0x800;
const MAP_NORESERVE: u32 = 0x4000;
const MAP_FIXED_NOREPLACE: u32 = 0x10_0000;
const MAP_ALLOWED: u32 = 0x1f_f7f3 | MAP_DENYWRITE;
const MREMAP_MAYMOVE: u32 = 1;
const MREMAP_FIXED: u32 = 2;
const MREMAP_DONTUNMAP: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    Marshal(MarshalError),
    Invalid,
    Overflow,
}

impl From<MarshalError> for AbiError {
    fn from(error: MarshalError) -> Self {
        Self::Marshal(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapSource {
    Anonymous { shared: bool },
    File { descriptor: i32, shared: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MmapPlan {
    pub placement: Placement,
    /// Byte-exact length requested by the guest before Linux page rounding.
    pub requested_length: u64,
    pub length: u64,
    pub protection: Protection,
    pub source: MapSource,
    pub offset: u64,
    pub populate: bool,
    pub locked: bool,
    pub no_reserve: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangePlan {
    pub range: AddressRange,
    pub protection: Option<Protection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MremapPlan {
    pub old_range: AddressRange,
    pub requested_old_length: u64,
    pub new_length: u64,
    pub requested_new_length: u64,
    pub may_move: bool,
    pub fixed: Option<GuestAddress>,
    pub keep_old: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvicePlan {
    Noop,
    Apply { range: AddressRange, advice: Advice },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Advice {
    Normal,
    Random,
    Sequential,
    WillNeed,
    DontNeed,
    Free,
    Remove,
    DontFork,
    DoFork,
    WipeOnFork,
    KeepOnFork,
    Noop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockAllPlan {
    pub current: bool,
    pub future: bool,
    pub on_fault: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnlockAllPlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsyncPlan {
    pub range: Option<AddressRange>,
    pub asynchronous: bool,
    pub invalidate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemfdPlan {
    pub name: Vec<u8>,
    pub close_on_exec: bool,
    pub allow_sealing: bool,
    pub huge_page: Option<u32>,
}

pub struct Abi<'a, M: GuestMemory> {
    pub(crate) marshaller: GuestMarshaller<'a, M>,
    architecture: GuestArchitecture,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
            architecture,
        }
    }

    #[must_use]
    pub const fn brk(requested: u64) -> GuestAddress {
        GuestAddress::new(requested)
    }

    pub fn mmap(
        &self,
        address: u64,
        length: u64,
        protection: u32,
        flags: u32,
        descriptor: i32,
        offset: u64,
    ) -> Result<MmapPlan, AbiError> {
        self.mmap_plan(address, length, protection, flags, descriptor, offset, false)
    }

    pub fn mmap2(
        &self,
        address: u64,
        length: u64,
        protection: u32,
        flags: u32,
        descriptor: i32,
        page_offset: u64,
    ) -> Result<MmapPlan, AbiError> {
        self.mmap_plan(address, length, protection, flags, descriptor, page_offset, true)
    }

    fn mmap_plan(
        &self,
        address: u64,
        length: u64,
        protection: u32,
        flags: u32,
        descriptor: i32,
        offset: u64,
        offset_in_pages: bool,
    ) -> Result<MmapPlan, AbiError> {
        if length == 0 || protection & !PROTECTION_MASK != 0 || flags & !MAP_ALLOWED != 0 {
            return Err(AbiError::Invalid);
        }
        let sharing = flags & (MAP_SHARED | MAP_PRIVATE);
        if sharing != MAP_SHARED && sharing != MAP_PRIVATE {
            return Err(AbiError::Invalid);
        }
        let offset = if offset_in_pages {
            offset.checked_mul(PAGE).ok_or(AbiError::Overflow)?
        } else {
            offset
        };
        if offset % PAGE != 0 {
            return Err(AbiError::Invalid);
        }
        let rounded = Self::round_length(length)?;
        let placement = Self::placement(address, flags)?;
        let shared = sharing == MAP_SHARED;
        let source = if flags & MAP_ANONYMOUS != 0 {
            MapSource::Anonymous { shared }
        } else {
            if descriptor < 0 {
                return Err(AbiError::Invalid);
            }
            MapSource::File { descriptor, shared }
        };
        Ok(MmapPlan {
            placement,
            requested_length: length,
            length: rounded,
            protection: Self::protection(protection),
            source,
            offset,
            populate: flags & 0x8000 != 0,
            locked: flags & 0x2000 != 0,
            no_reserve: flags & MAP_NORESERVE != 0,
        })
    }

    fn placement(address: u64, flags: u32) -> Result<Placement, AbiError> {
        let address = GuestAddress::new(address);
        if flags & MAP_FIXED_NOREPLACE != 0 {
            if !address.is_page_aligned(GuestPageSize::LINUX) {
                return Err(AbiError::Invalid);
            }
            return Ok(Placement::FixedNoReplace(address));
        }
        if flags & MAP_FIXED != 0 {
            if !address.is_page_aligned(GuestPageSize::LINUX) {
                return Err(AbiError::Invalid);
            }
            return Ok(Placement::Fixed(address));
        }
        Ok(Placement::Anywhere {
            minimum: GuestAddress::new(PAGE),
            maximum: GuestAddress::new(u64::MAX & !(PAGE - 1)),
            hint: (address.get() != 0).then_some(address.page_base(GuestPageSize::LINUX)),
        })
    }

    pub fn munmap(address: u64, length: u64) -> Result<RangePlan, AbiError> {
        Ok(RangePlan {
            range: Self::range(address, length, true).map_err(|_| AbiError::Invalid)?,
            protection: None,
        })
    }

    pub fn mprotect(address: u64, length: u64, protection: u32) -> Result<Option<RangePlan>, AbiError> {
        if length == 0 {
            return Ok(None);
        }
        if protection & !(PROTECTION_MASK | 0x0100_0000 | 0x0200_0000) != 0 {
            return Err(AbiError::Invalid);
        }
        Ok(Some(RangePlan {
            range: Self::range(address, length, true)?,
            protection: Some(Self::protection(protection)),
        }))
    }

    pub fn mremap(
        old_address: u64,
        old_length: u64,
        new_length: u64,
        flags: u32,
        new_address: u64,
    ) -> Result<MremapPlan, AbiError> {
        if flags & !(MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 || new_length == 0 {
            return Err(AbiError::Invalid);
        }
        if flags & (MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 && flags & MREMAP_MAYMOVE == 0 {
            return Err(AbiError::Invalid);
        }
        let fixed = if flags & MREMAP_FIXED != 0 {
            let address = GuestAddress::new(new_address);
            if !address.is_page_aligned(GuestPageSize::LINUX) {
                return Err(AbiError::Invalid);
            }
            Some(address)
        } else {
            None
        };
        let requested_old_length = old_length;
        let requested_new_length = new_length;
        let old_range = Self::range(old_address, old_length, false)?;
        let new_length = Self::round_length(new_length)?;
        if let Some(destination) = fixed {
            let destination = AddressRange::nonempty(destination, new_length).map_err(|_| AbiError::Invalid)?;
            if destination.start() < old_range.end() && old_range.start() < destination.end() {
                return Err(AbiError::Invalid);
            }
        }
        if flags & MREMAP_DONTUNMAP != 0 && old_range.length() != new_length {
            return Err(AbiError::Invalid);
        }
        Ok(MremapPlan {
            old_range,
            requested_old_length,
            new_length,
            requested_new_length,
            may_move: flags & MREMAP_MAYMOVE != 0,
            fixed,
            keep_old: flags & MREMAP_DONTUNMAP != 0,
        })
    }

    pub fn madvise(address: u64, length: u64, advice: i32) -> Result<AdvicePlan, AbiError> {
        const ALLOWED: &[i32] = &[
            0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 21, 19, 22, 23, 25, 100, 101,
        ];
        if !ALLOWED.contains(&advice) {
            return Err(AbiError::Invalid);
        }
        if address & (PAGE - 1) != 0 {
            return Err(AbiError::Invalid);
        }
        if length == 0 {
            return Ok(AdvicePlan::Noop);
        }
        let advice = match advice {
            0 => Advice::Normal,
            1 => Advice::Random,
            2 => Advice::Sequential,
            3 => Advice::WillNeed,
            4 => Advice::DontNeed,
            8 => Advice::Free,
            9 => Advice::Remove,
            10 => Advice::DontFork,
            11 => Advice::DoFork,
            18 => Advice::WipeOnFork,
            19 => Advice::KeepOnFork,
            _ => Advice::Noop,
        };
        Ok(AdvicePlan::Apply {
            range: Self::range(address, length, true)?,
            advice,
        })
    }

    pub fn mlock(address: u64, length: u64) -> Result<Option<RangePlan>, AbiError> {
        Self::lock_range(address, length, 0)
    }

    pub fn mlock2(address: u64, length: u64, flags: u32) -> Result<Option<RangePlan>, AbiError> {
        Self::lock_range(address, length, flags)
    }

    pub fn munlock(address: u64, length: u64) -> Result<Option<RangePlan>, AbiError> {
        Self::lock_range(address, length, 0)
    }

    fn lock_range(address: u64, length: u64, flags: u32) -> Result<Option<RangePlan>, AbiError> {
        if flags & !1 != 0 {
            return Err(AbiError::Invalid);
        }
        if length == 0 {
            return Ok(None);
        }
        Ok(Some(RangePlan {
            range: Self::covering_range(address, length)?,
            protection: None,
        }))
    }

    pub fn mlockall(flags: u32) -> Result<LockAllPlan, AbiError> {
        if flags & !7 != 0 || flags & 3 == 0 || flags & 4 != 0 && flags & 2 == 0 {
            return Err(AbiError::Invalid);
        }
        Ok(LockAllPlan {
            current: flags & 1 != 0,
            future: flags & 2 != 0,
            on_fault: flags & 4 != 0,
        })
    }

    #[must_use]
    pub const fn munlockall() -> UnlockAllPlan {
        UnlockAllPlan
    }

    pub fn msync(address: u64, length: u64, flags: u32) -> Result<MsyncPlan, AbiError> {
        if flags & !7 != 0 || flags & 5 == 5 {
            return Err(AbiError::Invalid);
        }
        if address & (PAGE - 1) != 0 {
            return Err(AbiError::Invalid);
        }
        let range = if length == 0 {
            None
        } else {
            Some(Self::range(address, length, true)?)
        };
        Ok(MsyncPlan {
            range,
            asynchronous: flags & 1 != 0,
            invalidate: flags & 2 != 0,
        })
    }

    pub fn memfd_create(&self, name_pointer: u64, flags: u32) -> Result<MemfdPlan, AbiError> {
        const BASE_FLAGS: u32 = 0x1f;
        const HUGE_MASK: u32 = 0x3f << 26;
        if flags & !(BASE_FLAGS | HUGE_MASK) != 0 || flags & HUGE_MASK != 0 && flags & 4 == 0 {
            return Err(AbiError::Invalid);
        }
        let name = match self.marshaller.c_string(name_pointer, 250) {
            Err(crate::MarshalError::TooBig) => return Err(AbiError::Invalid),
            result => result?,
        };
        Ok(MemfdPlan {
            name,
            close_on_exec: flags & 1 != 0,
            allow_sealing: flags & 2 != 0,
            huge_page: (flags & 4 != 0).then_some((flags & HUGE_MASK) >> 26),
        })
    }

    #[must_use]
    pub const fn architecture(&self) -> GuestArchitecture {
        self.architecture
    }

    fn protection(bits: u32) -> Protection {
        let mut protection = Protection::NONE;
        if bits & 1 != 0 {
            protection = protection.union(Protection::READ);
        }
        if bits & 2 != 0 {
            protection = protection.union(Protection::WRITE);
        }
        if bits & 4 != 0 {
            protection = protection.union(Protection::EXECUTE);
        }
        protection
    }

    pub(crate) fn range(address: u64, length: u64, aligned_start: bool) -> Result<AddressRange, AbiError> {
        let start = GuestAddress::new(address);
        if length == 0 || aligned_start && !start.is_page_aligned(GuestPageSize::LINUX) {
            return Err(AbiError::Invalid);
        }
        AddressRange::nonempty(start, Self::round_length(length)?).map_err(|_| AbiError::Overflow)
    }

    fn round_length(length: u64) -> Result<u64, AbiError> {
        length
            .checked_add(PAGE - 1)
            .map(|value| value & !(PAGE - 1))
            .filter(|value| *value != 0)
            .ok_or(AbiError::Overflow)
    }

    fn covering_range(address: u64, length: u64) -> Result<AddressRange, AbiError> {
        let start = GuestAddress::new(address).page_base(GuestPageSize::LINUX);
        let offset = address - start.get();
        let covered = offset.checked_add(length).ok_or(AbiError::Overflow)?;
        AddressRange::nonempty(start, Self::round_length(covered)?).map_err(|_| AbiError::Overflow)
    }
}
