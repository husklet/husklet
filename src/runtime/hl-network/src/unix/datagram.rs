use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hl_sync::WaitQueue;

use super::address::UnixAddress;

const DEFAULT_CAPACITY: usize = 65_536;
const RECORD_MAXIMUM: usize = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatagramRecordSnapshot {
    pub payload: Vec<u8>,
    pub source: UnixAddress,
}
pub type UnixDatagramRecordSnapshot = DatagramRecordSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatagramSnapshot {
    pub capacity: usize,
    pub connected: Option<UnixAddress>,
    pub records: Vec<UnixDatagramRecordSnapshot>,
    pub closed: bool,
}
pub type UnixDatagramSnapshot = DatagramSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatagramError {
    Invalid,
    WouldBlock,
    MessageTooLarge,
    Closed,
}
pub type UnixDatagramError = DatagramError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatagramReceive {
    pub count: usize,
    pub full_length: usize,
    pub source: UnixAddress,
}
pub type UnixDatagramReceive = DatagramReceive;

#[derive(Debug)]
struct State {
    connected: Option<UnixAddress>,
    records: VecDeque<UnixDatagramRecordSnapshot>,
    bytes: usize,
    closed: bool,
}

#[derive(Debug)]
pub struct DatagramSocket {
    capacity: usize,
    state: Mutex<State>,
    readable: Arc<WaitQueue>,
    writable: Arc<WaitQueue>,
}
pub type UnixDatagramSocket = DatagramSocket;

impl DatagramSocket {
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY).expect("nonzero default capacity")
    }

    pub fn with_capacity(capacity: usize) -> Result<Self, UnixDatagramError> {
        if capacity == 0 {
            return Err(UnixDatagramError::Invalid);
        }
        Ok(Self {
            capacity,
            state: Mutex::new(State {
                connected: None,
                records: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            readable: Arc::new(WaitQueue::new()),
            writable: Arc::new(WaitQueue::new()),
        })
    }

    pub fn restore(snapshot: &UnixDatagramSnapshot) -> Result<Self, UnixDatagramError> {
        Self::validate_snapshot(snapshot)?;
        let bytes = snapshot.records.iter().map(|record| record.payload.len()).sum();
        Ok(Self {
            capacity: snapshot.capacity,
            state: Mutex::new(State {
                connected: snapshot.connected.clone(),
                records: snapshot.records.clone().into(),
                bytes,
                closed: snapshot.closed,
            }),
            readable: Arc::new(WaitQueue::new()),
            writable: Arc::new(WaitQueue::new()),
        })
    }

    pub fn connect(&self, peer: UnixAddress) -> Result<(), UnixDatagramError> {
        if peer == UnixAddress::Unnamed {
            return Err(UnixDatagramError::Invalid);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(UnixDatagramError::Closed);
        }
        state.connected = Some(peer);
        Ok(())
    }

    pub fn connected(&self) -> Option<UnixAddress> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .connected
            .clone()
    }

    pub fn enqueue(&self, payload: &[u8], source: UnixAddress) -> Result<usize, UnixDatagramError> {
        if payload.len() > self.capacity {
            return Err(UnixDatagramError::MessageTooLarge);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(UnixDatagramError::Closed);
        }
        if state.records.len() >= RECORD_MAXIMUM || state.bytes.saturating_add(payload.len()) > self.capacity {
            return Err(UnixDatagramError::WouldBlock);
        }
        state.records.push_back(UnixDatagramRecordSnapshot {
            payload: payload.to_vec(),
            source,
        });
        state.bytes += payload.len();
        drop(state);
        self.readable.notify_all();
        Ok(payload.len())
    }

    pub fn receive(&self, output: &mut [u8], peek: bool) -> Result<UnixDatagramReceive, UnixDatagramError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(record) = state.records.front() else {
            return if state.closed {
                Err(UnixDatagramError::Closed)
            } else {
                Err(UnixDatagramError::WouldBlock)
            };
        };
        let count = output.len().min(record.payload.len());
        output[..count].copy_from_slice(&record.payload[..count]);
        let result = UnixDatagramReceive {
            count,
            full_length: record.payload.len(),
            source: record.source.clone(),
        };
        if !peek {
            state.bytes -= record.payload.len();
            state.records.pop_front();
            drop(state);
            self.writable.notify_all();
        }
        Ok(result)
    }

    pub fn close(&self) {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).closed = true;
        self.readable.notify_all();
        self.writable.notify_all();
    }

    #[must_use]
    pub fn readable_wait(&self) -> Arc<WaitQueue> {
        self.readable.clone()
    }

    #[must_use]
    pub fn writable_wait(&self) -> Arc<WaitQueue> {
        self.writable.clone()
    }

    pub fn snapshot(&self) -> UnixDatagramSnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        UnixDatagramSnapshot {
            capacity: self.capacity,
            connected: state.connected.clone(),
            records: state.records.iter().cloned().collect(),
            closed: state.closed,
        }
    }

    fn validate_snapshot(snapshot: &UnixDatagramSnapshot) -> Result<(), UnixDatagramError> {
        let bytes = snapshot.records.iter().try_fold(0_usize, |total, record| {
            total
                .checked_add(record.payload.len())
                .ok_or(UnixDatagramError::Invalid)
        })?;
        if snapshot.capacity == 0
            || snapshot.records.len() > RECORD_MAXIMUM
            || bytes > snapshot.capacity
            || snapshot.connected == Some(UnixAddress::Unnamed)
        {
            return Err(UnixDatagramError::Invalid);
        }
        Ok(())
    }
}

impl Default for DatagramSocket {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_source_and_peek() {
        let socket = DatagramSocket::with_capacity(8).unwrap();
        socket.enqueue(b"abc", UnixAddress::Abstract(b"one".to_vec())).unwrap();
        socket.enqueue(b"de", UnixAddress::Pathname(b"two".to_vec())).unwrap();
        let mut output = [0; 2];
        let peeked = socket.receive(&mut output, true).unwrap();
        assert_eq!((peeked.count, peeked.full_length, &output), (2, 3, b"ab"));
        assert_eq!(
            socket.receive(&mut output, false).unwrap().source,
            UnixAddress::Abstract(b"one".to_vec())
        );
        assert_eq!(
            socket.receive(&mut output, false).unwrap().source,
            UnixAddress::Pathname(b"two".to_vec())
        );
    }

    #[test]
    fn backpressure_is_nonblocking() {
        let socket = DatagramSocket::with_capacity(3).unwrap();
        socket.enqueue(b"abc", UnixAddress::Unnamed).unwrap();
        assert_eq!(
            socket.enqueue(b"d", UnixAddress::Unnamed),
            Err(DatagramError::WouldBlock)
        );
        assert_eq!(
            socket.enqueue(b"abcd", UnixAddress::Unnamed),
            Err(DatagramError::MessageTooLarge)
        );
    }

    #[test]
    fn snapshot_restores_order_and_peer() {
        let socket = DatagramSocket::with_capacity(8).unwrap();
        let peer = UnixAddress::Abstract(b"peer".to_vec());
        socket.connect(peer.clone()).unwrap();
        socket.enqueue(b"a", UnixAddress::Unnamed).unwrap();
        socket.enqueue(b"bc", peer.clone()).unwrap();
        let restored = DatagramSocket::restore(&socket.snapshot()).unwrap();
        assert_eq!(restored.connected(), Some(peer));
        let mut output = [0; 4];
        assert_eq!(restored.receive(&mut output, false).unwrap().full_length, 1);
        assert_eq!(restored.receive(&mut output, false).unwrap().full_length, 2);
    }

    #[test]
    fn malformed_snapshot_rejected() {
        let snapshot = DatagramSnapshot {
            capacity: 1,
            connected: Some(UnixAddress::Unnamed),
            records: vec![DatagramRecordSnapshot {
                payload: b"xx".to_vec(),
                source: UnixAddress::Unnamed,
            }],
            closed: false,
        };
        assert_eq!(DatagramSocket::restore(&snapshot).unwrap_err(), DatagramError::Invalid);
    }
}
