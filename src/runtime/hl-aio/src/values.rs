#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ContextId(u64);

impl ContextId {
    pub(crate) const fn new(slot: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | (slot + 1) as u64)
    }

    pub(crate) const fn parts(self) -> Option<(usize, u32)> {
        let raw_slot = self.0 as u32;
        if raw_slot == 0 {
            return None;
        }
        Some(((raw_slot - 1) as usize, (self.0 >> 32) as u32))
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub data: u64,
    pub object: u64,
    pub result: i64,
    pub secondary: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AioError {
    InvalidArgument,
    ResourceLimit,
    Closing,
    Interrupted,
}
