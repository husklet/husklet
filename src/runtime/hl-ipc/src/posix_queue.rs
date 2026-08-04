use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::{Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};
use hl_sync::WaitQueue;

const MQ_PRIORITY_MAX: u32 = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MqLimits {
    pub queues: usize,
    pub messages: usize,
    pub message_bytes: usize,
    pub name_bytes: usize,
}

impl Default for MqLimits {
    fn default() -> Self {
        Self {
            queues: 256,
            messages: 10,
            message_bytes: 8192,
            name_bytes: 255,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MqAccess {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MqOpen {
    pub create: bool,
    pub exclusive: bool,
    pub nonblocking: bool,
    pub access: MqAccess,
    pub maximum_messages: Option<usize>,
    pub message_bytes: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MqAttributes {
    pub nonblocking: bool,
    pub maximum_messages: usize,
    pub message_bytes: usize,
    pub current_messages: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MqEvent {
    None { owner: u32 },
    Signal { owner: u32, signal: u8, value: u64 },
    Thread { owner: u32, cookie: u64 },
}

impl MqEvent {
    const fn owner(self) -> u32 {
        match self {
            Self::None { owner } | Self::Signal { owner, .. } | Self::Thread { owner, .. } => owner,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MqReceipt {
    pub bytes: Vec<u8>,
    pub priority: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MqError {
    InvalidName,
    NameTooLong,
    NotFound,
    Exists,
    Capacity,
    InvalidGeometry,
    BadAccess,
    MessageTooBig,
    Priority,
    Again,
    Busy,
    Fault,
}

struct Message {
    bytes: Vec<u8>,
    priority: u32,
    sequence: u64,
}

struct QueueState {
    messages: Vec<Message>,
    notification: Option<MqEvent>,
    next_sequence: u64,
}

struct Queue {
    maximum_messages: usize,
    message_bytes: usize,
    state: Mutex<QueueState>,
    readiness: ReadinessRegistry,
    changed: WaitQueue,
    live_queues: Arc<Mutex<usize>>,
}

impl Drop for Queue {
    fn drop(&mut self) {
        let mut live = self
            .live_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *live = live.saturating_sub(1);
    }
}

struct DescriptionState {
    nonblocking: bool,
}

/// One POSIX message-queue open file description. Clones share `O_NONBLOCK`.
#[derive(Clone)]
pub struct MqDescription {
    queue: Arc<Queue>,
    access: MqAccess,
    state: Arc<Mutex<DescriptionState>>,
}

impl MqDescription {
    #[must_use]
    pub fn wait_queue(&self) -> &WaitQueue {
        &self.queue.changed
    }
    #[must_use]
    pub fn readiness(&self, interests: Readiness) -> Readiness {
        let state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut bits = 0;
        if !matches!(self.access, MqAccess::Write) && !state.messages.is_empty() {
            bits |= Readiness::READ;
        }
        if !matches!(self.access, MqAccess::Read) && state.messages.len() < self.queue.maximum_messages {
            bits |= Readiness::WRITE;
        }
        Readiness::from_bits(bits & interests.bits())
    }

    pub fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, hl_descriptor::ObjectError> {
        self.queue.readiness.subscribe(observer)
    }
    #[must_use]
    pub fn attributes(&self) -> MqAttributes {
        let state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        MqAttributes {
            nonblocking: self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking,
            maximum_messages: self.queue.maximum_messages,
            message_bytes: self.queue.message_bytes,
            current_messages: state.messages.len(),
        }
    }

    /// Returns attributes from before the flag update, matching `mq_getsetattr`.
    #[must_use]
    pub fn set_nonblocking(&self, value: bool) -> MqAttributes {
        let old = self.attributes();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .nonblocking = value;
        old
    }

    /// # Errors
    ///
    /// Rejects access, priority, size, and full-queue violations.
    pub fn send(&self, bytes: &[u8], priority: u32) -> Result<Option<MqEvent>, MqError> {
        if matches!(self.access, MqAccess::Read) {
            return Err(MqError::BadAccess);
        }
        if priority >= MQ_PRIORITY_MAX {
            return Err(MqError::Priority);
        }
        if bytes.len() > self.queue.message_bytes {
            return Err(MqError::MessageTooBig);
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.messages.len() == self.queue.maximum_messages {
            return Err(MqError::Again);
        }
        let was_empty = state.messages.is_empty();
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        state.messages.push(Message {
            bytes: bytes.to_vec(),
            priority,
            sequence,
        });
        state
            .messages
            .sort_by_key(|message| (std::cmp::Reverse(message.priority), message.sequence));
        let event = was_empty.then(|| state.notification.take()).flatten();
        drop(state);
        self.queue.changed.notify_all();
        self.queue.readiness.notify();
        Ok(event)
    }

    /// # Errors
    ///
    /// Rejects access, undersized buffers, and empty queues.
    pub fn receive(&self, capacity: usize) -> Result<MqReceipt, MqError> {
        self.receive_transactional(capacity, |_| Ok(()))
    }

    /// Publishes a receive copyout before consuming the selected message.
    ///
    /// # Errors
    ///
    /// Leaves the queue unchanged when `publish` rejects the receipt.
    pub fn receive_transactional(
        &self,
        capacity: usize,
        publish: impl FnOnce(&MqReceipt) -> Result<(), MqError>,
    ) -> Result<MqReceipt, MqError> {
        if matches!(self.access, MqAccess::Write) {
            return Err(MqError::BadAccess);
        }
        if capacity < self.queue.message_bytes {
            return Err(MqError::MessageTooBig);
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.messages.is_empty() {
            return Err(MqError::Again);
        }
        let message = &state.messages[0];
        let receipt = MqReceipt {
            bytes: message.bytes.clone(),
            priority: message.priority,
        };
        publish(&receipt)?;
        state.messages.remove(0);
        drop(state);
        self.queue.changed.notify_all();
        self.queue.readiness.notify();
        Ok(receipt)
    }

    /// # Errors
    ///
    /// Returns [`MqError::Busy`] when a notification is already registered.
    pub fn register(&self, event: MqEvent) -> Result<(), MqError> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.notification.is_some() {
            return Err(MqError::Busy);
        }
        state.notification = Some(event);
        Ok(())
    }

    pub fn unregister(&self, owner: u32) {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.notification.is_some_and(|event| event.owner() == owner) {
            state.notification = None;
        }
    }
}

pub struct MqNamespace {
    limits: MqLimits,
    names: Mutex<BTreeMap<Vec<u8>, Arc<Queue>>>,
    live_queues: Arc<Mutex<usize>>,
}

impl MqNamespace {
    #[must_use]
    pub fn new(limits: MqLimits) -> Self {
        Self {
            limits,
            names: Mutex::new(BTreeMap::new()),
            live_queues: Arc::new(Mutex::new(0)),
        }
    }

    /// # Errors
    ///
    /// Rejects invalid names, creation geometry, capacity, and lookup conflicts.
    pub fn open(&self, name: &[u8], request: MqOpen) -> Result<MqDescription, MqError> {
        self.validate_name(name)?;
        let mut names = self.names.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue = if let Some(queue) = names.get(name) {
            if request.create && request.exclusive {
                return Err(MqError::Exists);
            }
            Arc::clone(queue)
        } else {
            if !request.create {
                return Err(MqError::NotFound);
            }
            let mut live = self
                .live_queues
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *live == self.limits.queues {
                return Err(MqError::Capacity);
            }
            let maximum_messages = request.maximum_messages.unwrap_or(self.limits.messages);
            let message_bytes = request.message_bytes.unwrap_or(self.limits.message_bytes);
            if maximum_messages == 0
                || maximum_messages > self.limits.messages
                || message_bytes == 0
                || message_bytes > self.limits.message_bytes
            {
                return Err(MqError::InvalidGeometry);
            }
            let queue = Arc::new(Queue {
                maximum_messages,
                message_bytes,
                state: Mutex::new(QueueState {
                    messages: Vec::new(),
                    notification: None,
                    next_sequence: 0,
                }),
                readiness: ReadinessRegistry::new(),
                changed: WaitQueue::new(),
                live_queues: Arc::clone(&self.live_queues),
            });
            *live += 1;
            names.insert(name.to_vec(), Arc::clone(&queue));
            queue
        };
        Ok(MqDescription {
            queue,
            access: request.access,
            state: Arc::new(Mutex::new(DescriptionState {
                nonblocking: request.nonblocking,
            })),
        })
    }

    /// Reports whether a linked queue currently owns this validated name.
    ///
    /// # Errors
    ///
    /// Rejects names outside the POSIX queue namespace contract.
    pub fn contains(&self, name: &[u8]) -> Result<bool, MqError> {
        self.validate_name(name)?;
        Ok(self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(name))
    }

    /// # Errors
    ///
    /// Rejects invalid or absent names.
    pub fn unlink(&self, name: &[u8]) -> Result<(), MqError> {
        self.validate_name(name)?;
        let removed = self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
        removed.map(drop).ok_or(MqError::NotFound)
    }

    fn validate_name(&self, name: &[u8]) -> Result<(), MqError> {
        if name.is_empty() || name.contains(&b'/') || name.contains(&0) {
            return Err(MqError::InvalidName);
        }
        if name.len() > self.limits.name_bytes {
            return Err(MqError::NameTooLong);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(namespace: &MqNamespace, name: &[u8]) -> MqDescription {
        namespace
            .open(
                name,
                MqOpen {
                    create: true,
                    exclusive: true,
                    nonblocking: false,
                    access: MqAccess::ReadWrite,
                    maximum_messages: Some(3),
                    message_bytes: Some(8),
                },
            )
            .unwrap()
    }

    #[test]
    fn priority_fifo() {
        let namespace = MqNamespace::new(MqLimits::default());
        let queue = create(&namespace, b"priority");
        queue.send(b"old", 4).unwrap();
        queue.send(b"high", 9).unwrap();
        queue.send(b"", 4).unwrap();
        assert_eq!(queue.receive(8).unwrap().bytes, b"high");
        assert_eq!(queue.receive(8).unwrap().bytes, b"old");
        assert_eq!(queue.receive(8).unwrap().bytes, b"");
    }

    #[test]
    fn unlink_lifetime() {
        let namespace = MqNamespace::new(MqLimits::default());
        let old = create(&namespace, b"lifetime");
        namespace.unlink(b"lifetime").unwrap();
        assert!(matches!(
            namespace.open(
                b"lifetime",
                MqOpen {
                    create: false,
                    exclusive: false,
                    nonblocking: false,
                    access: MqAccess::Read,
                    maximum_messages: None,
                    message_bytes: None
                }
            ),
            Err(MqError::NotFound)
        ));
        old.send(b"live", 0).unwrap();
        assert_eq!(old.receive(8).unwrap().bytes, b"live");
        let replacement = create(&namespace, b"lifetime");
        assert_eq!(replacement.attributes().current_messages, 0);
    }

    #[test]
    fn description_notify() {
        let namespace = MqNamespace::new(MqLimits::default());
        let queue = create(&namespace, b"description");
        let duplicate = queue.clone();
        assert!(!duplicate.set_nonblocking(true).nonblocking);
        assert!(queue.attributes().nonblocking);
        let event = MqEvent::Signal {
            owner: 7,
            signal: 12,
            value: 99,
        };
        queue.register(event).unwrap();
        assert_eq!(queue.send(b"a", 0).unwrap(), Some(event));
        assert_eq!(queue.send(b"b", 0).unwrap(), None);
        assert_eq!(queue.receive(8).unwrap().bytes, b"a");
        assert_eq!(queue.receive(8).unwrap().bytes, b"b");
        assert_eq!(queue.send(b"c", 0).unwrap(), None);
    }

    #[test]
    fn receive_copyout_is_transactional_and_readiness_is_shared() {
        let namespace = MqNamespace::new(MqLimits::default());
        let queue = create(&namespace, b"transaction");
        let duplicate = queue.clone();
        let interests = Readiness::from_bits(Readiness::READ | Readiness::WRITE);
        assert_eq!(queue.readiness(interests).bits(), Readiness::WRITE);
        queue.send(b"kept", 3).unwrap();
        assert!(duplicate.readiness(interests).contains(Readiness::READ));
        assert_eq!(
            duplicate.receive_transactional(8, |_| Err(MqError::Fault)),
            Err(MqError::Fault)
        );
        assert_eq!(queue.attributes().current_messages, 1);
        assert_eq!(queue.receive(8).unwrap().bytes, b"kept");
        assert!(!duplicate.set_nonblocking(true).nonblocking);
        assert!(queue.attributes().nonblocking);
    }

    #[test]
    fn bounded_access() {
        let namespace = MqNamespace::new(MqLimits {
            queues: 1,
            messages: 1,
            message_bytes: 4,
            name_bytes: 5,
        });
        let reader = namespace
            .open(
                b"queue",
                MqOpen {
                    create: true,
                    exclusive: false,
                    nonblocking: true,
                    access: MqAccess::Read,
                    maximum_messages: None,
                    message_bytes: None,
                },
            )
            .unwrap();
        assert_eq!(reader.send(b"x", 0), Err(MqError::BadAccess));
        assert_eq!(reader.receive(3), Err(MqError::MessageTooBig));
        assert_eq!(
            namespace
                .open(
                    b"other",
                    MqOpen {
                        create: true,
                        exclusive: false,
                        nonblocking: false,
                        access: MqAccess::ReadWrite,
                        maximum_messages: None,
                        message_bytes: None
                    }
                )
                .err(),
            Some(MqError::Capacity)
        );
        assert_eq!(namespace.unlink(b"longer"), Err(MqError::NameTooLong));
    }

    #[test]
    fn live_capacity() {
        let namespace = MqNamespace::new(MqLimits {
            queues: 1,
            messages: 3,
            message_bytes: 8,
            name_bytes: 8,
        });
        let old = create(&namespace, b"old");
        namespace.unlink(b"old").unwrap();
        assert_eq!(
            namespace
                .open(
                    b"new",
                    MqOpen {
                        create: true,
                        exclusive: false,
                        nonblocking: false,
                        access: MqAccess::ReadWrite,
                        maximum_messages: None,
                        message_bytes: None,
                    },
                )
                .err(),
            Some(MqError::Capacity),
        );
        drop(old);
        assert!(
            namespace
                .open(
                    b"new",
                    MqOpen {
                        create: true,
                        exclusive: false,
                        nonblocking: false,
                        access: MqAccess::ReadWrite,
                        maximum_messages: None,
                        message_bytes: None,
                    },
                )
                .is_ok()
        );
    }
}
