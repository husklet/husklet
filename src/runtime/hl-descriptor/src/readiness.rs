use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::ObjectError;

const SUBSCRIPTION_LIMIT: usize = 64;

/// Stable identity of one open-file-description lifetime.
///
/// This identity names a readiness source, not a descriptor-table slot.
/// Aliased descriptor numbers share this value; a newly opened description
/// never inherits the identity of a closed description.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DescriptionIdentity {
    pub identity: u64,
    pub generation: u32,
}

/// Callback used by pollable open-file-descriptions.
pub trait ReadinessObserver: Send + Sync {
    fn readiness_changed(&self);
}

/// Ownership handle for one readiness callback registration.
pub trait ReadinessSubscription: Send + Sync {
    /// Stops future callbacks and waits until callbacks already admitted finish.
    fn quiesce(&self);
}

struct EntryState {
    active: bool,
    callbacks_in_flight: usize,
}

struct Entry {
    observer: Arc<dyn ReadinessObserver>,
    state: Mutex<EntryState>,
    quiescent: Condvar,
}

thread_local! {
    static ACTIVE_CALLBACKS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

struct CallbackScope {
    entry: usize,
}

impl CallbackScope {
    fn enter(entry: &Entry) -> Self {
        let entry = std::ptr::from_ref(entry) as usize;
        ACTIVE_CALLBACKS.with(|callbacks| callbacks.borrow_mut().push(entry));
        Self { entry }
    }

    fn depth(entry: &Entry) -> usize {
        let entry = std::ptr::from_ref(entry) as usize;
        ACTIVE_CALLBACKS.with(|callbacks| callbacks.borrow().iter().filter(|active| **active == entry).count())
    }
}

impl Drop for CallbackScope {
    fn drop(&mut self) {
        ACTIVE_CALLBACKS.with(|callbacks| {
            let active = callbacks.borrow_mut().pop();
            debug_assert_eq!(active, Some(self.entry));
        });
    }
}

struct CallbackAdmission<'a> {
    entry: &'a Entry,
    _scope: CallbackScope,
}

impl Drop for CallbackAdmission<'_> {
    fn drop(&mut self) {
        let mut state = self.entry.state.lock().unwrap_or_else(|error| error.into_inner());
        state.callbacks_in_flight -= 1;
        self.entry.quiescent.notify_all();
    }
}

impl Entry {
    fn notify(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if !state.active {
                return;
            }
            state.callbacks_in_flight += 1;
        }
        let _admission = CallbackAdmission {
            entry: self,
            _scope: CallbackScope::enter(self),
        };
        self.observer.readiness_changed();
    }

    fn quiesce(&self) {
        let local_callbacks = CallbackScope::depth(self);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.active = false;
        while state.callbacks_in_flight > local_callbacks {
            state = self.quiescent.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }
}

struct RegistryState {
    next_token: u64,
    closed: bool,
    entries: BTreeMap<u64, Arc<Entry>>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
}

/// Bounded, quiescent callback registry for pollable descriptions.
#[derive(Clone)]
pub struct ReadinessRegistry {
    inner: Arc<RegistryInner>,
}

impl std::fmt::Debug for ReadinessRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        formatter
            .debug_struct("ReadinessRegistry")
            .field("subscriptions", &state.entries.len())
            .field("closed", &state.closed)
            .finish()
    }
}

impl ReadinessRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState {
                    next_token: 1,
                    closed: false,
                    entries: BTreeMap::new(),
                }),
            }),
        }
    }

    pub fn subscribe(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(ObjectError::Retired);
        }
        if state.entries.len() == SUBSCRIPTION_LIMIT {
            return Err(ObjectError::ResourceLimit);
        }
        let token = state.next_token;
        state.next_token = state.next_token.wrapping_add(1).max(1);
        let entry = Arc::new(Entry {
            observer,
            state: Mutex::new(EntryState {
                active: true,
                callbacks_in_flight: 0,
            }),
            quiescent: Condvar::new(),
        });
        state.entries.insert(token, entry.clone());
        Ok(Box::new(Registration {
            registry: Arc::downgrade(&self.inner),
            token,
            entry: Arc::downgrade(&entry),
        }))
    }

    /// Invokes a snapshot of observers without holding the registry lock.
    pub fn notify(&self) {
        let entries = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for entry in entries {
            entry.notify();
        }
    }

    /// Rejects new registrations and synchronously quiesces existing callbacks.
    pub fn close(&self) {
        let entries = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.closed {
                return;
            }
            state.closed = true;
            std::mem::take(&mut state.entries)
        };
        for entry in entries.into_values() {
            entry.quiesce();
        }
    }
}

impl Default for ReadinessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

struct Registration {
    registry: Weak<RegistryInner>,
    token: u64,
    entry: Weak<Entry>,
}

impl ReadinessSubscription for Registration {
    fn quiesce(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entries
                .remove(&self.token);
        }
        if let Some(entry) = self.entry.upgrade() {
            entry.quiesce();
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.quiesce();
    }
}
