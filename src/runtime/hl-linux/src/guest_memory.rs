#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestAccess {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestFault {
    pub address: u64,
    pub access: GuestAccess,
}

/// Linux-owned port for guest userspace access.
///
/// Successful operations may report a short accessible prefix. A zero-byte
/// success is invalid and treated as a fault by the marshaller.
pub trait GuestMemory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault>;

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault>;

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyProgress {
    pub copied: usize,
    pub fault: Option<GuestFault>,
}

impl CopyProgress {
    #[must_use]
    pub const fn complete(length: usize) -> Self {
        Self {
            copied: length,
            fault: None,
        }
    }

    #[must_use]
    pub const fn fault(copied: usize, fault: GuestFault) -> Self {
        Self {
            copied,
            fault: Some(fault),
        }
    }
}
