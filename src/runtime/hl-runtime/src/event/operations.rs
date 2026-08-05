use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::DescriptionIdentity;
use hl_event::{Inotify, SignalFd, TimerFd};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationError {
    AlreadyRegistered,
    NotFound,
    WrongKind,
}

#[derive(Clone)]
enum Operation {
    Timer(Arc<TimerFd>),
    Signal(Arc<SignalFd>),
    Watch(Arc<Inotify>),
}

#[derive(Default)]
pub struct OperationRegistry {
    operations: Mutex<BTreeMap<DescriptionIdentity, Operation>>,
}

impl OperationRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_timer(&self, identity: DescriptionIdentity, object: Arc<TimerFd>) -> Result<(), OperationError> {
        self.register(identity, Operation::Timer(object))
    }

    pub fn register_signal(&self, identity: DescriptionIdentity, object: Arc<SignalFd>) -> Result<(), OperationError> {
        self.register(identity, Operation::Signal(object))
    }

    pub fn register_watch(&self, identity: DescriptionIdentity, object: Arc<Inotify>) -> Result<(), OperationError> {
        self.register(identity, Operation::Watch(object))
    }

    pub fn retire(&self, identity: DescriptionIdentity) -> bool {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&identity)
            .is_some()
    }

    pub fn timer(&self, identity: DescriptionIdentity) -> Result<Arc<TimerFd>, OperationError> {
        match self.lookup(identity)? {
            Operation::Timer(object) => Ok(object),
            _ => Err(OperationError::WrongKind),
        }
    }

    pub fn signal(&self, identity: DescriptionIdentity) -> Result<Arc<SignalFd>, OperationError> {
        match self.lookup(identity)? {
            Operation::Signal(object) => Ok(object),
            _ => Err(OperationError::WrongKind),
        }
    }

    pub fn watch(&self, identity: DescriptionIdentity) -> Result<Arc<Inotify>, OperationError> {
        match self.lookup(identity)? {
            Operation::Watch(object) => Ok(object),
            _ => Err(OperationError::WrongKind),
        }
    }

    fn register(&self, identity: DescriptionIdentity, operation: Operation) -> Result<(), OperationError> {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if operations.contains_key(&identity) {
            return Err(OperationError::AlreadyRegistered);
        }
        operations.insert(identity, operation);
        Ok(())
    }

    fn lookup(&self, identity: DescriptionIdentity) -> Result<Operation, OperationError> {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity)
            .cloned()
            .ok_or(OperationError::NotFound)
    }
}
