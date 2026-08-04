//! Behavior-free in-memory transport shared by provider tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::protocol::{HEADER_SIZE, Header};
use crate::{FrameKind, ProviderTransport, TransportError};

struct Pipe {
    state: Mutex<PipeState>,
    changed: Condvar,
}

struct PipeState {
    bytes: VecDeque<u8>,
    closed: bool,
}

pub(crate) struct Endpoint {
    incoming: Arc<Pipe>,
    outgoing: Arc<Pipe>,
    chunk: usize,
    read_interrupts: AtomicUsize,
    write_blocks: AtomicUsize,
    write_failures: AtomicUsize,
}

impl Endpoint {
    pub(crate) fn pair(chunk: usize) -> (Self, Self) {
        let left = Arc::new(Pipe {
            state: Mutex::new(PipeState {
                bytes: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let right = Arc::new(Pipe {
            state: Mutex::new(PipeState {
                bytes: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
        });
        (
            Self::new(Arc::clone(&left), Arc::clone(&right), chunk),
            Self::new(right, left, chunk),
        )
    }

    fn new(incoming: Arc<Pipe>, outgoing: Arc<Pipe>, chunk: usize) -> Self {
        Self {
            incoming,
            outgoing,
            chunk: chunk.max(1),
            read_interrupts: AtomicUsize::new(0),
            write_blocks: AtomicUsize::new(0),
            write_failures: AtomicUsize::new(0),
        }
    }

    pub(crate) fn interrupt_reads(&self, count: usize) {
        self.read_interrupts.store(count, Ordering::Release);
    }

    pub(crate) fn block_writes(&self, count: usize) {
        self.write_blocks.store(count, Ordering::Release);
    }

    pub(crate) fn fail_writes(&self, count: usize) {
        self.write_failures.store(count, Ordering::Release);
    }

    fn consume(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| value.checked_sub(1))
            .is_ok()
    }

    pub(crate) fn send_frame(&self, kind: FrameKind, request: u64, payload: &[u8]) {
        let header = Header::encode(kind, payload.len(), request).unwrap();
        self.write_all(&header);
        self.write_all(payload);
    }

    pub(crate) fn receive_frame(&self) -> (FrameKind, u64, Vec<u8>) {
        let mut header = [0_u8; HEADER_SIZE];
        self.read_all(&mut header);
        let header = Header::decode(&header, 4096).unwrap();
        let mut payload = vec![0_u8; header.size];
        self.read_all(&mut payload);
        (header.kind, header.request, payload)
    }

    pub(crate) fn write_all(&self, bytes: &[u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            match self.write(&bytes[offset..]) {
                Ok(count) => offset += count,
                Err(TransportError::WouldBlock | TransportError::Interrupted) => {}
                result => panic!("test write failed: {result:?}"),
            }
        }
    }

    fn read_all(&self, bytes: &mut [u8]) {
        let mut offset = 0;
        while offset < bytes.len() {
            match self.read(&mut bytes[offset..]) {
                Ok(count) => offset += count,
                Err(TransportError::WouldBlock | TransportError::Interrupted) => {}
                result => panic!("test read failed: {result:?}"),
            }
        }
    }
}

impl ProviderTransport for Endpoint {
    fn read(&self, output: &mut [u8]) -> Result<usize, TransportError> {
        if Self::consume(&self.read_interrupts) {
            return Err(TransportError::Interrupted);
        }
        let mut state = self
            .incoming
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.bytes.is_empty() && !state.closed {
            state = self
                .incoming
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if state.bytes.is_empty() {
            return Ok(0);
        }
        let count = output.len().min(self.chunk).min(state.bytes.len());
        for byte in &mut output[..count] {
            *byte = state.bytes.pop_front().unwrap();
        }
        Ok(count)
    }

    fn write(&self, input: &[u8]) -> Result<usize, TransportError> {
        if Self::consume(&self.write_failures) {
            return Err(TransportError::Failed);
        }
        if Self::consume(&self.write_blocks) {
            return Err(TransportError::WouldBlock);
        }
        let mut state = self
            .outgoing
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closed {
            return Err(TransportError::Closed);
        }
        let count = input.len().min(self.chunk);
        state.bytes.extend(&input[..count]);
        self.outgoing.changed.notify_all();
        Ok(count)
    }

    fn wait_readable(&self) -> Result<(), TransportError> {
        Ok(())
    }

    fn wait_writable(&self) -> Result<(), TransportError> {
        Ok(())
    }

    fn shutdown(&self) {
        for pipe in [&self.incoming, &self.outgoing] {
            let mut state = pipe.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closed = true;
            pipe.changed.notify_all();
        }
    }
}
