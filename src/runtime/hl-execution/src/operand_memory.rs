#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryFault {
    pub instruction: u64,
    pub address: u64,
    pub access: AccessKind,
}

/// Transient operand-fault evidence used for signal classification.
///
/// Unlike `MemoryFault`, the access length is not architectural snapshot
/// state and is deliberately excluded from the execution checkpoint codec.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultAccess {
    fault: MemoryFault,
    length: NonZeroU64,
}

impl FaultAccess {
    pub const fn new(fault: MemoryFault, length: u64) -> Option<Self> {
        match NonZeroU64::new(length) {
            Some(length) => Some(Self { fault, length }),
            None => None,
        }
    }

    pub(crate) const fn operand(instruction: u64, address: u64, access: AccessKind, length: u64) -> Self {
        let Some(length) = NonZeroU64::new(length) else {
            panic!("operand access length must be nonzero");
        };
        Self {
            fault: MemoryFault {
                instruction,
                address,
                access,
            },
            length,
        }
    }

    pub const fn fault(self) -> MemoryFault {
        self.fault
    }
    pub const fn length(self) -> u64 {
        self.length.get()
    }
    pub const fn instruction(self) -> u64 {
        self.fault.instruction
    }
    pub const fn address(self) -> u64 {
        self.fault.address
    }
    pub const fn access(self) -> AccessKind {
        self.fault.access
    }
}

/// Neutral scalar guest-memory boundary shared by ISA interpreters.
///
/// Reads and committed values use guest little-endian significance. A
/// reservation covers one indivisible scalar write: failure publishes no
/// bytes, while commit publishes the complete requested width.
pub trait GuestOperandMemory {
    type Reservation;
    type BatchReservation;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()>;

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()>;

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()>;

    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64>;

    fn commit_write_batch(&mut self, reservation: Self::BatchReservation, values: &[u64]) -> Result<(), ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_access_length() {
        let fault = MemoryFault {
            instruction: 3,
            address: 5,
            access: AccessKind::Read,
        };
        assert!(FaultAccess::new(fault, 0).is_none());
        let access = FaultAccess::new(fault, 16).unwrap();
        assert_eq!(access.fault(), fault);
        assert_eq!(access.length(), 16);
    }
}
use std::num::NonZeroU64;
