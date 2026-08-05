use std::fmt;

const PROCESS_TAG: u64 = 0x5052_0000_0000_0000;
const THREAD_TAG: u64 = 0x5448_0000_0000_0000;
const SESSION_TAG: u64 = 0x5345_0000_0000_0000;
const PROCESS_GROUP_TAG: u64 = 0x5047_0000_0000_0000;
const TAG_MASK: u64 = 0xffff_0000_0000_0000;

/// Generation-qualified process identity scoped to one registry.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessId(u64);

impl ProcessId {
    pub(crate) const fn new(slot: u32, generation: u16) -> Self {
        Self(PROCESS_TAG | ((generation as u64) << 32) | slot as u64)
    }

    pub(crate) const fn parts(self) -> Option<(usize, u16)> {
        if self.0 & TAG_MASK != PROCESS_TAG {
            return None;
        }
        Some(((self.0 as u32) as usize, (self.0 >> 32) as u16))
    }

    /// Stable guest-visible number for the life of this identity.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 as u32 + 1
    }

    #[must_use]
    pub const fn fork_identity(self) -> crate::ForkEntityId {
        let slot = self.0 as u32;
        let generation = ((self.0 >> 32) as u16) as u32;
        crate::ForkEntityId { slot, generation }
    }

    #[must_use]
    pub fn from_fork_identity(identity: crate::ForkEntityId) -> Option<Self> {
        let generation = u16::try_from(identity.generation).ok()?;
        Some(Self::new(identity.slot, generation))
    }

    #[must_use]
    pub const fn wire_parts(self) -> (u32, u16) {
        (self.0 as u32, (self.0 >> 32) as u16)
    }

    #[must_use]
    pub const fn from_wire(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self::new(slot, generation))
        }
    }
}

impl fmt::Debug for ProcessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ProcessId").field(&self.number()).finish()
    }
}

/// Generation-qualified thread identity scoped to one registry.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ThreadId(u64);

impl ThreadId {
    pub(crate) const fn new(slot: u32, generation: u16) -> Self {
        Self(THREAD_TAG | ((generation as u64) << 32) | slot as u64)
    }

    pub(crate) const fn parts(self) -> Option<(usize, u16)> {
        if self.0 & TAG_MASK != THREAD_TAG {
            return None;
        }
        Some(((self.0 as u32) as usize, (self.0 >> 32) as u16))
    }

    /// Stable guest-visible number for the life of this identity.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 as u32 + 1
    }

    #[must_use]
    pub const fn wire_parts(self) -> (u32, u16) {
        (self.0 as u32, (self.0 >> 32) as u16)
    }

    #[must_use]
    pub const fn from_wire(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self::new(slot, generation))
        }
    }
}

impl fmt::Debug for ThreadId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ThreadId").field(&self.number()).finish()
    }
}

/// Generation-qualified session identity scoped to one registry.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(u64);

impl SessionId {
    pub(crate) const fn new(slot: u32, generation: u16) -> Self {
        Self(SESSION_TAG | ((generation as u64) << 32) | slot as u64)
    }

    pub(crate) const fn parts(self) -> Option<(usize, u16)> {
        if self.0 & TAG_MASK != SESSION_TAG {
            return None;
        }
        Some(((self.0 as u32) as usize, (self.0 >> 32) as u16))
    }

    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 as u32 + 1
    }

    #[must_use]
    pub const fn wire_parts(self) -> (u32, u16) {
        (self.0 as u32, (self.0 >> 32) as u16)
    }

    #[must_use]
    pub const fn from_wire(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self::new(slot, generation))
        }
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SessionId").field(&self.number()).finish()
    }
}

/// Generation-qualified process-group identity scoped to one registry.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessGroupId(u64);

impl ProcessGroupId {
    pub(crate) const fn new(slot: u32, generation: u16) -> Self {
        Self(PROCESS_GROUP_TAG | ((generation as u64) << 32) | slot as u64)
    }

    pub(crate) const fn parts(self) -> Option<(usize, u16)> {
        if self.0 & TAG_MASK != PROCESS_GROUP_TAG {
            return None;
        }
        Some(((self.0 as u32) as usize, (self.0 >> 32) as u16))
    }

    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 as u32 + 1
    }

    #[must_use]
    pub const fn wire_parts(self) -> (u32, u16) {
        (self.0 as u32, (self.0 >> 32) as u16)
    }

    #[must_use]
    pub const fn from_wire(slot: u32, generation: u16) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self::new(slot, generation))
        }
    }
}

impl fmt::Debug for ProcessGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ProcessGroupId").field(&self.number()).finish()
    }
}
