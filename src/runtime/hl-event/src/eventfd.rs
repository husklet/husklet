use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_descriptor::{ObjectError, ReadinessRegistry};

mod ofd;
mod operation;

const COUNTER_MAX: u64 = u64::MAX - 1;
const SUBSCRIPTION_LIMIT: usize = 64;
const EVENTFD_MODE: u32 = 0o100_600;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct EventFdFlags(u32);

impl EventFdFlags {
    pub const SEMAPHORE: u32 = 1;
    pub const NONBLOCKING: u32 = 1 << 1;
    const ALLOWED: u32 = Self::SEMAPHORE | Self::NONBLOCKING;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    const fn is_valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    const fn semaphore(self) -> bool {
        self.0 & Self::SEMAPHORE != 0
    }

    const fn nonblocking(self) -> bool {
        self.0 & Self::NONBLOCKING != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventFdError {
    InvalidArgument,
    WouldBlock,
    ResourceLimit,
    Interrupted,
    Retired,
}

impl EventFdError {
    const fn object_error(self) -> ObjectError {
        match self {
            Self::InvalidArgument => ObjectError::InvalidArgument,
            Self::WouldBlock => ObjectError::WouldBlock,
            Self::ResourceLimit => ObjectError::ResourceLimit,
            Self::Interrupted => ObjectError::Interrupted,
            Self::Retired => ObjectError::Retired,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct EventInterest(u32);

impl EventInterest {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const PRIORITY: u32 = 1 << 2;
    pub const ERROR: u32 = 1 << 3;
    pub const HANGUP: u32 = 1 << 4;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

pub type EventFdStatus = crate::EventStatus;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventFdSnapshot {
    pub counter: u64,
    pub semaphore: bool,
    pub nonblocking: bool,
}

type Observer = dyn Fn(u64) + Send + Sync + 'static;

struct SubscriptionState {
    active: bool,
    callbacks_in_flight: usize,
}

struct Subscription {
    token: u64,
    observer: Arc<Observer>,
    state: Mutex<SubscriptionState>,
    quiescent: Condvar,
}

impl Subscription {
    fn notify(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.active {
                return;
            }
            state.callbacks_in_flight += 1;
        }
        (self.observer)(self.token);
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.callbacks_in_flight -= 1;
        if state.callbacks_in_flight == 0 {
            self.quiescent.notify_all();
        }
    }

    fn quiesce(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = false;
        while state.callbacks_in_flight != 0 {
            state = self
                .quiescent
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

struct EventFdState {
    counter: u64,
    semaphore: bool,
    nonblocking: bool,
    retired: bool,
    next_subscription: u64,
    subscriptions: BTreeMap<u64, Arc<Subscription>>,
}

struct EventFdInner {
    state: Mutex<EventFdState>,
    changed: Condvar,
    readiness: ReadinessRegistry,
}

/// Linux eventfd counter state shared by every descriptor alias.
#[derive(Clone)]
pub struct EventFd {
    inner: Arc<EventFdInner>,
}

impl std::fmt::Debug for EventFd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventFd")
            .field("counter", &self.counter())
            .field("retired", &self.is_retired())
            .finish_non_exhaustive()
    }
}

impl EventFd {
    pub fn from_snapshot(snapshot: EventFdSnapshot) -> Result<Self, EventFdError> {
        let mut flags = 0;
        if snapshot.semaphore {
            flags |= EventFdFlags::SEMAPHORE;
        }
        if snapshot.nonblocking {
            flags |= EventFdFlags::NONBLOCKING;
        }
        Self::new(snapshot.counter, EventFdFlags::from_bits(flags))
    }

    pub fn new(initial: u64, flags: EventFdFlags) -> Result<Self, EventFdError> {
        if initial == u64::MAX || !flags.is_valid() {
            return Err(EventFdError::InvalidArgument);
        }
        Ok(Self {
            inner: Arc::new(EventFdInner {
                state: Mutex::new(EventFdState {
                    counter: initial,
                    semaphore: flags.semaphore(),
                    nonblocking: flags.nonblocking(),
                    retired: false,
                    next_subscription: 1,
                    subscriptions: BTreeMap::new(),
                }),
                changed: Condvar::new(),
                readiness: ReadinessRegistry::new(),
            }),
        })
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), EventFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(EventFdError::Retired);
        }
        state.nonblocking = nonblocking;
        self.inner.changed.notify_all();
        Ok(())
    }

    #[must_use]
    pub fn readiness(&self, interests: EventInterest) -> EventInterest {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ready = 0;
        if state.retired {
            ready |= EventInterest::ERROR;
        } else {
            if state.counter != 0 {
                ready |= EventInterest::READ;
            }
            if state.counter < COUNTER_MAX {
                ready |= EventInterest::WRITE;
            }
        }
        EventInterest::from_bits(ready & (interests.bits() | EventInterest::ERROR | EventInterest::HANGUP))
    }

    #[must_use]
    pub const fn status(&self) -> EventFdStatus {
        EventFdStatus {
            mode: EVENTFD_MODE,
            size: 0,
            link_count: 1,
        }
    }

    pub fn subscribe(&self, token: u64, observer: Arc<Observer>) -> Result<EventSubscription, EventFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(EventFdError::Retired);
        }
        if state.subscriptions.len() == SUBSCRIPTION_LIMIT {
            return Err(EventFdError::ResourceLimit);
        }
        let identity = state.next_subscription;
        state.next_subscription = state.next_subscription.wrapping_add(1).max(1);
        state.subscriptions.insert(
            identity,
            Arc::new(Subscription {
                token,
                observer,
                state: Mutex::new(SubscriptionState {
                    active: true,
                    callbacks_in_flight: 0,
                }),
                quiescent: Condvar::new(),
            }),
        );
        Ok(EventSubscription {
            eventfd: Arc::downgrade(&self.inner),
            identity,
        })
    }

    #[must_use]
    pub fn counter(&self) -> u64 {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counter
    }

    #[must_use]
    pub fn snapshot(&self) -> EventFdSnapshot {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        EventFdSnapshot {
            counter: state.counter,
            semaphore: state.semaphore,
            nonblocking: state.nonblocking,
        }
    }

    #[must_use]
    pub fn is_retired(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired
    }

    fn retire_inner(&self) {
        let subscriptions = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return;
            }
            state.retired = true;
            self.inner.changed.notify_all();
            std::mem::take(&mut state.subscriptions)
        };
        for subscription in subscriptions.into_values() {
            subscription.quiesce();
        }
        self.inner.readiness.notify();
        self.inner.readiness.close();
    }

    fn notify(subscriptions: Vec<Arc<Subscription>>) {
        for subscription in subscriptions {
            subscription.notify();
        }
    }
}

/// An eventfd readiness registration. Drop synchronously quiesces its callback.
pub struct EventSubscription {
    eventfd: Weak<EventFdInner>,
    identity: u64,
}

impl std::fmt::Debug for EventSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventSubscription")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        let Some(eventfd) = self.eventfd.upgrade() else {
            return;
        };
        let subscription = eventfd
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .subscriptions
            .remove(&self.identity);
        if let Some(subscription) = subscription {
            subscription.quiesce();
        }
    }
}
