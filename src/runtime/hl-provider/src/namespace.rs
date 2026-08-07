use std::sync::{Arc, Mutex, MutexGuard};

mod identity;
mod reservation;

pub use identity::{Handle, HandleKind, RemoteId};
pub use reservation::{HandleReservation, Limits as NamespaceLimits};

const TAG: u64 = 0x4850_0000_0000_0000;
const TAG_MASK: u64 = 0xffff_0000_0000_0000;
pub(crate) const MAX_CAPACITY: usize = u32::MAX as usize;

/// Single-owner capability used to move a resource between namespaces.
///
/// It deliberately contains only protocol-safe scalar identity. It is not
/// cloneable, and callers must either accept it into a namespace or close it.
#[derive(Debug)]
#[must_use = "a transferred resource must be accepted or explicitly closed"]
pub struct TransferCapability {
    remote: RemoteId,
    kind: HandleKind,
}

impl TransferCapability {
    #[must_use]
    pub fn remote(&self) -> RemoteId {
        self.remote
    }

    #[must_use]
    pub fn kind(&self) -> HandleKind {
        self.kind
    }

    pub fn close(self) -> Close {
        Close {
            remote: self.remote,
            kind: self.kind,
        }
    }
}

/// Proof that the caller owns the one required remote close operation.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "the remote resource must be closed exactly once"]
pub struct Close {
    remote: RemoteId,
    kind: HandleKind,
}

impl Close {
    #[must_use]
    pub fn remote(&self) -> RemoteId {
        self.remote
    }

    #[must_use]
    pub fn kind(&self) -> HandleKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceError {
    InvalidCapacity,
    Full,
    InvalidHandle,
    WrongKind,
    ReferenceLimit,
    SharedTransfer,
    ForkLimit,
    InvalidSnapshot,
    Busy,
}

#[derive(Debug)]
pub(crate) struct RemoteLease {
    remote: RemoteId,
    kind: HandleKind,
    owners: Mutex<u32>,
}

impl RemoteLease {
    pub(crate) fn new(remote: RemoteId, kind: HandleKind) -> Self {
        Self {
            remote,
            kind,
            owners: Mutex::new(1),
        }
    }

    fn acquire(&self) -> Result<(), NamespaceError> {
        let mut owners = self.owners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *owners = owners.checked_add(1).ok_or(NamespaceError::ReferenceLimit)?;
        Ok(())
    }

    fn release(&self) -> Option<Close> {
        let mut owners = self.owners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *owners -= 1;
        (*owners == 0).then_some(Close {
            remote: self.remote,
            kind: self.kind,
        })
    }

    fn is_sole(&self) -> bool {
        *self.owners.lock().unwrap_or_else(std::sync::PoisonError::into_inner) == 1
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceSnapshot {
    pub capacity: usize,
    pub live: usize,
    pub references: u64,
    pub generations: Vec<u16>,
    pub entries: Vec<SnapshotEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotEntry {
    pub slot: usize,
    pub generation: u16,
    pub remote: RemoteId,
    pub kind: HandleKind,
    pub references: u32,
}

#[derive(Clone)]
pub(crate) struct Resource {
    pub(crate) lease: Arc<RemoteLease>,
    pub(crate) references: u32,
}

pub(crate) struct Slot {
    pub(crate) generation: u16,
    pub(crate) reserved: bool,
    pub(crate) resource: Option<Resource>,
}

pub(crate) struct State {
    pub(crate) slots: Vec<Slot>,
}

/// Bounded, concurrency-safe provider handle namespace.
pub struct HandleNamespace {
    pub(crate) state: Mutex<State>,
    pub(crate) activity: Arc<crate::checkpoint_activity::CheckpointActivity>,
}

#[must_use = "a provider namespace fork must be committed or rolled back"]
pub struct NamespaceForkPlan {
    child: Option<HandleNamespace>,
}

impl NamespaceForkPlan {
    pub fn snapshot(&self) -> NamespaceSnapshot {
        self.child
            .as_ref()
            .expect("an active fork plan owns a child")
            .snapshot()
    }

    pub fn commit(mut self) -> HandleNamespace {
        self.child.take().expect("an active fork plan owns a child")
    }

    pub fn rollback(mut self) {
        if let Some(child) = self.child.take() {
            child.release_all();
        }
    }
}

impl Drop for NamespaceForkPlan {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            child.release_all();
        }
    }
}

impl HandleNamespace {
    pub fn restore_checkpoint(
        snapshot: &NamespaceSnapshot,
        remotes: &[(usize, RemoteId)],
    ) -> Result<Self, NamespaceError> {
        crate::checkpoint::ProviderCheckpointImage::restore_namespace(snapshot, remotes)
    }

    pub fn new(capacity: usize) -> Result<Self, NamespaceError> {
        Self::with_limits(NamespaceLimits::new(capacity)?)
    }

    pub fn with_limits(limits: NamespaceLimits) -> Result<Self, NamespaceError> {
        let slots = (0..limits.handles)
            .map(|_| Slot {
                generation: 0,
                reserved: false,
                resource: None,
            })
            .collect();
        Ok(Self {
            state: Mutex::new(State { slots }),
            activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
        })
    }

    pub fn open(&self, remote: RemoteId, kind: HandleKind) -> Result<Handle, NamespaceError> {
        self.reserve(kind)?.publish(remote)
    }

    pub fn reserve(&self, kind: HandleKind) -> Result<HandleReservation<'_>, NamespaceError> {
        let admission = self.activity.admit();
        let mut state = self.lock();
        let (index, slot) = state
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| !slot.reserved && slot.resource.is_none())
            .ok_or(NamespaceError::Full)?;
        slot.generation = slot.generation.wrapping_add(1);
        if slot.generation == 0 {
            slot.generation = 1;
        }
        slot.reserved = true;
        Ok(HandleReservation {
            namespace: self,
            handle: Self::encode(index, slot.generation),
            kind,
            admission: Some(admission),
        })
    }

    pub(super) fn publish(&self, handle: Handle, kind: HandleKind, remote: RemoteId) -> Result<(), NamespaceError> {
        let mut state = self.lock();
        let (index, generation) = Self::decode(handle)?;
        let slot = state.slots.get_mut(index).ok_or(NamespaceError::InvalidHandle)?;
        if slot.generation != generation || !slot.reserved || slot.resource.is_some() {
            return Err(NamespaceError::InvalidHandle);
        }
        slot.resource = Some(Resource {
            lease: Arc::new(RemoteLease::new(remote, kind)),
            references: 1,
        });
        slot.reserved = false;
        Ok(())
    }

    pub(super) fn cancel(&self, handle: Handle) {
        let Ok((index, generation)) = Self::decode(handle) else {
            return;
        };
        let mut state = self.lock();
        if let Some(slot) = state.slots.get_mut(index)
            && slot.generation == generation
            && slot.reserved
            && slot.resource.is_none()
        {
            slot.reserved = false;
        }
    }

    pub fn resolve(&self, handle: Handle, expected: HandleKind) -> Result<RemoteId, NamespaceError> {
        let state = self.lock();
        let resource = Self::resource(&state, handle)?;
        if resource.lease.kind != expected {
            return Err(NamespaceError::WrongKind);
        }
        Ok(resource.lease.remote)
    }

    /// Clones local ownership. The returned identifier aliases the same slot.
    pub fn clone_handle(&self, handle: Handle) -> Result<Handle, NamespaceError> {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        let resource = Self::resource_mut(&mut state, handle)?;
        resource.references = resource
            .references
            .checked_add(1)
            .ok_or(NamespaceError::ReferenceLimit)?;
        Ok(handle)
    }

    /// Releases one owner and returns the remote close obligation exactly once.
    pub fn close(&self, handle: Handle) -> Result<Option<Close>, NamespaceError> {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        let resource = Self::resource_mut(&mut state, handle)?;
        resource.references -= 1;
        if resource.references != 0 {
            return Ok(None);
        }
        let resource = state.slots[Self::decode(handle)?.0]
            .resource
            .take()
            .ok_or(NamespaceError::InvalidHandle)?;
        Ok(resource.lease.release())
    }

    /// Moves a sole-owned resource into a pointer-free capability.
    pub fn transfer(&self, handle: Handle) -> Result<TransferCapability, NamespaceError> {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        let index = Self::decode(handle)?.0;
        let resource = Self::resource(&state, handle)?.clone();
        if resource.references != 1 || !resource.lease.is_sole() {
            return Err(NamespaceError::SharedTransfer);
        }
        state.slots[index].resource = None;
        Ok(TransferCapability {
            remote: resource.lease.remote,
            kind: resource.lease.kind,
        })
    }

    pub fn accept(&self, capability: TransferCapability) -> Result<Handle, (NamespaceError, TransferCapability)> {
        match self.open(capability.remote, capability.kind) {
            Ok(handle) => Ok(handle),
            Err(error) => Err((error, capability)),
        }
    }

    /// Revokes every live identifier and returns all final close obligations.
    pub fn revoke(&self) -> Vec<Close> {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        state
            .slots
            .iter_mut()
            .filter_map(|slot| slot.resource.take().and_then(|resource| resource.lease.release()))
            .collect()
    }

    pub fn begin_fork(&self) -> Result<NamespaceForkPlan, NamespaceError> {
        self.begin_fork_bounded(usize::MAX)
    }

    pub fn rebind_fork(&self, snapshot: &NamespaceSnapshot) -> Result<NamespaceForkPlan, NamespaceError> {
        let plan = self.begin_fork_bounded(snapshot.capacity)?;
        if plan.snapshot() != *snapshot {
            plan.rollback();
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(plan)
    }

    pub fn begin_fork_bounded(&self, maximum_entries: usize) -> Result<NamespaceForkPlan, NamespaceError> {
        let _admission = self.activity.admit();
        let state = self.lock();
        if state.slots.iter().any(|slot| slot.reserved) {
            return Err(NamespaceError::Busy);
        }
        let mut slots = Vec::with_capacity(state.slots.len());
        let mut acquired = 0;
        for slot in &state.slots {
            let resource = match Self::fork_resource(slot, acquired, maximum_entries) {
                Ok(resource) => resource,
                Err(error) => {
                    Self::release_slots(&mut slots);
                    return Err(error);
                }
            };
            acquired += usize::from(resource.is_some());
            slots.push(Slot {
                generation: slot.generation,
                reserved: false,
                resource,
            });
        }
        Ok(NamespaceForkPlan {
            child: Some(Self {
                state: Mutex::new(State { slots }),
                activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
            }),
        })
    }

    pub fn snapshot(&self) -> NamespaceSnapshot {
        let _admission = self.activity.admit();
        self.snapshot_state()
    }

    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.lock());
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    pub fn checkpoint_snapshot(&self) -> Result<NamespaceSnapshot, NamespaceError> {
        if !self.activity.frozen() {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(self.snapshot_state())
    }

    fn snapshot_state(&self) -> NamespaceSnapshot {
        let state = self.lock();
        let entries: Vec<_> = state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, value)| {
                value.resource.as_ref().map(|resource| SnapshotEntry {
                    slot,
                    generation: value.generation,
                    remote: resource.lease.remote,
                    kind: resource.lease.kind,
                    references: resource.references,
                })
            })
            .collect();
        NamespaceSnapshot {
            capacity: state.slots.len(),
            live: entries.len(),
            references: entries.iter().map(|entry| u64::from(entry.references)).sum(),
            generations: state.slots.iter().map(|slot| slot.generation).collect(),
            entries,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn release_all(self) {
        let state = self
            .state
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut slots = state.slots;
        Self::release_slots(&mut slots);
    }

    fn release_slots(slots: &mut [Slot]) {
        for slot in slots {
            if let Some(resource) = slot.resource.take() {
                let close = resource.lease.release();
                debug_assert!(close.is_none(), "a fork child cannot own the final parent lease");
            }
        }
    }

    fn fork_resource(slot: &Slot, acquired: usize, maximum: usize) -> Result<Option<Resource>, NamespaceError> {
        let Some(resource) = &slot.resource else {
            return Ok(None);
        };
        if acquired == maximum {
            return Err(NamespaceError::ForkLimit);
        }
        resource.lease.acquire()?;
        Ok(Some(resource.clone()))
    }

    fn encode(index: usize, generation: u16) -> Handle {
        Handle(TAG | u64::from(generation) << 32 | (index as u64 + 1))
    }

    fn decode(handle: Handle) -> Result<(usize, u16), NamespaceError> {
        if handle.0 & TAG_MASK != TAG {
            return Err(NamespaceError::InvalidHandle);
        }
        let raw = (handle.0 & u64::from(u32::MAX)) as u32;
        let generation = (handle.0 >> 32) as u16;
        if raw == 0 || generation == 0 {
            return Err(NamespaceError::InvalidHandle);
        }
        Ok((raw as usize - 1, generation))
    }

    fn resource(state: &State, handle: Handle) -> Result<&Resource, NamespaceError> {
        let (index, generation) = Self::decode(handle)?;
        let slot = state.slots.get(index).ok_or(NamespaceError::InvalidHandle)?;
        if slot.generation != generation {
            return Err(NamespaceError::InvalidHandle);
        }
        slot.resource.as_ref().ok_or(NamespaceError::InvalidHandle)
    }

    fn resource_mut(state: &mut State, handle: Handle) -> Result<&mut Resource, NamespaceError> {
        let (index, generation) = Self::decode(handle)?;
        let slot = state.slots.get_mut(index).ok_or(NamespaceError::InvalidHandle)?;
        if slot.generation != generation {
            return Err(NamespaceError::InvalidHandle);
        }
        slot.resource.as_mut().ok_or(NamespaceError::InvalidHandle)
    }
}
