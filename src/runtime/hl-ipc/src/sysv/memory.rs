use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_memory::SharedBackingRef;

use super::model::{
    ATTACH_FLAGS, AttachPlan, Credentials, IPC_PRIVATE, IpcKey, SHM_RDONLY, SHM_REMAP, SHM_RND, SharedMemoryError,
    SharedMemoryId, SharedMemoryLimits, SharedMemoryLockIntent, SharedMemoryMetadata, SharedMemorySnapshot,
    ShmGetRequest,
};
use crate::SHM_EXEC;

mod exec;
mod fork;
mod snapshot;
pub use exec::{CommittedMemoryExec, PreparedMemoryExec};
pub use fork::{CommittedMemoryFork, ForkAttachmentPlan, OwnedPreparedMemoryFork, PreparedMemoryFork};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Segment {
    pub(super) metadata: SharedMemoryMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Slot {
    pub(super) generation: u32,
    pub(super) segment: Option<Segment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Attachment {
    pub(super) segment: SharedMemoryId,
    pub(super) pid: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NamespaceState {
    pub(super) slots: Vec<Slot>,
    pub(super) attachments: BTreeMap<u64, Attachment>,
    pub(super) next_attachment: u64,
    pub(super) allocated: usize,
}

#[derive(Debug)]
pub struct SharedMemoryNamespace {
    pub(super) memory: Arc<dyn crate::SharedBackingAccess>,
    limits: SharedMemoryLimits,
    pub(super) state: Mutex<NamespaceState>,
}

impl SharedMemoryNamespace {
    pub fn new(
        memory: Arc<dyn crate::SharedBackingAccess>,
        limits: SharedMemoryLimits,
    ) -> Result<Self, SharedMemoryError> {
        if limits.segments == 0 || limits.segment_bytes > limits.total_bytes || limits.attachments == 0 {
            return Err(SharedMemoryError::InvalidArgument);
        }
        Ok(Self {
            memory,
            limits,
            state: Mutex::new(NamespaceState {
                slots: Vec::new(),
                attachments: BTreeMap::new(),
                next_attachment: 1,
                allocated: 0,
            }),
        })
    }

    pub fn shmget(&self, request: ShmGetRequest) -> Result<SharedMemoryId, SharedMemoryError> {
        if request.mode & !0o777 != 0 {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if request.key != IPC_PRIVATE {
            if let Some(id) = Self::key_id(&state, request.key) {
                return self.open_existing(&state, id, request);
            }
            if !request.create {
                return Err(SharedMemoryError::NotFound);
            }
        }
        self.create_segment(&mut state, request)
    }

    pub fn shmat_plan(
        &self,
        id: SharedMemoryId,
        actor: Credentials,
        flags: u32,
    ) -> Result<AttachPlan, SharedMemoryError> {
        if flags & !ATTACH_FLAGS != 0 {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let segment = Self::segment(&state, id)?;
        if segment.metadata.marked_for_removal {
            return Err(SharedMemoryError::Removed);
        }
        let read_only = flags & SHM_RDONLY != 0;
        self.require(&segment.metadata, actor, if read_only { 0o4 } else { 0o6 })?;
        Ok(AttachPlan {
            segment: id,
            backing: SharedBackingRef {
                object: segment.metadata.backing,
                offset: 0,
                length: Self::page_extent(segment.metadata.size)?,
                write_shared: true,
            },
            read_only,
            executable: flags & SHM_EXEC != 0,
            round_address: flags & SHM_RND != 0,
            replace: flags & SHM_REMAP != 0,
        })
    }

    pub fn commit_attach(&self, plan: AttachPlan, pid: u32, now: u64) -> Result<u64, SharedMemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.attachments.len() >= self.limits.attachments {
            return Err(SharedMemoryError::ResourceLimit);
        }
        let segment = Self::segment(&state, plan.segment)?;
        if segment.metadata.marked_for_removal
            || segment.metadata.backing != plan.backing.object
            || Self::page_extent(segment.metadata.size)? != plan.backing.length
        {
            return Err(SharedMemoryError::Removed);
        }
        let token = state.next_attachment;
        state.next_attachment = state
            .next_attachment
            .checked_add(1)
            .ok_or(SharedMemoryError::ResourceLimit)?;
        let segment = Self::segment_mut(&mut state, plan.segment)?;
        segment.metadata.attaches += 1;
        segment.metadata.last_pid = pid;
        segment.metadata.attached_at = Some(now);
        state.attachments.insert(
            token,
            Attachment {
                segment: plan.segment,
                pid,
            },
        );
        Ok(token)
    }

    pub fn shmdt(&self, attachment: u64, pid: u32, now: u64) -> Result<(), SharedMemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let attached = state
            .attachments
            .get(&attachment)
            .copied()
            .ok_or(SharedMemoryError::NotFound)?;
        if attached.pid != pid {
            return Err(SharedMemoryError::Permission);
        }
        state.attachments.remove(&attachment);
        self.detach_segment(&mut state, attached.segment, pid, now)
    }

    pub fn remove(&self, id: SharedMemoryId, actor: Credentials, pid: u32, now: u64) -> Result<(), SharedMemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let segment = Self::segment_mut(&mut state, id)?;
        if actor.uid != 0 && actor.uid != segment.metadata.owner.uid && actor.uid != segment.metadata.creator_uid {
            return Err(SharedMemoryError::Permission);
        }
        segment.metadata.key = None;
        segment.metadata.marked_for_removal = true;
        segment.metadata.last_pid = pid;
        segment.metadata.changed_at = now;
        if segment.metadata.attaches == 0 {
            self.destroy(&mut state, id)?;
        }
        Ok(())
    }

    pub fn set_permissions(
        &self,
        id: SharedMemoryId,
        actor: Credentials,
        owner: Credentials,
        mode: u16,
        pid: u32,
        now: u64,
    ) -> Result<(), SharedMemoryError> {
        if mode & !0o777 != 0 {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let segment = Self::segment_mut(&mut state, id)?;
        if actor.uid != 0 && actor.uid != segment.metadata.owner.uid && actor.uid != segment.metadata.creator_uid {
            return Err(SharedMemoryError::Permission);
        }
        segment.metadata.owner = owner;
        segment.metadata.mode = mode;
        segment.metadata.last_pid = pid;
        segment.metadata.changed_at = now;
        Ok(())
    }

    /// Authorizes a page-residency control operation for a live segment.
    ///
    /// Both intents deliberately leave the segment and its backing unchanged:
    /// the engine has no host-wired-page state. Resolving and authorizing while
    /// holding the namespace lock preserves the retained implementation's
    /// generation, removal, and ownership ordering.
    pub fn authorize_lock(
        &self,
        id: SharedMemoryId,
        actor: Credentials,
        _intent: SharedMemoryLockIntent,
    ) -> Result<(), SharedMemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let segment = Self::segment(&state, id)?;
        if segment.metadata.marked_for_removal {
            return Err(SharedMemoryError::Removed);
        }
        if actor.uid != 0 && actor.uid != segment.metadata.owner.uid && actor.uid != segment.metadata.creator_uid {
            return Err(SharedMemoryError::Permission);
        }
        Ok(())
    }

    pub fn exit(&self, pid: u32, now: u64) -> Result<(), SharedMemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let detached: Vec<_> = state
            .attachments
            .iter()
            .filter(|(_, attachment)| attachment.pid == pid)
            .map(|(token, attachment)| (*token, attachment.segment))
            .collect();
        for (token, id) in detached {
            state.attachments.remove(&token);
            self.detach_segment(&mut state, id, pid, now)?;
        }
        Ok(())
    }

    pub fn metadata(&self, id: SharedMemoryId) -> Result<SharedMemoryMetadata, SharedMemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(Self::segment(&state, id)?.metadata)
    }

    fn create_segment(
        &self,
        state: &mut NamespaceState,
        request: ShmGetRequest,
    ) -> Result<SharedMemoryId, SharedMemoryError> {
        if request.size == 0 {
            return Err(SharedMemoryError::InvalidArgument);
        }
        if request.size > self.limits.segment_bytes
            || state
                .allocated
                .checked_add(request.size)
                .is_none_or(|total| total > self.limits.total_bytes)
        {
            return Err(SharedMemoryError::ResourceLimit);
        }
        let index = state.slots.iter().position(|slot| slot.segment.is_none());
        let index = match index {
            Some(index) => index,
            None if state.slots.len() < self.limits.segments => {
                state.slots.push(Slot {
                    generation: 1,
                    segment: None,
                });
                state.slots.len() - 1
            }
            None => return Err(SharedMemoryError::ResourceLimit),
        };
        let id = SharedMemoryId {
            slot: u32::try_from(index).map_err(|_| SharedMemoryError::ResourceLimit)?,
            generation: state.slots[index].generation,
        };
        let backing_size =
            usize::try_from(Self::page_extent(request.size)?).map_err(|_| SharedMemoryError::ResourceLimit)?;
        let backing = self.memory.create(u64::from(request.actor.uid), backing_size)?;
        state.slots[index].segment = Some(Segment {
            metadata: SharedMemoryMetadata {
                id,
                key: (request.key != IPC_PRIVATE).then_some(request.key),
                backing,
                size: request.size,
                owner: request.actor,
                creator_uid: request.actor.uid,
                creator_gid: request.actor.gid,
                mode: request.mode,
                creator_pid: request.pid,
                last_pid: 0,
                attaches: 0,
                marked_for_removal: false,
                created_at: request.now,
                attached_at: None,
                detached_at: None,
                changed_at: request.now,
            },
        });
        state.allocated += request.size;
        Ok(id)
    }

    fn open_existing(
        &self,
        state: &NamespaceState,
        id: SharedMemoryId,
        request: ShmGetRequest,
    ) -> Result<SharedMemoryId, SharedMemoryError> {
        if request.create && request.exclusive {
            return Err(SharedMemoryError::Exists);
        }
        let segment = Self::segment(state, id)?;
        if request.size > segment.metadata.size {
            return Err(SharedMemoryError::Size);
        }
        self.require(&segment.metadata, request.actor, (request.mode >> 6) & 0o6)?;
        Ok(id)
    }

    #[allow(clippy::unused_self)]
    fn require(
        &self,
        metadata: &SharedMemoryMetadata,
        actor: Credentials,
        requested: u16,
    ) -> Result<(), SharedMemoryError> {
        if actor.uid == 0 {
            return Ok(());
        }
        let shift = if actor.uid == metadata.owner.uid {
            6
        } else if actor.gid == metadata.owner.gid {
            3
        } else {
            0
        };
        ((metadata.mode >> shift) & requested == requested)
            .then_some(())
            .ok_or(SharedMemoryError::Permission)
    }

    fn key_id(state: &NamespaceState, key: IpcKey) -> Option<SharedMemoryId> {
        state.slots.iter().find_map(|slot| {
            let segment = slot.segment.as_ref()?;
            (segment.metadata.key == Some(key)).then_some(segment.metadata.id)
        })
    }

    fn segment(state: &NamespaceState, id: SharedMemoryId) -> Result<&Segment, SharedMemoryError> {
        let slot = state.slots.get(id.slot as usize).ok_or(SharedMemoryError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedMemoryError::NotFound);
        }
        slot.segment.as_ref().ok_or(SharedMemoryError::NotFound)
    }

    fn segment_mut(state: &mut NamespaceState, id: SharedMemoryId) -> Result<&mut Segment, SharedMemoryError> {
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(SharedMemoryError::NotFound)?;
        if slot.generation != id.generation {
            return Err(SharedMemoryError::NotFound);
        }
        slot.segment.as_mut().ok_or(SharedMemoryError::NotFound)
    }

    fn detach_segment(
        &self,
        state: &mut NamespaceState,
        id: SharedMemoryId,
        pid: u32,
        now: u64,
    ) -> Result<(), SharedMemoryError> {
        let segment = Self::segment_mut(state, id)?;
        segment.metadata.attaches = segment
            .metadata
            .attaches
            .checked_sub(1)
            .ok_or(SharedMemoryError::InvalidArgument)?;
        segment.metadata.last_pid = pid;
        segment.metadata.detached_at = Some(now);
        if segment.metadata.marked_for_removal && segment.metadata.attaches == 0 {
            self.destroy(state, id)?;
        }
        Ok(())
    }

    fn destroy(&self, state: &mut NamespaceState, id: SharedMemoryId) -> Result<(), SharedMemoryError> {
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(SharedMemoryError::NotFound)?;
        let segment = slot.segment.take().ok_or(SharedMemoryError::NotFound)?;
        self.memory.remove(segment.metadata.backing)?;
        state.allocated -= segment.metadata.size;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        Ok(())
    }

    fn validate_restored(
        &self,
        state: &NamespaceState,
        metadata: SharedMemoryMetadata,
    ) -> Result<(), SharedMemoryError> {
        if metadata.id.generation == 0
            || metadata.size == 0
            || metadata.size > self.limits.segment_bytes
            || state
                .allocated
                .checked_add(metadata.size)
                .is_none_or(|total| total > self.limits.total_bytes)
            || metadata.mode & !0o777 != 0
        {
            return Err(SharedMemoryError::InvalidArgument);
        }
        let reference = SharedBackingRef {
            object: metadata.backing,
            offset: 0,
            length: metadata.size as u64,
            write_shared: true,
        };
        self.memory.validate(reference)?;
        if metadata.key.is_some_and(|key| Self::key_id(state, key).is_some()) {
            return Err(SharedMemoryError::Exists);
        }
        Ok(())
    }

    fn validate_attach_counts(state: &NamespaceState) -> Result<(), SharedMemoryError> {
        for slot in &state.slots {
            let Some(segment) = &slot.segment else { continue };
            let count = state
                .attachments
                .values()
                .filter(|attachment| attachment.segment == segment.metadata.id)
                .count();
            if count != segment.metadata.attaches || segment.metadata.marked_for_removal && count == 0 {
                return Err(SharedMemoryError::InvalidArgument);
            }
        }
        Ok(())
    }

    fn page_extent(size: usize) -> Result<u64, SharedMemoryError> {
        let size = u64::try_from(size).map_err(|_| SharedMemoryError::ResourceLimit)?;
        size.checked_add(4095)
            .map(|value| value & !4095)
            .ok_or(SharedMemoryError::ResourceLimit)
    }
}
