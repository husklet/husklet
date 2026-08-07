//! Generation-safe provider-backed epoll watch registry.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub const EPOLL_EDGE_TRIGGERED: u32 = 0x8000_0000;
pub const EPOLL_ONE_SHOT: u32 = 0x4000_0000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WatchIdentity {
    pub epoll: i32,
    pub epoll_generation: u32,
    pub descriptor: i32,
    pub descriptor_generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchConfig {
    pub remote_handle: u64,
    pub events: u32,
    pub interests: u32,
    pub data: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WatchToken {
    slot: u32,
    generation: u32,
}

impl WatchToken {
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyEvent {
    pub readiness: u32,
    pub data: u64,
    pub unsubscribe: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    InvalidCapacity,
    Capacity,
    InvalidToken,
    InvalidState,
    Duplicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    pub capacity: usize,
    pub watches: Vec<WatchSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchSnapshot {
    pub token: WatchToken,
    pub identity: WatchIdentity,
    pub config: WatchConfig,
    pub ready: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Reserved,
    Active,
    Retiring,
}

#[derive(Debug)]
struct Watch {
    identity: WatchIdentity,
    config: WatchConfig,
    phase: Phase,
    ready: u32,
    callbacks: usize,
}

#[derive(Debug)]
struct Slot {
    generation: u32,
    watch: Option<Watch>,
}

#[derive(Debug)]
struct RegistryState {
    slots: Vec<Slot>,
}

impl RegistryState {
    fn contains(&self, identity: WatchIdentity) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.watch.as_ref().is_some_and(|watch| watch.identity == identity))
    }

    fn reserve(
        &mut self,
        identity: WatchIdentity,
        config: WatchConfig,
        phase: Phase,
    ) -> Result<WatchToken, RegistryError> {
        let (index, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.watch.is_none())
            .ok_or(RegistryError::Capacity)?;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        let token = WatchToken {
            slot: index as u32,
            generation: slot.generation,
        };
        slot.watch = Some(Watch {
            identity,
            config,
            phase,
            ready: 0,
            callbacks: 0,
        });
        Ok(token)
    }

    fn has_callbacks(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.watch.as_ref().is_some_and(|watch| watch.callbacks != 0))
    }

    fn begin_reset(&mut self) {
        for slot in &mut self.slots {
            if let Some(watch) = &mut slot.watch {
                watch.phase = Phase::Retiring;
            }
        }
    }

    fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.watch = None;
        }
    }
}

#[derive(Debug)]
struct RegistryCore {
    state: Mutex<RegistryState>,
    quiescent: Condvar,
}

#[derive(Clone, Debug)]
pub struct EpollRegistry {
    core: Arc<RegistryCore>,
}

impl EpollRegistry {
    pub fn new(capacity: usize) -> Result<Self, RegistryError> {
        if capacity == 0 || capacity > u32::MAX as usize {
            return Err(RegistryError::InvalidCapacity);
        }
        Ok(Self {
            core: Arc::new(RegistryCore {
                state: Mutex::new(RegistryState {
                    slots: (0..capacity)
                        .map(|_| Slot {
                            generation: 0,
                            watch: None,
                        })
                        .collect(),
                }),
                quiescent: Condvar::new(),
            }),
        })
    }

    pub fn reserve(&self, identity: WatchIdentity, config: WatchConfig) -> Result<WatchToken, RegistryError> {
        let mut state = self.lock();
        if state.contains(identity) {
            return Err(RegistryError::Duplicate);
        }
        state.reserve(identity, config, Phase::Reserved)
    }

    pub fn activate(&self, token: WatchToken) -> Result<(), RegistryError> {
        let mut state = self.lock();
        let watch = Self::watch_mut(&mut state, token)?;
        if watch.phase != Phase::Reserved {
            return Err(RegistryError::InvalidState);
        }
        watch.phase = Phase::Active;
        Ok(())
    }

    pub fn cancel(&self, token: WatchToken) -> Result<(), RegistryError> {
        let mut state = self.lock();
        let watch = Self::watch(&state, token)?;
        if watch.phase != Phase::Reserved || watch.callbacks != 0 {
            return Err(RegistryError::InvalidState);
        }
        state.slots[token.slot as usize].watch = None;
        Ok(())
    }

    pub fn replace(
        &self,
        old: WatchToken,
        identity: WatchIdentity,
        config: WatchConfig,
    ) -> Result<WatchToken, RegistryError> {
        let mut state = self.lock();
        {
            let watch = Self::watch(&state, old)?;
            if watch.phase != Phase::Active || watch.identity != identity {
                return Err(RegistryError::InvalidState);
            }
        }
        Self::watch_mut(&mut state, old)?.phase = Phase::Retiring;
        while Self::watch(&state, old)?.callbacks != 0 {
            state = self
                .core
                .quiescent
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let slot = &mut state.slots[old.slot as usize];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        let replacement = WatchToken {
            slot: old.slot,
            generation: slot.generation,
        };
        slot.watch = Some(Watch {
            identity,
            config,
            phase: Phase::Active,
            ready: 0,
            callbacks: 0,
        });
        Ok(replacement)
    }

    #[must_use]
    pub fn find(&self, identity: WatchIdentity) -> Option<WatchToken> {
        self.lock().slots.iter().enumerate().find_map(|(index, slot)| {
            let watch = slot.watch.as_ref()?;
            (watch.phase == Phase::Active && watch.identity == identity).then_some(WatchToken {
                slot: index as u32,
                generation: slot.generation,
            })
        })
    }

    #[must_use]
    pub fn callback(&self, token: WatchToken) -> Option<CallbackLease> {
        let mut state = self.lock();
        let watch = Self::watch_mut(&mut state, token).ok()?;
        if watch.phase != Phase::Active {
            return None;
        }
        watch.callbacks += 1;
        Some(CallbackLease {
            core: Arc::clone(&self.core),
            token,
            active: true,
        })
    }

    pub fn take_ready(&self, token: WatchToken, level_sample: u32) -> Result<Option<ReadyEvent>, RegistryError> {
        let mut state = self.lock();
        let watch = Self::watch_mut(&mut state, token)?;
        if watch.phase != Phase::Active {
            return Err(RegistryError::InvalidState);
        }
        let readiness = std::mem::take(&mut watch.ready);
        if readiness == 0 {
            return Ok(None);
        }
        let mut unsubscribe = false;
        if watch.config.events & EPOLL_ONE_SHOT != 0 {
            watch.config.interests = 0;
            unsubscribe = true;
        } else if watch.config.events & EPOLL_EDGE_TRIGGERED == 0 {
            watch.ready |= level_sample;
        }
        Ok(Some(ReadyEvent {
            readiness,
            data: watch.config.data,
            unsubscribe,
        }))
    }

    pub fn retire(&self, token: WatchToken) -> Result<(), RegistryError> {
        let mut state = self.lock();
        {
            let watch = Self::watch_mut(&mut state, token)?;
            if watch.phase != Phase::Active {
                return Err(RegistryError::InvalidState);
            }
            watch.phase = Phase::Retiring;
        }
        while Self::watch(&state, token)?.callbacks != 0 {
            state = self
                .core
                .quiescent
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.slots[token.slot as usize].watch = None;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> RegistrySnapshot {
        let state = self.lock();
        let watches = state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let watch = slot.watch.as_ref()?;
                (watch.phase == Phase::Active).then_some(WatchSnapshot {
                    token: WatchToken {
                        slot: index as u32,
                        generation: slot.generation,
                    },
                    identity: watch.identity,
                    config: watch.config,
                    ready: watch.ready,
                })
            })
            .collect();
        RegistrySnapshot {
            capacity: state.slots.len(),
            watches,
        }
    }

    #[must_use]
    pub fn reset(&self) -> RegistrySnapshot {
        let mut state = self.lock();
        let watches = Self::active_snapshots(&state);
        state.begin_reset();
        while state.has_callbacks() {
            state = self
                .core
                .quiescent
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.clear();
        RegistrySnapshot {
            capacity: state.slots.len(),
            watches,
        }
    }

    #[must_use]
    pub const fn linux_events(readiness: u32) -> u32 {
        (if readiness & 1 != 0 { 0x001 } else { 0 })
            | (if readiness & 2 != 0 { 0x004 } else { 0 })
            | (if readiness & 4 != 0 { 0x002 } else { 0 })
            | (if readiness & 8 != 0 { 0x008 } else { 0 })
            | (if readiness & 16 != 0 { 0x010 } else { 0 })
    }

    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        self.core
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn active_snapshots(state: &RegistryState) -> Vec<WatchSnapshot> {
        state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let watch = slot.watch.as_ref()?;
                (watch.phase == Phase::Active).then_some(WatchSnapshot {
                    token: WatchToken {
                        slot: index as u32,
                        generation: slot.generation,
                    },
                    identity: watch.identity,
                    config: watch.config,
                    ready: watch.ready,
                })
            })
            .collect()
    }

    fn watch(state: &RegistryState, token: WatchToken) -> Result<&Watch, RegistryError> {
        let slot = state
            .slots
            .get(token.slot as usize)
            .ok_or(RegistryError::InvalidToken)?;
        if slot.generation != token.generation {
            return Err(RegistryError::InvalidToken);
        }
        slot.watch.as_ref().ok_or(RegistryError::InvalidToken)
    }

    fn watch_mut(state: &mut RegistryState, token: WatchToken) -> Result<&mut Watch, RegistryError> {
        let slot = state
            .slots
            .get_mut(token.slot as usize)
            .ok_or(RegistryError::InvalidToken)?;
        if slot.generation != token.generation {
            return Err(RegistryError::InvalidToken);
        }
        slot.watch.as_mut().ok_or(RegistryError::InvalidToken)
    }
}

pub struct CallbackLease {
    core: Arc<RegistryCore>,
    token: WatchToken,
    active: bool,
}

impl CallbackLease {
    pub fn ready(&self, readiness: u32) {
        let mut state = self
            .core
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(watch) = EpollRegistry::watch_mut(&mut state, self.token) {
            watch.ready |= readiness & watch.config.interests;
        }
    }
}

impl Drop for CallbackLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .core
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(watch) = EpollRegistry::watch_mut(&mut state, self.token) {
            watch.callbacks -= 1;
            if watch.callbacks == 0 {
                self.core.quiescent.notify_all();
            }
        }
        self.active = false;
    }
}
