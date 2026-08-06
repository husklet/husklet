use crate::PipeSnapshot;
use hl_descriptor::{CancellationNotification, ObjectError, Readiness, ReadinessRegistry};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
mod description;
mod endpoint_io;
mod named;
pub(crate) mod snapshot;
mod splice;
pub(crate) mod transfer;
pub const PIPE_BUF: usize = 4_096;
pub const DEFAULT_PIPE_CAPACITY: usize = 65_536;
pub const MAX_PIPE_CAPACITY: usize = 1_048_576;
pub const PIPE_CAPACITY_GRANULE: usize = 4_096;
const PIPE_MODE: u32 = 0o010_600;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeCreateError {
    InvalidCapacity,
    Busy,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PipeStatus {
    pub mode: u32,
    pub size: u64,
    pub link_count: u64,
}
pub use named::{
    NamedFifo, NamedFifoCatalog, NamedFifoKey, NamedFifoOpen, NamedFifoOpenError, NamedFifoStatus, NamedFifoWait,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EndpointDirection {
    Read,
    Write,
}

#[derive(Debug)]
pub(super) struct PipeState {
    pub(super) bytes: VecDeque<u8>,
    pub(super) head_fragment: usize,
    pub(super) packets: VecDeque<usize>,
    pub(super) packet_mode: bool,
    pub(super) capacity: usize,
    pub(super) readers: usize,
    pub(super) writers: usize,
    pub(super) read_nonblocking: bool,
    pub(super) write_nonblocking: bool,
    pub(super) waiters: usize,
    pub(super) open_waiters: usize,
    pub(super) sleepers: usize,
    #[cfg(test)]
    pub(super) sleeper_registration: Option<std::sync::mpsc::Sender<()>>,
    pub(super) splice_reserved: bool,
}

#[derive(Debug)]
pub(super) struct PipeShared {
    pub(super) state: Mutex<PipeState>,
    pub(super) changed: Condvar,
    reader_readiness: ReadinessRegistry,
    writer_readiness: ReadinessRegistry,
}

impl PipeShared {
    pub(super) fn notify_sleepers(&self, state: &PipeState) {
        if state.sleepers != 0 {
            self.changed.notify_all();
        }
    }
}

pub(super) struct PipeCancellationWake {
    pub(super) pipe: std::sync::Weak<PipeShared>,
}

impl CancellationNotification for PipeCancellationWake {
    fn notify(&self) {
        if let Some(pipe) = self.pipe.upgrade() {
            let state = pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            pipe.notify_sleepers(&state);
            drop(state);
        }
    }
}

/// A newly-created pair of Linux pipe endpoints.
#[derive(Debug)]
pub struct Pipe {
    pub reader: Arc<PipeEndpoint>,
    pub writer: Arc<PipeEndpoint>,
}

impl Pipe {
    #[must_use]
    pub fn new(nonblocking: bool) -> Self {
        Self::with_capacity(DEFAULT_PIPE_CAPACITY, nonblocking).expect("the default pipe capacity is valid")
    }

    #[must_use]
    pub fn new_packet(nonblocking: bool) -> Self {
        Self::with_capacity_mode(DEFAULT_PIPE_CAPACITY, nonblocking, true).expect("the default pipe capacity is valid")
    }

    pub fn with_capacity(capacity: usize, nonblocking: bool) -> Result<Self, PipeCreateError> {
        Self::with_capacity_mode(capacity, nonblocking, false)
    }

    fn with_capacity_mode(capacity: usize, nonblocking: bool, packet_mode: bool) -> Result<Self, PipeCreateError> {
        if !(PIPE_BUF..=MAX_PIPE_CAPACITY).contains(&capacity) {
            return Err(PipeCreateError::InvalidCapacity);
        }
        let shared = Arc::new(PipeShared {
            state: Mutex::new(PipeState {
                bytes: VecDeque::with_capacity(capacity),
                head_fragment: 0,
                packets: VecDeque::new(),
                packet_mode,
                capacity,
                readers: 1,
                writers: 1,
                read_nonblocking: nonblocking,
                write_nonblocking: nonblocking,
                waiters: 0,
                open_waiters: 0,
                sleepers: 0,
                #[cfg(test)]
                sleeper_registration: None,
                splice_reserved: false,
            }),
            changed: Condvar::new(),
            reader_readiness: ReadinessRegistry::new(),
            writer_readiness: ReadinessRegistry::new(),
        });
        Ok(Self {
            reader: Arc::new(PipeEndpoint::new(shared.clone(), EndpointDirection::Read)),
            writer: Arc::new(PipeEndpoint::new(shared, EndpointDirection::Write)),
        })
    }

    pub fn snapshot(&self) -> Result<PipeSnapshot, PipeCreateError> {
        let state = self.reader.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.waiters != 0 || state.open_waiters != 0 || state.sleepers != 0 {
            return Err(PipeCreateError::Busy);
        }
        Ok(PipeSnapshot {
            bytes: state.bytes.iter().copied().collect(),
            head_fragment: state.head_fragment,
            packets: state.packets.iter().copied().collect(),
            packet_mode: state.packet_mode,
            capacity: state.capacity,
            readers: state.readers,
            writers: state.writers,
            read_nonblocking: state.read_nonblocking,
            write_nonblocking: state.write_nonblocking,
        })
    }

    pub fn restore(snapshot: &PipeSnapshot) -> Result<Self, PipeCreateError> {
        snapshot.validate()?;
        let pipe = Self::with_capacity_mode(snapshot.capacity, false, snapshot.packet_mode)?;
        {
            let mut state = pipe.reader.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.bytes = snapshot.bytes.iter().copied().collect();
            state.head_fragment = snapshot.head_fragment;
            state.packets = snapshot.packets.iter().copied().collect();
            state.readers = snapshot.readers;
            state.writers = snapshot.writers;
            state.read_nonblocking = snapshot.read_nonblocking;
            state.write_nonblocking = snapshot.write_nonblocking;
        }
        pipe.reader.closed.store(snapshot.readers == 0, Ordering::Release);
        pipe.writer.closed.store(snapshot.writers == 0, Ordering::Release);
        pipe.reader.set_nonblocking(snapshot.read_nonblocking);
        pipe.writer.set_nonblocking(snapshot.write_nonblocking);
        Ok(pipe)
    }
}

/// One open-file-description endpoint of a shared pipe buffer.
#[derive(Debug)]
pub struct PipeEndpoint {
    pub(super) pipe: Arc<PipeShared>,
    pub(super) direction: EndpointDirection,
    retired: AtomicBool,
    closed: AtomicBool,
    nonblocking: AtomicBool,
}

impl PipeEndpoint {
    #[cfg(test)]
    pub(crate) fn sleeper_count(&self) -> usize {
        self.pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sleepers
    }

    #[cfg(test)]
    pub(crate) fn observe_sleeper_registrations(&self, sender: std::sync::mpsc::Sender<()>) {
        self.pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sleeper_registration = Some(sender);
    }

    fn new(pipe: Arc<PipeShared>, direction: EndpointDirection) -> Self {
        let nonblocking = {
            let state = pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match direction {
                EndpointDirection::Read => state.read_nonblocking,
                EndpointDirection::Write => state.write_nonblocking,
            }
        };
        Self::new_with_nonblocking(pipe, direction, nonblocking)
    }

    fn new_with_nonblocking(pipe: Arc<PipeShared>, direction: EndpointDirection, nonblocking: bool) -> Self {
        Self {
            pipe,
            direction,
            retired: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            nonblocking: AtomicBool::new(nonblocking),
        }
    }

    #[must_use]
    pub const fn status(&self) -> PipeStatus {
        PipeStatus {
            mode: PIPE_MODE,
            size: 0,
            link_count: 1,
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capacity
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bytes
            .len()
    }

    pub fn resize_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        if requested > MAX_PIPE_CAPACITY {
            return Err(ObjectError::PermissionDenied);
        }
        let requested = requested.max(PIPE_CAPACITY_GRANULE);
        let capacity = requested
            .checked_next_power_of_two()
            .ok_or(ObjectError::PermissionDenied)?;
        let mut state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if capacity < state.bytes.len() {
            return Err(ObjectError::Busy);
        }
        state.capacity = capacity;
        self.pipe.notify_sleepers(&state);
        drop(state);
        self.notify_readiness();
        Ok(capacity)
    }

    pub(super) fn set_nonblocking(&self, enabled: bool) {
        self.nonblocking.store(enabled, Ordering::Release);
        let mut state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.direction {
            EndpointDirection::Read => state.read_nonblocking = enabled,
            EndpointDirection::Write => state.write_nonblocking = enabled,
        }
        self.pipe.notify_sleepers(&state);
    }

    pub(super) fn endpoint_readiness(&self, interests: Readiness) -> Readiness {
        let state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = match self.direction {
            EndpointDirection::Read => {
                let mut ready = 0;
                if !state.bytes.is_empty() {
                    ready |= Readiness::READ;
                }
                if state.writers == 0 {
                    ready |= Readiness::HANGUP;
                }
                ready
            }
            EndpointDirection::Write => {
                if state.readers == 0 {
                    Readiness::ERROR
                } else if state.capacity - state.bytes.len() - state.head_fragment >= PIPE_BUF {
                    Readiness::WRITE
                } else {
                    0
                }
            }
        };
        Readiness::from_bits(ready & (interests.bits() | Readiness::ERROR | Readiness::HANGUP))
    }

    pub(super) fn retire_endpoint(&self) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pipe.notify_sleepers(&state);
        drop(state);
        self.endpoint_registry().notify();
        self.endpoint_registry().close();
    }

    pub(super) fn close_endpoint(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.direction {
            EndpointDirection::Read => state.readers -= 1,
            EndpointDirection::Write => state.writers -= 1,
        }
        self.pipe.notify_sleepers(&state);
        drop(state);
        self.notify_readiness();
        self.endpoint_registry().close();
    }

    pub(super) fn endpoint_registry(&self) -> &ReadinessRegistry {
        match self.direction {
            EndpointDirection::Read => &self.pipe.reader_readiness,
            EndpointDirection::Write => &self.pipe.writer_readiness,
        }
    }

    pub(super) fn notify_readiness(&self) {
        self.pipe.reader_readiness.notify();
        self.pipe.writer_readiness.notify();
    }
}
