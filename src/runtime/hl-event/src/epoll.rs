use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};
pub(crate) mod model;
mod ofd;
mod readiness;
mod registration;
mod wait;

use hl_descriptor::{OperationLease, Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};

use self::model::{
    EpollError, EpollEvent, EpollInterest, EpollSnapshot, EpollStatus, EpollWatchKey, EpollWatchSnapshot,
};

const EPOLL_MODE: u32 = 0o100_600;
const DEFAULT_WATCH_LIMIT: usize = 4_096;

pub(crate) struct Watch {
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
pub(crate) struct BatchSelection {
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

pub(crate) struct EpollState {
    watches: Vec<Watch>,
    token_index: HashMap<u64, usize>,
    ready: VecDeque<u64>,
    next_token: u64,
    epoch: u64,
    retired: bool,
}

pub(crate) struct EpollInner {
    watch_limit: usize,
    state: Mutex<EpollState>,
    changed: Condvar,
    readiness: ReadinessRegistry,
    retired_watches: Mutex<Vec<Watch>>,
}

pub(crate) struct TargetObserver {
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
                    token_index: HashMap::new(),
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
        let token_index = watches
            .iter()
            .enumerate()
            .map(|(index, watch)| (watch.token, index))
            .collect();
        *epoll.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = EpollState {
            watches,
            token_index,
            ready,
            next_token: snapshot.next_token,
            epoch: snapshot.epoch,
            retired: false,
        };
        Ok(epoll)
    }

    #[must_use]
    pub fn snapshot(&self) -> EpollSnapshot {
        let state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
                .filter_map(|token| state.token_index.get(token).map(|index| state.watches[*index].key))
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .watches
            .len()
    }

    pub(crate) fn poll_readiness(&self, interests: Readiness) -> Readiness {
        if !interests.contains(Readiness::READ) {
            return Readiness::default();
        }
        let watches = {
            let state = self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

}
