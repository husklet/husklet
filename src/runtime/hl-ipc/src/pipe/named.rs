use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NamedFifoKey {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Default)]
pub struct NamedFifoCatalog {
    entries: Mutex<std::collections::BTreeMap<NamedFifoKey, Arc<NamedFifo>>>,
    next_identity: std::sync::atomic::AtomicU64,
}

use super::{DEFAULT_PIPE_CAPACITY, EndpointDirection, PipeCreateError, PipeEndpoint, PipeShared, PipeState};
use hl_descriptor::ReadinessRegistry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamedFifoOpenError {
    NoReader,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamedFifoStatus {
    pub identity: u64,
    pub linked: bool,
    pub readers: usize,
    pub writers: usize,
    pub waiting: usize,
}

#[derive(Debug)]
pub enum NamedFifoOpen {
    Ready(Arc<PipeEndpoint>),
    Waiting(NamedFifoWait),
}

#[derive(Debug)]
pub struct NamedFifo {
    identity: u64,
    linked: AtomicBool,
    shared: Arc<PipeShared>,
}

#[derive(Debug)]
pub struct NamedFifoWait {
    endpoint: Option<Arc<PipeEndpoint>>,
}

impl NamedFifo {
    #[must_use]
    pub fn new(identity: u64) -> Self {
        Self::with_capacity(identity, DEFAULT_PIPE_CAPACITY).expect("default FIFO capacity is valid")
    }

    pub fn with_capacity(identity: u64, capacity: usize) -> Result<Self, PipeCreateError> {
        if !(super::PIPE_BUF..=super::MAX_PIPE_CAPACITY).contains(&capacity) {
            return Err(PipeCreateError::InvalidCapacity);
        }
        Ok(Self {
            identity,
            linked: AtomicBool::new(true),
            shared: Arc::new(PipeShared {
                state: std::sync::Mutex::new(PipeState {
                    bytes: std::collections::VecDeque::with_capacity(capacity),
                    head_fragment: 0,
                    packets: std::collections::VecDeque::new(),
                    packet_mode: false,
                    capacity,
                    readers: 0,
                    writers: 0,
                    read_nonblocking: false,
                    write_nonblocking: false,
                    waiters: 0,
                    open_waiters: 0,
                    sleepers: 0,
                    #[cfg(test)]
                    sleeper_registration: None,
                    splice_reserved: false,
                }),
                changed: std::sync::Condvar::new(),
                reader_readiness: ReadinessRegistry::new(),
                writer_readiness: ReadinessRegistry::new(),
            }),
        })
    }

    pub fn open_reader(&self, nonblocking: bool) -> NamedFifoOpen {
        self.open(EndpointDirection::Read, nonblocking)
            .expect("reader open cannot fail")
    }

    pub fn open_writer(&self, nonblocking: bool) -> Result<NamedFifoOpen, NamedFifoOpenError> {
        self.open(EndpointDirection::Write, nonblocking)
    }

    /// Opens both sides of a FIFO as one Linux `O_RDWR` open operation.
    ///
    /// Linux does not wait for a peer in this mode. Publishing both endpoint
    /// counts while holding the shared-state lock is important: a concurrent
    /// blocking writer or reader must observe the duplex opener atomically.
    pub fn open_readwrite(&self, nonblocking: bool) -> (Arc<PipeEndpoint>, Arc<PipeEndpoint>) {
        let mut state = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.readers += 1;
        state.writers += 1;
        state.read_nonblocking = nonblocking;
        state.write_nonblocking = nonblocking;
        let reader = Arc::new(PipeEndpoint::new_with_nonblocking(
            Arc::clone(&self.shared),
            EndpointDirection::Read,
            nonblocking,
        ));
        let writer = Arc::new(PipeEndpoint::new_with_nonblocking(
            Arc::clone(&self.shared),
            EndpointDirection::Write,
            nonblocking,
        ));
        self.shared.notify_sleepers(&state);
        drop(state);
        reader.notify_readiness();
        (reader, writer)
    }

    fn open(&self, direction: EndpointDirection, nonblocking: bool) -> Result<NamedFifoOpen, NamedFifoOpenError> {
        let mut state = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if direction == EndpointDirection::Write && nonblocking && state.readers == 0 {
            return Err(NamedFifoOpenError::NoReader);
        }
        let matched = match direction {
            EndpointDirection::Read => state.writers != 0,
            EndpointDirection::Write => state.readers != 0,
        };
        match direction {
            EndpointDirection::Read => {
                state.readers += 1;
                state.read_nonblocking = nonblocking;
            }
            EndpointDirection::Write => {
                state.writers += 1;
                state.write_nonblocking = nonblocking;
            }
        }
        let endpoint = Arc::new(PipeEndpoint::new_with_nonblocking(
            Arc::clone(&self.shared),
            direction,
            nonblocking,
        ));
        if nonblocking || matched {
            self.shared.notify_sleepers(&state);
            drop(state);
            endpoint.notify_readiness();
            Ok(NamedFifoOpen::Ready(endpoint))
        } else {
            state.open_waiters += 1;
            self.shared.notify_sleepers(&state);
            drop(state);
            endpoint.notify_readiness();
            Ok(NamedFifoOpen::Waiting(NamedFifoWait {
                endpoint: Some(endpoint),
            }))
        }
    }

    pub fn unlink(&self) {
        self.linked.store(false, Ordering::Release);
    }

    pub fn snapshot(&self) -> Result<crate::NamedFifoSnapshot, PipeCreateError> {
        let state = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.waiters != 0 || state.open_waiters != 0 || state.sleepers != 0 {
            return Err(PipeCreateError::Busy);
        }
        Ok(crate::NamedFifoSnapshot {
            identity: self.identity,
            linked: self.linked.load(Ordering::Acquire),
            pipe: crate::PipeSnapshot {
                bytes: state.bytes.iter().copied().collect(),
                head_fragment: state.head_fragment,
                packets: state.packets.iter().copied().collect(),
                packet_mode: state.packet_mode,
                capacity: state.capacity,
                readers: state.readers,
                writers: state.writers,
                read_nonblocking: state.read_nonblocking,
                write_nonblocking: state.write_nonblocking,
            },
        })
    }

    #[must_use]
    pub fn status(&self) -> NamedFifoStatus {
        let state = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        NamedFifoStatus {
            identity: self.identity,
            linked: self.linked.load(Ordering::Acquire),
            readers: state.readers,
            writers: state.writers,
            waiting: state.open_waiters,
        }
    }

    #[must_use]
    pub fn reclaimable(&self) -> bool {
        let status = self.status();
        !status.linked && status.readers == 0 && status.writers == 0 && status.waiting == 0
    }
}

impl NamedFifoCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(std::collections::BTreeMap::new()),
            next_identity: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn open(&self, key: NamedFifoKey) -> Arc<NamedFifo> {
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(fifo) = entries.get(&key) {
            return Arc::clone(fifo);
        }
        let identity = self.next_identity.fetch_add(1, Ordering::AcqRel);
        let fifo = Arc::new(NamedFifo::new(identity));
        entries.insert(key, Arc::clone(&fifo));
        fifo
    }

    pub fn unlink(&self, key: NamedFifoKey, last_link: bool) {
        if !last_link {
            return;
        }
        let fifo = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        if let Some(fifo) = fifo {
            fifo.unlink();
        }
    }
}

impl NamedFifoWait {
    #[must_use]
    pub fn ready(&self) -> bool {
        let endpoint = self.endpoint.as_ref().expect("completed FIFO wait");
        let state = endpoint.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match endpoint.direction {
            EndpointDirection::Read => state.writers != 0,
            EndpointDirection::Write => state.readers != 0,
        }
    }

    pub fn complete(mut self) -> Result<Arc<PipeEndpoint>, Self> {
        if !self.ready() {
            return Err(self);
        }
        let endpoint = self.endpoint.take().expect("completed FIFO wait");
        let mut state = endpoint.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open_waiters -= 1;
        drop(state);
        Ok(endpoint)
    }

    #[must_use]
    pub fn wait(mut self) -> Arc<PipeEndpoint> {
        loop {
            let endpoint = self.endpoint.as_ref().expect("completed FIFO wait");
            let mut state = endpoint.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let ready = match endpoint.direction {
                EndpointDirection::Read => state.writers != 0,
                EndpointDirection::Write => state.readers != 0,
            };
            if ready {
                state.open_waiters -= 1;
                drop(state);
                return self.endpoint.take().expect("completed FIFO wait");
            }
            state.sleepers += 1;
            state = endpoint
                .pipe
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.sleepers -= 1;
            drop(state);
        }
    }
}

impl Drop for NamedFifoWait {
    fn drop(&mut self) {
        let Some(endpoint) = self.endpoint.take() else {
            return;
        };
        let mut state = endpoint.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open_waiters -= 1;
        drop(state);
        endpoint.close_endpoint();
    }
}
