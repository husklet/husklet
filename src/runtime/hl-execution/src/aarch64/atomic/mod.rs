#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Order {
    Relaxed,
    Acquire,
    Release,
    AcquireRelease,
    SequentiallyConsistent,
}

impl Order {
    pub(crate) fn from_bits(acquire: bool, release: bool) -> Self {
        match (acquire, release) {
            (false, false) => Self::Relaxed,
            (true, false) => Self::Acquire,
            (false, true) => Self::Release,
            (true, true) => Self::AcquireRelease,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
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
pub struct Value {
    pub low: u64,
    pub high: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generation(u64);

impl Generation {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reservation {
    address: u64,
    element_bytes: u8,
    pair: bool,
    generation: Generation,
    writes: Generation,
}

impl Reservation {
    pub const fn new(address: u64, element_bytes: u8, pair: bool, generation: Generation) -> Self {
        Self {
            address,
            element_bytes,
            pair,
            generation,
            writes: generation,
        }
    }

    pub const fn versioned(
        address: u64,
        element_bytes: u8,
        pair: bool,
        generation: Generation,
        writes: Generation,
    ) -> Self {
        Self {
            address,
            element_bytes,
            pair,
            generation,
            writes,
        }
    }

    pub const fn address(self) -> u64 {
        self.address
    }

    pub const fn bytes(self) -> u8 {
        self.element_bytes * if self.pair { 2 } else { 1 }
    }

    pub const fn element_bytes(self) -> u8 {
        self.element_bytes
    }

    pub const fn pair(self) -> bool {
        self.pair
    }

    pub const fn generation(self) -> Generation {
        self.generation
    }

    pub const fn write_epoch(self) -> Generation {
        self.writes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Load {
    pub value: Value,
    pub reservation: Reservation,
}

/// Atomic guest-memory capability owned by the execution consumer.
///
/// Implementations serialize only overlapping locations. Reservation validity
/// includes both mapping generation and exact byte range; mapping replacement
/// and any conflicting committed store invalidate a prior reservation.
pub trait Memory {
    fn load_ordered(&mut self, address: u64, bytes: u8, order: Order) -> Result<u64, ()>;

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, order: Order) -> Result<(), ()>;

    fn load_exclusive(&mut self, address: u64, element_bytes: u8, pair: bool, order: Order) -> Result<Load, ()>;

    /// Releases implementation-owned state associated with a local monitor.
    ///
    /// Stateless implementations need no work. Adapters that translate the
    /// architectural reservation into an opaque host token must discard that
    /// token when execution rejects an STXR before attempting the host store.
    fn discard_exclusive(&mut self, _reservation: Reservation) {}

    fn store_exclusive(&mut self, reservation: Reservation, replacement: Value, order: Order) -> Result<bool, ()>;

    fn compare_exchange(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        expected: Value,
        replacement: Value,
        order: Order,
    ) -> Result<Value, ()>;

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: Operation,
        operand: u64,
        order: Order,
    ) -> Result<u64, ()>;
}

mod decode;
mod execute;

pub(crate) use decode::Decoder;
pub(crate) use execute::Executor;

#[cfg(test)]
mod test;
