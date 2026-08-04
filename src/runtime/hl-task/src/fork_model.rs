pub const FORK_WIRE_VERSION: u16 = 1;
pub const MAX_FORK_PARTICIPANTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ForkEntityId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ForkCloneFlags(u8);

impl ForkCloneFlags {
    pub const FILES: Self = Self(1);
    pub const FS: Self = Self(2);
    pub const SIGHAND: Self = Self(4);
    pub const VM: Self = Self(8);
    pub const THREAD: Self = Self(16);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    pub fn validate(self) -> Result<(), ForkModelError> {
        if self.contains(Self::SIGHAND) && !self.contains(Self::VM)
            || self.contains(Self::THREAD) && (!self.contains(Self::VM) || !self.contains(Self::SIGHAND))
        {
            return Err(ForkModelError::InvalidFlags);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    pub parent: ForkEntityId,
    pub child: ForkEntityId,
    pub flags: ForkCloneFlags,
}

impl ForkRequest {
    pub fn validate(self) -> Result<(), ForkModelError> {
        if self.parent.generation == 0 || self.child.generation == 0 || self.parent == self.child {
            return Err(ForkModelError::InvalidIdentity);
        }
        self.flags.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkWireSnapshot {
    pub version: u16,
    pub transaction: u64,
    pub request: ForkRequest,
    pub phase: u8,
    pub participant_reservations: Vec<(u8, u64)>,
}

impl ForkWireSnapshot {
    pub fn validate(&self) -> Result<(), ForkModelError> {
        if self.version != FORK_WIRE_VERSION
            || self.transaction == 0
            || self.phase > 7
            || self.participant_reservations.len() > MAX_FORK_PARTICIPANTS
        {
            return Err(ForkModelError::InvalidWire);
        }
        self.request.validate()?;
        let mut previous = None;
        for (role, reservation) in &self.participant_reservations {
            Self::validate_reservation(previous, *role, *reservation)?;
            previous = Some(*role);
        }
        Ok(())
    }

    fn validate_reservation(previous: Option<u8>, role: u8, reservation: u64) -> Result<(), ForkModelError> {
        if role >= MAX_FORK_PARTICIPANTS as u8 || reservation == 0 || previous.is_some_and(|value| value >= role) {
            return Err(ForkModelError::InvalidWire);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkModelError {
    InvalidFlags,
    InvalidIdentity,
    InvalidWire,
}
