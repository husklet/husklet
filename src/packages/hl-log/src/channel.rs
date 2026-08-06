//! Per-instance bounded transport for typed diagnostic records.
//!
//! This transport is deliberately independent from the process-global formatted
//! log sinks. A composition root opts in by retaining `Some(Channel<T>)`; a
//! disabled producer retains `None` and guards event construction with
//! `if let Some(channel) = &channel`, so the disabled path performs no queue,
//! formatting, or allocation work.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
};

/// Outcome of a nonblocking publication attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publish {
    Published,
    /// The fixed-capacity queue was full. The newest record was discarded.
    Dropped,
    /// The receiver, or the complete channel, was explicitly closed.
    Closed,
}

/// Outcome when no record is immediately available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveError {
    Empty,
    /// No record remains and no publisher can publish another one.
    Closed,
}

struct State<T> {
    queue: VecDeque<T>,
    publishers: usize,
    publish_open: bool,
    receive_open: bool,
}

struct Transport<T> {
    capacity: usize,
    state: Mutex<State<T>>,
    available: Condvar,
    lost: AtomicU64,
}

impl<T> Transport<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Cloneable publishing capability for one typed event stream.
///
/// Publication never waits for capacity: when the queue is full it drops the
/// newest record and increments [`Self::lost`]. The queue allocates its storage
/// at construction and never grows beyond its declared capacity.
pub struct Channel<T> {
    shared: Arc<Transport<T>>,
}

impl<T> Channel<T> {
    /// Creates an isolated channel and its single receiving capability.
    ///
    /// A zero capacity cannot retain a record and is therefore rejected.
    #[must_use]
    pub fn bounded(capacity: usize) -> Option<(Self, Receiver<T>)> {
        if capacity == 0 {
            return None;
        }
        let shared = Arc::new(Transport {
            capacity,
            state: Mutex::new(State {
                queue: VecDeque::with_capacity(capacity),
                publishers: 1,
                publish_open: true,
                receive_open: true,
            }),
            available: Condvar::new(),
            lost: AtomicU64::new(0),
        });
        Some((
            Self {
                shared: Arc::clone(&shared),
            },
            Receiver { shared },
        ))
    }

    /// Attempts to publish without waiting for queue capacity.
    pub fn try_publish(&self, event: T) -> Publish {
        let mut state = self.shared.lock();
        if !state.publish_open || !state.receive_open {
            return Publish::Closed;
        }
        if state.queue.len() == self.shared.capacity {
            self.shared.lost.fetch_add(1, Ordering::Relaxed);
            return Publish::Dropped;
        }
        state.queue.push_back(event);
        drop(state);
        self.shared.available.notify_one();
        Publish::Published
    }

    /// Number of records discarded because the queue was full.
    #[must_use]
    pub fn lost(&self) -> u64 {
        self.shared.lost.load(Ordering::Relaxed)
    }

    /// Closes publication for this entire stream. Retained records remain
    /// available to the receiver; later attempts return [`Publish::Closed`].
    pub fn close(&self) {
        self.shared.lock().publish_open = false;
        self.shared.available.notify_all();
    }
}

impl<T> Clone for Channel<T> {
    fn clone(&self) -> Self {
        self.shared.lock().publishers += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.publishers -= 1;
        let last = state.publishers == 0;
        drop(state);
        if last {
            self.shared.available.notify_all();
        }
    }
}

/// The unique receiving capability for a [`Channel`].
pub struct Receiver<T> {
    shared: Arc<Transport<T>>,
}

impl<T> Receiver<T> {
    /// Removes one retained record without waiting.
    pub fn try_receive(&self) -> Result<T, ReceiveError> {
        let mut state = self.shared.lock();
        if let Some(event) = state.queue.pop_front() {
            return Ok(event);
        }
        if !state.receive_open || !state.publish_open || state.publishers == 0 {
            Err(ReceiveError::Closed)
        } else {
            Err(ReceiveError::Empty)
        }
    }

    /// Waits until a record arrives or the stream can no longer produce one.
    pub fn receive(&self) -> Result<T, ReceiveError> {
        let mut state = self.shared.lock();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Ok(event);
            }
            if !state.receive_open || !state.publish_open || state.publishers == 0 {
                return Err(ReceiveError::Closed);
            }
            state = self
                .shared
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Cancels the stream and discards retained records. Waiting receivers wake,
    /// and every later publication observes closure.
    pub fn close(&self) {
        let mut state = self.shared.lock();
        state.receive_open = false;
        state.queue.clear();
        drop(state);
        self.shared.available.notify_all();
    }

    /// Number of records discarded because the queue was full.
    #[must_use]
    pub fn lost(&self) -> u64 {
        self.shared.lost.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, Publish, ReceiveError};
    use std::sync::{Arc, Barrier};

    #[test]
    fn ordering_bound() {
        let (channel, receiver) = Channel::bounded(2).unwrap();
        assert_eq!(channel.try_publish(1), Publish::Published);
        assert_eq!(channel.try_publish(2), Publish::Published);
        assert_eq!(channel.try_publish(3), Publish::Dropped);
        assert_eq!(channel.lost(), 1);
        assert_eq!(receiver.try_receive(), Ok(1));
        assert_eq!(receiver.try_receive(), Ok(2));
        assert_eq!(receiver.try_receive(), Err(ReceiveError::Empty));
    }

    #[test]
    fn receiver_close() {
        let (channel, receiver) = Channel::bounded(2).unwrap();
        assert_eq!(channel.try_publish(1), Publish::Published);
        receiver.close();
        assert_eq!(channel.try_publish(2), Publish::Closed);
        assert_eq!(receiver.try_receive(), Err(ReceiveError::Closed));
        assert_eq!(channel.lost(), 0);
    }

    #[test]
    fn channel_close() {
        let (channel, receiver) = Channel::bounded(2).unwrap();
        assert_eq!(channel.try_publish(7), Publish::Published);
        channel.close();
        assert_eq!(receiver.receive(), Ok(7));
        assert_eq!(receiver.receive(), Err(ReceiveError::Closed));
    }

    #[test]
    fn publisher_drop() {
        let (channel, receiver) = Channel::<u8>::bounded(1).unwrap();
        let waiter = std::thread::spawn(move || receiver.receive());
        drop(channel);
        assert_eq!(waiter.join().unwrap(), Err(ReceiveError::Closed));
    }

    #[test]
    fn concurrent_publish() {
        const WORKERS: usize = 8;
        const RECORDS: usize = 128;
        let (channel, receiver) = Channel::bounded(WORKERS * RECORDS).unwrap();
        let barrier = Arc::new(Barrier::new(WORKERS));
        let workers = (0..WORKERS)
            .map(|worker| {
                let publisher = channel.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for record in 0..RECORDS {
                        assert_eq!(publisher.try_publish((worker, record)), Publish::Published);
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        channel.close();
        let mut seen = vec![false; WORKERS * RECORDS];
        while let Ok((worker, record)) = receiver.receive() {
            seen[worker * RECORDS + record] = true;
        }
        assert!(seen.into_iter().all(|record| record));
        assert_eq!(receiver.lost(), 0);
    }

    #[test]
    fn channel_isolation() {
        let (first, first_receiver) = Channel::bounded(1).unwrap();
        let (second, second_receiver) = Channel::bounded(1).unwrap();
        assert_eq!(first.try_publish(11_u64), Publish::Published);
        assert_eq!(second.try_publish(22_u64), Publish::Published);
        assert_eq!(first_receiver.try_receive(), Ok(11));
        assert_eq!(second_receiver.try_receive(), Ok(22));
    }

    #[test]
    fn zero_capacity() {
        assert!(Channel::<u8>::bounded(0).is_none());
    }
}
