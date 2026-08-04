use crate::{MAX_PIPE_CAPACITY, PIPE_BUF, PipeCreateError};

use super::{EndpointDirection, PipeEndpoint};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub bytes: Vec<u8>,
    pub head_fragment: usize,
    pub packets: Vec<usize>,
    pub packet_mode: bool,
    pub capacity: usize,
    pub readers: usize,
    pub writers: usize,
    pub read_nonblocking: bool,
    pub write_nonblocking: bool,
}
pub type PipeSnapshot = Snapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedFifoSnapshot {
    pub identity: u64,
    pub linked: bool,
    pub pipe: PipeSnapshot,
}

impl NamedFifoSnapshot {
    pub fn validate(&self) -> Result<(), PipeCreateError> {
        self.pipe.validate()
    }
}

impl PipeSnapshot {
    pub fn validate(&self) -> Result<(), PipeCreateError> {
        if !(PIPE_BUF..=MAX_PIPE_CAPACITY).contains(&self.capacity)
            || self.bytes.len().saturating_add(self.head_fragment) > self.capacity
            || self.head_fragment >= PIPE_BUF
            || self.readers > 1
            || self.writers > 1
            || (self.packet_mode && self.packets.iter().sum::<usize>() != self.bytes.len())
            || self.packets.iter().any(|length| *length == 0 || *length > PIPE_BUF)
            || (!self.packet_mode && !self.packets.is_empty())
        {
            return Err(PipeCreateError::InvalidCapacity);
        }
        Ok(())
    }
}

impl PipeEndpoint {
    #[must_use]
    pub const fn checkpoint_kind(&self) -> crate::PipeEndpointKind {
        match self.direction {
            EndpointDirection::Read => crate::PipeEndpointKind::Reader,
            EndpointDirection::Write => crate::PipeEndpointKind::Writer,
        }
    }
}
