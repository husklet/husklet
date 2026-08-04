#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicOrder {
    Relaxed,
    Acquire,
    Release,
    AcquireRelease,
}

// Coordinator atomic operations currently share one transaction lock. That
// deliberately strengthens every requested order to sequential consistency.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicOperation {
    Swap,
    Add,
    Clear,
    ExclusiveOr,
    Set,
    SignedMaximum,
    SignedMinimum,
    UnsignedMaximum,
    UnsignedMinimum,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtomicValue {
    pub low: u64,
    pub high: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExclusiveReservation {
    pub(crate) address: u64,
    pub(crate) element_bytes: u8,
    pub(crate) pair: bool,
    pub(crate) mapping_generation: u64,
    pub(crate) write_epoch: u64,
}

impl ExclusiveReservation {
    #[must_use]
    pub const fn new(address: u64, element_bytes: u8, pair: bool, mapping_generation: u64, write_epoch: u64) -> Self {
        Self {
            address,
            element_bytes,
            pair,
            mapping_generation,
            write_epoch,
        }
    }

    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    #[must_use]
    pub const fn element_bytes(self) -> u8 {
        self.element_bytes
    }

    #[must_use]
    pub const fn pair(self) -> bool {
        self.pair
    }

    #[must_use]
    pub const fn mapping_generation(self) -> u64 {
        self.mapping_generation
    }

    #[must_use]
    pub const fn write_epoch(self) -> u64 {
        self.write_epoch
    }
}
