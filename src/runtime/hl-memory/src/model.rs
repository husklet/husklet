use crate::{SharedBackingRef, SharedError};
use hl_isa::{AddressRange, GeometryError, GuestAddress, GuestPageSize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Protection(u8);

impl Protection {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);

    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !7 == 0 { Some(Self(bits)) } else { None }
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn contains(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub device: u64,
    pub object: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backing {
    Shared(SharedBackingRef),
    Anonymous { identity: u64, shared: bool },
    File { identity: FileIdentity, shared: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    pub(crate) range: AddressRange,
    pub(crate) protection: Protection,
    pub(crate) backing: Backing,
    pub(crate) backing_offset: u64,
}

impl Region {
    pub fn from_checkpoint(
        range: AddressRange,
        protection: Protection,
        backing: Backing,
        backing_offset: u64,
    ) -> Result<Self, MemoryError> {
        let region = Self {
            range,
            protection,
            backing,
            backing_offset,
        };
        region.validate()?;
        Ok(region)
    }
    #[must_use]
    pub const fn range(self) -> AddressRange {
        self.range
    }

    #[must_use]
    pub const fn protection(self) -> Protection {
        self.protection
    }

    #[must_use]
    pub const fn backing(self) -> Backing {
        self.backing
    }

    #[must_use]
    pub const fn backing_offset(self) -> u64 {
        self.backing_offset
    }

    pub(crate) const fn overlaps(self, range: AddressRange) -> bool {
        self.range.start().get() < range.end().get() && range.start().get() < self.range.end().get()
    }

    pub(crate) fn slice(self, start: GuestAddress, end: GuestAddress) -> Result<Self, MemoryError> {
        let offset = start
            .get()
            .checked_sub(self.range.start().get())
            .ok_or(MemoryError::InvariantViolation)?;
        Ok(Self {
            range: AddressRange::nonempty(start, end.get() - start.get())?,
            protection: self.protection,
            backing: self.backing,
            backing_offset: self
                .backing_offset
                .checked_add(offset)
                .ok_or(MemoryError::BackingOverflow)?,
        })
    }

    pub(crate) fn can_merge(self, right: Self) -> Result<bool, MemoryError> {
        if self.range.end() != right.range.start()
            || self.protection != right.protection
            || self.backing != right.backing
        {
            return Ok(false);
        }
        let next_offset = self
            .backing_offset
            .checked_add(self.range.length())
            .ok_or(MemoryError::BackingOverflow)?;
        Ok(next_offset == right.backing_offset)
    }

    pub(crate) fn merge(&mut self, right: Self) -> Result<(), MemoryError> {
        self.range = AddressRange::nonempty(self.range.start(), right.range.end().get() - self.range.start().get())?;
        Ok(())
    }

    pub(crate) fn validate(self) -> Result<(), MemoryError> {
        MappingRange::validate(self.range)?;
        self.backing_offset
            .checked_add(self.range.length())
            .ok_or(MemoryError::BackingOverflow)?;
        if self.backing_offset % GuestPageSize::LINUX.bytes() != 0 {
            return Err(MemoryError::Unaligned);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    Fixed(GuestAddress),
    FixedNoReplace(GuestAddress),
    Anywhere {
        minimum: GuestAddress,
        maximum: GuestAddress,
        hint: Option<GuestAddress>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapRequest {
    pub placement: Placement,
    pub length: u64,
    pub alignment: u64,
    pub protection: Protection,
    pub backing: Backing,
    pub backing_offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub region: Region,
    pub backing_offset: u64,
    pub contiguous: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FutexIdentity {
    Private {
        address_space: AddressSpaceId,
        address: u64,
    },
    SharedObject {
        slot: u32,
        generation: u32,
        offset: u64,
    },
    SharedAnonymous {
        object: u64,
        offset: u64,
    },
    SharedFile {
        device: u64,
        object: u64,
        offset: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FutexAccess {
    KeyOnly,
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AddressSpaceId {
    pub slot: u64,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryError {
    AddressOverflow,
    EmptyRange,
    Unaligned,
    AlreadyMapped,
    NoAddressSpace,
    ResourceLimit,
    Unmapped,
    BackingOverflow,
    InvariantViolation,
    Shared(SharedError),
}

impl From<SharedError> for MemoryError {
    fn from(error: SharedError) -> Self {
        Self::Shared(error)
    }
}

impl From<GeometryError> for MemoryError {
    fn from(error: GeometryError) -> Self {
        match error {
            GeometryError::AddressOverflow => Self::AddressOverflow,
            GeometryError::EmptyRange => Self::EmptyRange,
            GeometryError::AddressUnderflow
            | GeometryError::UnsupportedPageSize(_)
            | GeometryError::UnsupportedWordSize(_) => Self::InvariantViolation,
        }
    }
}

pub(crate) struct MappingRange;

impl MappingRange {
    pub(crate) fn create(start: GuestAddress, length: u64) -> Result<AddressRange, MemoryError> {
        let range = AddressRange::nonempty(start, length)?;
        Self::validate(range)?;
        Ok(range)
    }

    pub(crate) fn validate(range: AddressRange) -> Result<(), MemoryError> {
        if range.is_empty() {
            return Err(MemoryError::EmptyRange);
        }
        if !range.is_page_aligned(GuestPageSize::LINUX) {
            return Err(MemoryError::Unaligned);
        }
        Ok(())
    }
}
