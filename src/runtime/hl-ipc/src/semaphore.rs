use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};

use hl_sync::WaitQueue;

use self::model::{
    SemGetRequest, SemaphoreError, SemaphoreId, SemaphoreLimits, SemaphoreMetadata, SemaphoreSetSnapshot,
    SemaphoreSnapshot,
};
use crate::{Credentials, IPC_PRIVATE, IpcKey};

mod exec;
mod exit;
mod fork;
pub(crate) mod model;
mod operation;
mod snapshot;
mod wait;
pub use exec::PreparedSemaphoreExec;
pub use exit::{CommittedSemaphoreExit, PreparedSemaphoreExit};
pub use fork::{CommittedSemaphoreFork, PreparedSemaphoreFork};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Set {
    pub(super) metadata: SemaphoreMetadata,
    pub(super) values: Vec<u16>,
    pub(super) last_pids: Vec<u32>,
    decrement_waiters: Vec<usize>,
    zero_waiters: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Slot {
    pub(super) generation: u32,
    pub(super) set: Option<Set>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct State {
    pub(super) slots: Vec<Slot>,
    semaphores: usize,
    pub(super) undo: BTreeMap<(u32, SemaphoreId, u16), i32>,
}

#[derive(Debug)]
pub struct SemaphoreNamespace {
    pub(super) limits: SemaphoreLimits,
    pub(super) state: Mutex<State>,
    pub(super) changed: WaitQueue,
    waiters: AtomicUsize,
}

impl SemaphoreNamespace {
    pub fn new(limits: SemaphoreLimits) -> Result<Self, SemaphoreError> {
        if limits.sets == 0
            || limits.set_semaphores == 0
            || limits.set_semaphores > limits.total_semaphores
            || limits.maximum_value == 0
            || limits.operations == 0
            || limits.undo_entries == 0
        {
            return Err(SemaphoreError::InvalidArgument);
        }
        Ok(Self {
            limits,
            state: Mutex::new(State {
                slots: Vec::new(),
                semaphores: 0,
                undo: BTreeMap::new(),
            }),
            changed: WaitQueue::new(),
            waiters: AtomicUsize::new(0),
        })
    }

    pub fn semget(&self, request: SemGetRequest) -> Result<SemaphoreId, SemaphoreError> {
        if request.mode & !0o777 != 0 {
            return Err(SemaphoreError::InvalidArgument);
        }
        let mut state = self.lock();
        if request.key != IPC_PRIVATE {
            return self.keyed_get(&mut state, request);
        }
        self.create(&mut state, request)
    }

    pub fn get_value(&self, id: SemaphoreId, index: usize, actor: Credentials) -> Result<u16, SemaphoreError> {
        let state = self.lock();
        let set = Self::set(&state, id)?;
        Self::require(&set.metadata, actor, 0o4)?;
        set.values.get(index).copied().ok_or(SemaphoreError::Range)
    }

    pub fn get_all(&self, id: SemaphoreId, actor: Credentials) -> Result<Vec<u16>, SemaphoreError> {
        let state = self.lock();
        let set = Self::set(&state, id)?;
        Self::require(&set.metadata, actor, 0o4)?;
        Ok(set.values.clone())
    }

    pub fn set_value(
        &self,
        id: SemaphoreId,
        index: usize,
        value: u16,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> Result<(), SemaphoreError> {
        if value > self.limits.maximum_value {
            return Err(SemaphoreError::Range);
        }
        let mut state = self.lock();
        Self::require(&Self::set(&state, id)?.metadata, actor, 0o2)?;
        let set = Self::set_mut(&mut state, id)?;
        *set.values.get_mut(index).ok_or(SemaphoreError::Range)? = value;
        set.last_pids[index] = pid;
        set.metadata.last_pid = pid;
        set.metadata.changed_at = now;
        state
            .undo
            .retain(|(_, set_id, sem), _| *set_id != id || usize::from(*sem) != index);
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub fn set_all(
        &self,
        id: SemaphoreId,
        values: &[u16],
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> Result<(), SemaphoreError> {
        if values.iter().any(|value| *value > self.limits.maximum_value) {
            return Err(SemaphoreError::Range);
        }
        let mut state = self.lock();
        Self::require(&Self::set(&state, id)?.metadata, actor, 0o2)?;
        let set = Self::set_mut(&mut state, id)?;
        if values.len() != set.values.len() {
            return Err(SemaphoreError::InvalidArgument);
        }
        set.values.copy_from_slice(values);
        set.last_pids.fill(pid);
        set.metadata.last_pid = pid;
        set.metadata.changed_at = now;
        state.undo.retain(|(_, set_id, _), _| *set_id != id);
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub fn get_wait_counts(
        &self,
        id: SemaphoreId,
        index: usize,
        actor: Credentials,
    ) -> Result<(usize, usize), SemaphoreError> {
        let state = self.lock();
        let set = Self::set(&state, id)?;
        Self::require(&set.metadata, actor, 0o4)?;
        Ok((
            *set.decrement_waiters.get(index).ok_or(SemaphoreError::Range)?,
            *set.zero_waiters.get(index).ok_or(SemaphoreError::Range)?,
        ))
    }

    pub fn get_pid(&self, id: SemaphoreId, index: usize, actor: Credentials) -> Result<u32, SemaphoreError> {
        let state = self.lock();
        let set = Self::set(&state, id)?;
        Self::require(&set.metadata, actor, 0o4)?;
        set.last_pids.get(index).copied().ok_or(SemaphoreError::Range)
    }

    pub fn metadata(&self, id: SemaphoreId) -> Result<SemaphoreMetadata, SemaphoreError> {
        Ok(Self::set(&self.lock(), id)?.metadata.clone())
    }

    pub fn set_permissions(
        &self,
        id: SemaphoreId,
        actor: Credentials,
        owner: Credentials,
        mode: u16,
        now: u64,
    ) -> Result<(), SemaphoreError> {
        if mode & !0o777 != 0 {
            return Err(SemaphoreError::InvalidArgument);
        }
        let mut state = self.lock();
        let set = Self::set_mut(&mut state, id)?;
        if actor.uid != 0 && actor.uid != set.metadata.owner.uid && actor.uid != set.metadata.creator_uid {
            return Err(SemaphoreError::Permission);
        }
        set.metadata.owner = owner;
        set.metadata.mode = mode;
        set.metadata.changed_at = now;
        Ok(())
    }

    pub fn remove(&self, id: SemaphoreId, actor: Credentials, pid: u32, now: u64) -> Result<(), SemaphoreError> {
        let mut state = self.lock();
        let set = Self::set(&state, id)?;
        if actor.uid != 0 && actor.uid != set.metadata.owner.uid && actor.uid != set.metadata.creator_uid {
            return Err(SemaphoreError::Permission);
        }
        let count = {
            let slot = state.slots.get_mut(id.slot as usize).ok_or(SemaphoreError::NotFound)?;
            let mut set = slot.set.take().ok_or(SemaphoreError::Removed)?;
            set.metadata.last_pid = pid;
            set.metadata.changed_at = now;
            slot.generation = slot.generation.wrapping_add(1).max(1);
            set.values.len()
        };
        state.semaphores -= count;
        state.undo.retain(|(_, set_id, _), _| *set_id != id);
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub fn fork(&self, _parent: u32, child: u32) {
        self.lock().undo.retain(|(pid, _, _), _| *pid != child);
    }

    /// `SEM_UNDO` adjustments survive `execve`; only the process image changes.
    pub const fn exec(&self, _pid: u32) {}

    pub fn exit(&self, pid: u32, now: u64) {
        let mut state = self.lock();
        let adjustments: Vec<_> = state
            .undo
            .iter()
            .filter(|((owner, _, _), _)| *owner == pid)
            .map(|(key, value)| (*key, *value))
            .collect();
        state.undo.retain(|(owner, _, _), _| *owner != pid);
        for ((_, id, index), adjustment) in adjustments {
            if let Ok(set) = Self::set_mut(&mut state, id) {
                let value = i32::from(set.values[index as usize]) + adjustment;
                set.values[index as usize] = value.clamp(0, i32::from(self.limits.maximum_value)) as u16;
                set.last_pids[index as usize] = pid;
                set.metadata.last_pid = pid;
                set.metadata.operated_at = Some(now);
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    fn keyed_get(&self, state: &mut State, request: SemGetRequest) -> Result<SemaphoreId, SemaphoreError> {
        let Some(id) = Self::key_id(state, request.key) else {
            if request.create {
                return self.create(state, request);
            }
            return Err(SemaphoreError::NotFound);
        };
        if request.create && request.exclusive {
            return Err(SemaphoreError::Exists);
        }
        let set = Self::set(state, id)?;
        if request.semaphores > set.values.len() {
            return Err(SemaphoreError::InvalidArgument);
        }
        Self::require(&set.metadata, request.actor, (request.mode >> 6) & 0o6)?;
        Ok(id)
    }

    fn create(&self, state: &mut State, request: SemGetRequest) -> Result<SemaphoreId, SemaphoreError> {
        if request.semaphores == 0 {
            return Err(SemaphoreError::InvalidArgument);
        }
        if request.semaphores > self.limits.set_semaphores
            || state
                .semaphores
                .checked_add(request.semaphores)
                .is_none_or(|v| v > self.limits.total_semaphores)
        {
            return Err(SemaphoreError::ResourceLimit);
        }
        let index = state.slots.iter().position(|slot| slot.set.is_none());
        let index = match index {
            Some(index) => index,
            None if state.slots.len() < self.limits.sets => {
                state.slots.push(Slot {
                    generation: 1,
                    set: None,
                });
                state.slots.len() - 1
            }
            None => return Err(SemaphoreError::ResourceLimit),
        };
        let id = SemaphoreId {
            slot: index as u32,
            generation: state.slots[index].generation,
        };
        state.slots[index].set = Some(Set {
            metadata: SemaphoreMetadata {
                id,
                key: (request.key != IPC_PRIVATE).then_some(request.key),
                owner: request.actor,
                creator_uid: request.actor.uid,
                creator_gid: request.actor.gid,
                mode: request.mode,
                last_pid: 0,
                created_at: request.now,
                operated_at: None,
                changed_at: request.now,
            },
            values: vec![0; request.semaphores],
            last_pids: vec![0; request.semaphores],
            decrement_waiters: vec![0; request.semaphores],
            zero_waiters: vec![0; request.semaphores],
        });
        state.semaphores += request.semaphores;
        Ok(id)
    }

    fn require(metadata: &SemaphoreMetadata, actor: Credentials, requested: u16) -> Result<(), SemaphoreError> {
        if actor.uid == 0 {
            return Ok(());
        }
        let shift = if actor.uid == metadata.owner.uid || actor.uid == metadata.creator_uid {
            6
        } else if actor.gid == metadata.owner.gid || actor.gid == metadata.creator_gid {
            3
        } else {
            0
        };
        ((metadata.mode >> shift) & requested == requested)
            .then_some(())
            .ok_or(SemaphoreError::Permission)
    }

    fn key_id(state: &State, key: IpcKey) -> Option<SemaphoreId> {
        state.slots.iter().find_map(|slot| {
            let set = slot.set.as_ref()?;
            (set.metadata.key == Some(key)).then_some(set.metadata.id)
        })
    }

    fn set(state: &State, id: SemaphoreId) -> Result<&Set, SemaphoreError> {
        let slot = state.slots.get(id.slot as usize).ok_or(SemaphoreError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SemaphoreError::Removed);
        }
        slot.set.as_ref().ok_or(SemaphoreError::Removed)
    }

    fn set_mut(state: &mut State, id: SemaphoreId) -> Result<&mut Set, SemaphoreError> {
        let slot = state.slots.get_mut(id.slot as usize).ok_or(SemaphoreError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SemaphoreError::Removed);
        }
        slot.set.as_mut().ok_or(SemaphoreError::Removed)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
