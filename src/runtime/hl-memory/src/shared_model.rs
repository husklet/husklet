#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SharedObjectId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedBackingRef {
    pub object: SharedObjectId,
    pub offset: u64,
    pub length: u64,
    /// Whether this mapping may write back to the shared object. Linux memfd
    /// write seals use this independently of the current page protection:
    /// even a read-only MAP_SHARED mapping blocks F_SEAL_WRITE, while a
    /// writable MAP_PRIVATE mapping does not.
    pub write_shared: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SharedSeal(u8);

impl SharedSeal {
    pub const SEAL: u8 = 1;
    pub const SHRINK: u8 = 2;
    pub const GROW: u8 = 4;
    pub const WRITE: u8 = 8;
    pub const FUTURE_WRITE: u8 = 16;

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn contains(self, bits: u8) -> bool {
        self.0 & bits == bits
    }
    pub const fn intersects(self, bits: u8) -> bool {
        self.0 & bits != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedError {
    InvalidArgument,
    NotFound,
    ResourceLimit,
    Sealed,
    Busy,
    Range,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedLimits {
    pub objects: usize,
    pub object_bytes: usize,
    pub total_bytes: usize,
}

impl Default for SharedLimits {
    fn default() -> Self {
        Self {
            objects: 4096,
            object_bytes: 1 << 30,
            total_bytes: 1 << 32,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedObjectSnapshot {
    pub id: SharedObjectId,
    pub owner: u64,
    pub seals: SharedSeal,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedStoreSnapshot {
    /// Generation for every allocated slot, including currently vacant slots.
    ///
    /// Vacant generations are durable so a checkpoint restore cannot make a
    /// stale shared-object identity valid again.
    pub generations: Vec<u32>,
    pub objects: Vec<SharedObjectSnapshot>,
}
