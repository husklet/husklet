use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_descriptor::{Readiness, ReadinessRegistry};

pub(crate) mod model;
mod ofd;
mod queue;

use self::model::{
    INOTIFY_HEADER_SIZE, InotifyError, InotifyEventSnapshot, InotifyLimits, InotifyMask, InotifySnapshot,
    InotifyStatus, InotifyWatchSnapshot, WatchBinding, WatchRequest, WatchSourceError, WatchSourceEvent,
};

pub(crate) mod prepared;
mod retire;
mod snapshot;
pub use prepared::PreparedInotifyRead;

const INOTIFY_MODE: u32 = 0o100_600;

pub trait WatchSourceObserver: Send + Sync {
    fn watch_event(&self, event: WatchSourceEvent);
}

pub trait WatchSourceSubscription: Send + Sync {
    fn quiesce(&self);
}

/// VFS-owned path resolution and observation port consumed by inotify.
///
/// Implementations invoke observers without source locks held. `add` publishes
/// no callback before it returns successfully.
pub trait WatchSource: std::fmt::Debug + Send + Sync {
    fn resolve(&self, request: WatchRequest<'_>) -> Result<WatchBinding, WatchSourceError>;

    fn add(&self, binding: WatchBinding, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError>;

    fn modify(&self, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError>;

    fn remove(&self, token: u64) -> Result<(), WatchSourceError>;

    fn subscribe(
        &self,
        observer: Arc<dyn WatchSourceObserver>,
    ) -> Result<Box<dyn WatchSourceSubscription>, WatchSourceError>;

    fn checkpoint_clone(&self) -> Result<Arc<dyn WatchSource>, WatchSourceError> {
        Err(WatchSourceError::NotSupported)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Watch {
    pub(crate) binding: WatchBinding,
    pub(crate) mask: InotifyMask,
    pub(crate) token: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct WatchSlot {
    pub(crate) generation: u32,
    pub(crate) watch: Option<Watch>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QueuedEvent {
    pub(crate) watch_descriptor: i32,
    pub(crate) mask: InotifyMask,
    pub(crate) cookie: u32,
    pub(crate) name: Vec<u8>,
}

impl QueuedEvent {
    pub(crate) fn encoded_len(&self) -> usize {
        INOTIFY_HEADER_SIZE + Self::padded_name_len(self.name.len())
    }

    fn padded_name_len(name_len: usize) -> usize {
        if name_len == 0 { 0 } else { (name_len + 4) & !3 }
    }

    fn encode(&self, output: &mut [u8]) {
        output[0..4].copy_from_slice(&self.watch_descriptor.to_le_bytes());
        output[4..8].copy_from_slice(&self.mask.bits().to_le_bytes());
        output[8..12].copy_from_slice(&self.cookie.to_le_bytes());
        let padded = Self::padded_name_len(self.name.len());
        output[12..16].copy_from_slice(&u32::try_from(padded).unwrap().to_le_bytes());
        if padded != 0 {
            output[16..16 + self.name.len()].copy_from_slice(&self.name);
            output[16 + self.name.len()..16 + padded].fill(0);
        }
    }
}

pub(crate) struct InotifyState {
    pub(crate) slots: Vec<WatchSlot>,
    pub(crate) queue: VecDeque<QueuedEvent>,
    pub(crate) queue_bytes: usize,
    pub(crate) overflow_queued: bool,
    nonblocking: bool,
    pub(crate) retired: bool,
    next_cookie: u32,
    source_subscription: Option<Box<dyn WatchSourceSubscription>>,
}

pub(crate) struct InotifyInner {
    pub(crate) source: Arc<dyn WatchSource>,
    pub(crate) limits: InotifyLimits,
    mutation: Mutex<()>,
    pub(crate) state: Mutex<InotifyState>,
    pub(crate) changed: Condvar,
    pub(crate) readiness: ReadinessRegistry,
}

struct SourceObserver {
    inner: Weak<InotifyInner>,
}

impl WatchSourceObserver for SourceObserver {
    fn watch_event(&self, event: WatchSourceEvent) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        Inotify::accept_source_event(&inner, event);
    }
}

#[derive(Clone)]
pub struct Inotify {
    pub(crate) inner: Arc<InotifyInner>,
}

impl std::fmt::Debug for Inotify {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Inotify")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Inotify {
    pub fn new(nonblocking: bool, limits: InotifyLimits, source: Arc<dyn WatchSource>) -> Result<Self, InotifyError> {
        if limits.watches == 0
            || limits.queued_events < 2
            || limits.queued_bytes < INOTIFY_HEADER_SIZE * 2
            || limits.name_bytes == 0
        {
            return Err(InotifyError::InvalidArgument);
        }
        let inner = Arc::new(InotifyInner {
            source: source.clone(),
            limits,
            mutation: Mutex::new(()),
            state: Mutex::new(InotifyState {
                slots: Vec::new(),
                queue: VecDeque::new(),
                queue_bytes: 0,
                overflow_queued: false,
                nonblocking,
                retired: false,
                next_cookie: 1,
                source_subscription: None,
            }),
            changed: Condvar::new(),
            readiness: ReadinessRegistry::new(),
        });
        let observer = Arc::new(SourceObserver {
            inner: Arc::downgrade(&inner),
        });
        let subscription = source.subscribe(observer)?;
        inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .source_subscription = Some(subscription);
        Ok(Self { inner })
    }

    pub fn add_watch(&self, path: &[u8], mask: InotifyMask) -> Result<i32, InotifyError> {
        if path.is_empty() || !mask.valid_watch() {
            return Err(InotifyError::InvalidArgument);
        }
        let _mutation = self.inner.mutation.lock().unwrap_or_else(|error| error.into_inner());
        let binding = self.inner.source.resolve(WatchRequest { path, mask })?;
        let existing = {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::ensure_active(&state)?;
            state.slots.iter().position(|slot| {
                slot.watch
                    .as_ref()
                    .is_some_and(|watch| watch.binding.node == binding.node)
            })
        };
        if let Some(index) = existing {
            return self.modify_existing(index, mask);
        }
        self.install_new(binding, mask)
    }

    pub fn remove_watch(&self, watch_descriptor: i32) -> Result<(), InotifyError> {
        let _mutation = self.inner.mutation.lock().unwrap_or_else(|error| error.into_inner());
        let (index, token) = {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::ensure_active(&state)?;
            let index = Self::watch_index(watch_descriptor, state.slots.len())?;
            let watch = state.slots[index].watch.as_ref().ok_or(InotifyError::InvalidArgument)?;
            (index, watch.token)
        };
        match self.inner.source.remove(token) {
            Ok(()) | Err(WatchSourceError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
        let notify = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.slots[index]
                .watch
                .as_ref()
                .is_none_or(|watch| watch.token != token)
            {
                return Err(InotifyError::InvalidArgument);
            }
            state.slots[index].watch = None;
            Self::queue_event(
                &self.inner,
                &mut state,
                QueuedEvent {
                    watch_descriptor,
                    mask: InotifyMask::from_bits(InotifyMask::IGNORED),
                    cookie: 0,
                    name: Vec::new(),
                },
            )
        };
        self.notify_if(notify);
        Ok(())
    }

    pub fn read(&self, output: &mut [u8]) -> Result<usize, InotifyError> {
        if output.len() < INOTIFY_HEADER_SIZE {
            return Err(InotifyError::InvalidArgument);
        }
        let prepared = self.prepare_read(output.len())?;
        output[..prepared.bytes().len()].copy_from_slice(prepared.bytes());
        self.commit_read(&prepared)?;
        Ok(prepared.bytes().len())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), InotifyError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::ensure_active(&state)?;
        state.nonblocking = nonblocking;
        self.inner.changed.notify_all();
        Ok(())
    }

    #[must_use]
    pub fn readiness(&self, interests: Readiness) -> Readiness {
        let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        let ready = if state.retired {
            Readiness::ERROR
        } else if !state.queue.is_empty() {
            Readiness::READ
        } else {
            0
        };
        Readiness::from_bits(ready & (interests.bits() | Readiness::ERROR | Readiness::HANGUP))
    }

    pub fn next_rename_cookie(&self) -> Result<u32, InotifyError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::ensure_active(&state)?;
        let cookie = state.next_cookie;
        state.next_cookie = state.next_cookie.wrapping_add(1).max(1);
        Ok(cookie)
    }

    #[must_use]
    pub fn snapshot(&self) -> InotifySnapshot {
        let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        InotifySnapshot {
            limits: self.inner.limits,
            nonblocking: state.nonblocking,
            next_cookie: state.next_cookie,
            overflow_queued: state.overflow_queued,
            watch_generations: state.slots.iter().map(|slot| slot.generation).collect(),
            watches: state
                .slots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    slot.watch.as_ref().map(|watch| InotifyWatchSnapshot {
                        watch_descriptor: i32::try_from(index + 1).unwrap(),
                        generation: slot.generation,
                        binding: watch.binding,
                        mask: watch.mask,
                    })
                })
                .collect(),
            queue: state
                .queue
                .iter()
                .map(|event| InotifyEventSnapshot {
                    watch_descriptor: event.watch_descriptor,
                    mask: event.mask,
                    cookie: event.cookie,
                    name: event.name.clone(),
                })
                .collect(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> InotifyStatus {
        InotifyStatus {
            mode: INOTIFY_MODE,
            size: 0,
            link_count: 1,
        }
    }

    fn modify_existing(&self, index: usize, requested: InotifyMask) -> Result<i32, InotifyError> {
        let (token, previous, descriptor) = {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            let watch = state.slots[index].watch.as_ref().ok_or(InotifyError::InvalidArgument)?;
            if requested.contains(InotifyMask::MASK_CREATE) {
                return Err(InotifyError::AlreadyExists);
            }
            (
                watch.token,
                watch.mask,
                i32::try_from(index + 1).map_err(|_| InotifyError::ResourceLimit)?,
            )
        };
        let next = if requested.contains(InotifyMask::MASK_ADD) {
            InotifyMask::from_bits(previous.bits() | requested.source_bits().bits())
        } else {
            requested.source_bits()
        };
        self.inner.source.modify(token, next)?;
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        let watch = state.slots[index]
            .watch
            .as_mut()
            .filter(|watch| watch.token == token)
            .ok_or(InotifyError::Interrupted)?;
        watch.mask = next;
        Ok(descriptor)
    }

    fn install_new(&self, binding: WatchBinding, requested: InotifyMask) -> Result<i32, InotifyError> {
        let (index, generation, token) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::ensure_active(&state)?;
            let index = match state.slots.iter().position(|slot| slot.watch.is_none()) {
                Some(index) => index,
                None if state.slots.len() < self.inner.limits.watches => {
                    state.slots.push(WatchSlot {
                        generation: 0,
                        watch: None,
                    });
                    state.slots.len() - 1
                }
                None => return Err(InotifyError::ResourceLimit),
            };
            let generation = state.slots[index].generation.wrapping_add(1).max(1);
            state.slots[index].generation = generation;
            let token =
                (u64::from(generation) << 32) | u64::try_from(index + 1).map_err(|_| InotifyError::ResourceLimit)?;
            (index, generation, token)
        };
        let mask = requested.source_bits();
        self.inner.source.add(binding, token, mask)?;
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::ensure_active(&state)?;
        if state.slots[index].generation != generation || state.slots[index].watch.is_some() {
            drop(state);
            let _ = self.inner.source.remove(token);
            return Err(InotifyError::Interrupted);
        }
        state.slots[index].watch = Some(Watch { binding, mask, token });
        i32::try_from(index + 1).map_err(|_| InotifyError::ResourceLimit)
    }

    fn watch_index(watch_descriptor: i32, slots: usize) -> Result<usize, InotifyError> {
        let index = watch_descriptor
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(InotifyError::InvalidArgument)?;
        if index >= slots {
            Err(InotifyError::InvalidArgument)
        } else {
            Ok(index)
        }
    }

    fn ensure_active(state: &InotifyState) -> Result<(), InotifyError> {
        if state.retired {
            Err(InotifyError::Retired)
        } else {
            Ok(())
        }
    }

    fn notify_if(&self, notify: bool) {
        if notify {
            self.inner.changed.notify_all();
            self.inner.readiness.notify();
        }
    }
}

#[cfg(test)]
mod ofd_test;
#[cfg(test)]
mod test;
#[cfg(test)]
mod test_support;
