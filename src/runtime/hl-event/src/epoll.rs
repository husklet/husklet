use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};
pub(crate) mod model;
mod ofd;
mod wait;

use hl_descriptor::{
    ObjectError, OperationLease, Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription,
};

use self::model::{
    EpollError, EpollEvent, EpollInterest, EpollSnapshot, EpollStatus, EpollWatchKey, EpollWatchSnapshot,
};

const EPOLL_MODE: u32 = 0o100_600;
const DEFAULT_WATCH_LIMIT: usize = 4_096;

struct Watch {
    key: EpollWatchKey,
    target: OperationLease,
    interests: EpollInterest,
    data: u64,
    observed: Readiness,
    disabled: bool,
    token: u64,
    revision: u64,
    readiness_sequence: u64,
    queued: bool,
    subscription: Box<dyn ReadinessSubscription>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BatchSelection {
    token: u64,
    revision: u64,
    readiness_sequence: u64,
    sampled: Readiness,
}

/// A non-consuming readiness observation.
///
/// The batch remains valid across unrelated readiness changes. Control changes
/// to a selected watch make commit fail, while a newer edge on the same watch
/// remains queued after commit.
#[derive(Debug)]
pub struct EpollBatch {
    events: Vec<EpollEvent>,
    selections: Vec<BatchSelection>,
}

impl EpollBatch {
    #[must_use]
    pub fn events(&self) -> &[EpollEvent] {
        &self.events
    }
}

struct EpollState {
    watches: Vec<Watch>,
    ready: VecDeque<u64>,
    next_token: u64,
    epoch: u64,
    retired: bool,
}

struct EpollInner {
    watch_limit: usize,
    state: Mutex<EpollState>,
    changed: Condvar,
    readiness: ReadinessRegistry,
    retired_watches: Mutex<Vec<Watch>>,
}

struct TargetObserver {
    epoll: Weak<EpollInner>,
    token: u64,
}

impl ReadinessObserver for TargetObserver {
    fn readiness_changed(&self) {
        let Some(epoll) = self.epoll.upgrade() else {
            return;
        };
        Epoll::signal_token(&epoll, self.token);
    }
}

/// A bounded Linux epoll interest registry and wait state machine.
pub struct Epoll {
    inner: Arc<EpollInner>,
}

impl std::fmt::Debug for Epoll {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Epoll")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Epoll {
    #[must_use]
    pub fn new() -> Self {
        Self::with_watch_limit(DEFAULT_WATCH_LIMIT).expect("the default epoll watch limit is valid")
    }

    pub fn with_watch_limit(watch_limit: usize) -> Result<Self, EpollError> {
        if watch_limit == 0 {
            return Err(EpollError::InvalidArgument);
        }
        Ok(Self {
            inner: Arc::new(EpollInner {
                watch_limit,
                state: Mutex::new(EpollState {
                    watches: Vec::new(),
                    ready: VecDeque::new(),
                    next_token: 1,
                    epoch: 0,
                    retired: false,
                }),
                changed: Condvar::new(),
                readiness: ReadinessRegistry::new(),
                retired_watches: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn from_snapshot(snapshot: &EpollSnapshot, targets: Vec<OperationLease>) -> Result<Self, EpollError> {
        if snapshot.watch_limit == 0
            || snapshot.next_token == 0
            || snapshot.watches.len() != targets.len()
            || snapshot.watches.len() > snapshot.watch_limit
        {
            return Err(EpollError::InvalidArgument);
        }
        for (watch, target) in snapshot.watches.iter().zip(&targets) {
            if watch.key != EpollWatchKey::from_lease(target) || !watch.interests.valid() {
                return Err(EpollError::InvalidArgument);
            }
        }
        let epoll = Self::with_watch_limit(snapshot.watch_limit)?;
        let mut watches = Vec::with_capacity(snapshot.watches.len());
        for (index, (saved, target)) in snapshot.watches.iter().zip(targets).enumerate() {
            let mut token = u64::try_from(index).map_err(|_| EpollError::ResourceLimit)? + 1;
            if token >= snapshot.next_token {
                token = token.checked_add(1).ok_or(EpollError::ResourceLimit)?;
            }
            let observer = Arc::new(TargetObserver {
                epoll: Arc::downgrade(&epoll.inner),
                token,
            });
            let subscription = target.subscribe_readiness(observer)?;
            watches.push(Watch {
                key: saved.key,
                target,
                interests: saved.interests,
                data: saved.data,
                observed: saved.previous,
                disabled: saved.disabled,
                token,
                revision: 1,
                readiness_sequence: 0,
                queued: snapshot.ready.contains(&saved.key),
                subscription,
            });
        }
        let ready = snapshot
            .ready
            .iter()
            .map(|key| {
                watches
                    .iter()
                    .find(|watch| watch.key == *key)
                    .map(|watch| watch.token)
                    .ok_or(EpollError::InvalidArgument)
            })
            .collect::<Result<VecDeque<_>, _>>()?;
        *epoll.inner.state.lock().unwrap_or_else(|error| error.into_inner()) = EpollState {
            watches,
            ready,
            next_token: snapshot.next_token,
            epoch: snapshot.epoch,
            retired: false,
        };
        Ok(epoll)
    }

    pub fn add(
        &self,
        target: OperationLease,
        interests: EpollInterest,
        data: u64,
    ) -> Result<EpollWatchKey, EpollError> {
        if !interests.valid() {
            return Err(EpollError::InvalidArgument);
        }
        self.prune_retired();
        let target = target.into_durable();
        let key = EpollWatchKey::from_lease(&target);
        {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            if state.watches.iter().any(|watch| watch.key == key) {
                return Err(EpollError::AlreadyExists);
            }
            if state.watches.len() == self.inner.watch_limit {
                return Err(EpollError::ResourceLimit);
            }
        }
        let token = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            let token = state.next_token;
            state.next_token = state.next_token.wrapping_add(1).max(1);
            token
        };
        let observer = Arc::new(TargetObserver {
            epoll: Arc::downgrade(&self.inner),
            token,
        });
        let subscription = target.subscribe_readiness(observer)?;
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::validate_active(&state)?;
        if state.watches.iter().any(|watch| watch.key == key) {
            drop(state);
            subscription.quiesce();
            return Err(EpollError::AlreadyExists);
        }
        if state.watches.len() == self.inner.watch_limit {
            drop(state);
            subscription.quiesce();
            return Err(EpollError::ResourceLimit);
        }
        state.watches.push(Watch {
            key,
            target,
            interests,
            data,
            observed: Readiness::default(),
            disabled: false,
            token,
            revision: 1,
            readiness_sequence: 0,
            queued: false,
            subscription,
        });
        Self::wake(&self.inner, &mut state);
        drop(state);
        Self::refresh_token(&self.inner, token);
        Ok(key)
    }

    pub fn modify(&self, target: &OperationLease, interests: EpollInterest, data: u64) -> Result<(), EpollError> {
        if !interests.valid() {
            return Err(EpollError::InvalidArgument);
        }
        let key = EpollWatchKey::from_lease(target);
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::validate_active(&state)?;
        let index = state
            .watches
            .iter()
            .position(|watch| watch.key == key)
            .ok_or(EpollError::NotFound)?;
        let token = state.watches[index].token;
        if interests.exclusive() || state.watches[index].interests.exclusive() {
            return Err(EpollError::InvalidArgument);
        }
        if state.watches[index].queued {
            state.ready.retain(|queued| *queued != token);
        }
        let watch = &mut state.watches[index];
        watch.interests = interests;
        watch.data = data;
        watch.revision = watch.revision.wrapping_add(1).max(1);
        watch.observed = Readiness::default();
        watch.disabled = false;
        watch.queued = false;
        Self::wake(&self.inner, &mut state);
        drop(state);
        Self::refresh_token(&self.inner, token);
        Ok(())
    }

    pub fn delete(&self, target: &OperationLease) -> Result<(), EpollError> {
        let key = EpollWatchKey::from_lease(target);
        let removed = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            let index = state
                .watches
                .iter()
                .position(|watch| watch.key == key)
                .ok_or(EpollError::NotFound)?;
            let removed = state.watches.remove(index);
            state.ready.retain(|token| *token != removed.token);
            Self::wake(&self.inner, &mut state);
            removed
        };
        removed.subscription.quiesce();
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> EpollSnapshot {
        let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        EpollSnapshot {
            watch_limit: self.inner.watch_limit,
            next_token: state.next_token,
            epoch: state.epoch,
            watches: state
                .watches
                .iter()
                .map(|watch| EpollWatchSnapshot {
                    key: watch.key,
                    interests: watch.interests,
                    data: watch.data,
                    previous: watch.observed,
                    disabled: watch.disabled,
                })
                .collect(),
            ready: state
                .ready
                .iter()
                .filter_map(|token| {
                    state
                        .watches
                        .iter()
                        .find(|watch| watch.token == *token)
                        .map(|watch| watch.key)
                })
                .collect(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> EpollStatus {
        EpollStatus {
            mode: EPOLL_MODE,
            size: 0,
            link_count: 1,
        }
    }

    #[must_use]
    pub fn watch_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .watches
            .len()
    }

    pub(crate) fn poll_readiness(&self, interests: Readiness) -> Readiness {
        if !interests.contains(Readiness::READ) {
            return Readiness::default();
        }
        let watches = {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.retired {
                return Readiness::from_bits(Readiness::ERROR);
            }
            state
                .watches
                .iter()
                .filter(|watch| !watch.disabled)
                .map(|watch| (watch.target.clone(), watch.interests))
                .collect::<Vec<_>>()
        };
        if watches
            .into_iter()
            .any(|(target, interest)| target.readiness(interest.readiness()).bits() != 0)
        {
            Readiness::from_bits(Readiness::READ)
        } else {
            Readiness::default()
        }
    }

    fn validate_active(state: &EpollState) -> Result<(), EpollError> {
        if state.retired {
            Err(EpollError::Retired)
        } else {
            Ok(())
        }
    }

    fn refresh_token(inner: &Arc<EpollInner>, token: u64) {
        let snapshot = {
            let state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.retired {
                return;
            }
            let Some(watch) = state.watches.iter().find(|watch| watch.token == token) else {
                return;
            };
            (watch.target.clone(), watch.interests, watch.observed, watch.disabled)
        };
        let (target, interests, observed, disabled) = snapshot;
        let sampled = target.readiness(interests.readiness());
        let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = state.watches.iter().position(|watch| watch.token == token) else {
            return;
        };
        let transition = Readiness::from_bits(sampled.bits() & !observed.bits()).bits() != 0;
        let should_queue = !disabled && sampled.bits() != 0 && (!interests.edge_triggered() || transition);
        state.watches[index].observed = sampled;
        if should_queue && !state.watches[index].queued {
            state.watches[index].queued = true;
            state.ready.push_back(token);
        }
        if should_queue {
            state.watches[index].readiness_sequence = state.watches[index].readiness_sequence.wrapping_add(1).max(1);
        }
        Self::wake(inner, &mut state);
        drop(state);
        inner.readiness.notify();
    }

    fn signal_token(inner: &Arc<EpollInner>, token: u64) {
        let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired {
            return;
        }
        let Some(index) = state.watches.iter().position(|watch| watch.token == token) else {
            return;
        };
        if state.watches[index].disabled {
            return;
        }
        state.watches[index].readiness_sequence = state.watches[index].readiness_sequence.wrapping_add(1).max(1);
        if !state.watches[index].queued {
            state.watches[index].queued = true;
            state.ready.push_back(token);
        }
        Self::wake(inner, &mut state);
        drop(state);
        inner.readiness.notify();
    }

    fn prune_retired(&self) {
        let removed = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::take_retired(&mut state)
        };
        for watch in removed {
            watch.subscription.quiesce();
        }
    }

    fn take_retired(state: &mut EpollState) -> Vec<Watch> {
        let mut removed = Vec::new();
        let mut index = 0;
        while index < state.watches.len() {
            if !state.watches[index].target.retired() {
                index += 1;
                continue;
            }
            let watch = state.watches.remove(index);
            state.ready.retain(|token| *token != watch.token);
            removed.push(watch);
        }
        removed
    }

    fn wake(inner: &EpollInner, state: &mut EpollState) {
        state.epoch = state.epoch.wrapping_add(1);
        inner.changed.notify_all();
    }

    pub(crate) fn subscribe_observer(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.readiness.subscribe(observer)
    }

    pub(crate) fn retire_description(&self) {
        let watches = {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.retired {
                return;
            }
            state.retired = true;
            state.epoch = state.epoch.wrapping_add(1);
            self.inner.changed.notify_all();
            std::mem::take(&mut state.watches)
        };
        self.inner
            .retired_watches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .extend(watches);
    }

    /// Completes final close after descriptor retirement has stopped new waits.
    /// Every target subscription is synchronously quiesced exactly once.
    pub fn finish_retirement(&self) {
        let watches = std::mem::take(
            &mut *self
                .inner
                .retired_watches
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for watch in watches {
            watch.subscription.quiesce()
        }
        self.inner.readiness.close();
    }
}
