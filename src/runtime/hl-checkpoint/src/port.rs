//! Consumer-owned byte stream ports and deterministic in-memory adapters.

use std::fmt;

/// Transport failure categories needed by checkpoint policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortError {
    Interrupted,
    WouldBlock,
    Canceled,
    Closed,
    Failed,
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "checkpoint transport {self:?}")
    }
}

impl std::error::Error for PortError {}

/// Transactional destination capability owned by the checkpoint domain.
pub trait CheckpointSink {
    fn begin(&mut self, image_size: usize) -> Result<(), PortError>;
    fn write(&mut self, bytes: &[u8]) -> Result<usize, PortError>;
    fn wait_writable(&mut self) -> Result<(), PortError>;
    fn commit(&mut self) -> Result<(), PortError>;
    fn abort(&mut self);
}

/// Bounded source capability owned by the checkpoint domain.
pub trait CheckpointSource {
    fn image_size(&mut self) -> Result<usize, PortError>;
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, PortError>;
    fn wait_readable(&mut self) -> Result<(), PortError>;
}

/// Deterministic failure injected at one port operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fault {
    pub operation: usize,
    pub error: PortError,
}

/// Transactional memory sink used by tests and embedding adapters.
#[derive(Debug, Default)]
pub struct MemorySink {
    committed: Option<Vec<u8>>,
    staged: Option<Vec<u8>>,
    expected: usize,
    operations: usize,
    fault: Option<Fault>,
    chunk: usize,
}

impl MemorySink {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            committed: None,
            staged: None,
            expected: 0,
            operations: 0,
            fault: None,
            chunk: usize::MAX,
        }
    }

    #[must_use]
    pub const fn with_fault(fault: Fault) -> Self {
        Self {
            committed: None,
            staged: None,
            expected: 0,
            operations: 0,
            fault: Some(fault),
            chunk: usize::MAX,
        }
    }

    pub fn set_chunk_size(&mut self, chunk: usize) {
        self.chunk = chunk.max(1);
    }

    pub fn inject(&mut self, fault: Fault) {
        self.operations = 0;
        self.fault = Some(fault);
    }

    #[must_use]
    pub fn committed(&self) -> Option<&[u8]> {
        self.committed.as_deref()
    }

    fn operation(&mut self) -> Result<(), PortError> {
        self.operations = self.operations.saturating_add(1);
        match self.fault {
            Some(fault) if fault.operation == self.operations => Err(fault.error),
            _ => Ok(()),
        }
    }
}

impl CheckpointSink for MemorySink {
    fn begin(&mut self, image_size: usize) -> Result<(), PortError> {
        self.operation()?;
        self.expected = image_size;
        self.staged = Some(Vec::with_capacity(image_size));
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, PortError> {
        self.operation()?;
        let count = bytes.len().min(self.chunk);
        let staged = self.staged.as_mut().ok_or(PortError::Closed)?;
        if staged.len().saturating_add(count) > self.expected {
            return Err(PortError::Failed);
        }
        staged.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn wait_writable(&mut self) -> Result<(), PortError> {
        self.operation()
    }

    fn commit(&mut self) -> Result<(), PortError> {
        self.operation()?;
        let staged = self.staged.take().ok_or(PortError::Closed)?;
        if staged.len() != self.expected {
            return Err(PortError::Failed);
        }
        self.committed = Some(staged);
        Ok(())
    }

    fn abort(&mut self) {
        self.staged = None;
        self.expected = 0;
    }
}

/// Memory source with partial-read and failure injection support.
#[derive(Debug)]
pub struct MemorySource {
    bytes: Vec<u8>,
    offset: usize,
    operations: usize,
    fault: Option<Fault>,
    chunk: usize,
}

impl MemorySource {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            offset: 0,
            operations: 0,
            fault: None,
            chunk: usize::MAX,
        }
    }

    #[must_use]
    pub fn with_fault(bytes: Vec<u8>, fault: Fault) -> Self {
        Self {
            bytes,
            offset: 0,
            operations: 0,
            fault: Some(fault),
            chunk: usize::MAX,
        }
    }

    pub fn set_chunk_size(&mut self, chunk: usize) {
        self.chunk = chunk.max(1);
    }

    fn operation(&mut self) -> Result<(), PortError> {
        self.operations = self.operations.saturating_add(1);
        match self.fault {
            Some(fault) if fault.operation == self.operations => Err(fault.error),
            _ => Ok(()),
        }
    }
}

impl CheckpointSource for MemorySource {
    fn image_size(&mut self) -> Result<usize, PortError> {
        self.operation()?;
        Ok(self.bytes.len())
    }

    fn read(&mut self, output: &mut [u8]) -> Result<usize, PortError> {
        self.operation()?;
        let remaining = self.bytes.len().saturating_sub(self.offset);
        let count = remaining.min(output.len()).min(self.chunk);
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }

    fn wait_readable(&mut self) -> Result<(), PortError> {
        self.operation()
    }
}
