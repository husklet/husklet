use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_ipc::{CommittedSemaphoreFork, IpcCatalog, PreparedSemaphoreFork};
use hl_memory::MappingHost;

use crate::ipc::{OwnedCommittedFork, OwnedPreparedFork};
use crate::{
    ForkArtifactExchange, ForkContext, ForkParticipant, ForkParticipantRole, MemoryChildMapping, MemoryLifecycle,
    MemoryMappings,
};

pub struct IpcForkChild<H: MappingHost> {
    pub catalog: Arc<IpcCatalog>,
    pub memory: Arc<hl_memory::MappingCoordinator<H>>,
    pub mappings: Arc<MemoryMappings<H>>,
}

struct PreparedIpcFork<H: MappingHost> {
    reservation: u64,
    child: Arc<IpcForkChild<H>>,
    shared: OwnedPreparedFork,
    semaphores: PreparedSemaphoreFork,
}

struct CommittedIpcFork<H: MappingHost> {
    child: Arc<IpcForkChild<H>>,
    shared: OwnedCommittedFork,
    semaphores: CommittedSemaphoreFork,
}

struct IpcForkState<H: MappingHost> {
    next: u64,
    reservations: BTreeMap<u64, u64>,
    prepared: BTreeMap<u64, PreparedIpcFork<H>>,
    committed: BTreeMap<u64, CommittedIpcFork<H>>,
}

pub struct IpcForkParticipant<H: MappingHost> {
    catalog: Arc<IpcCatalog>,
    shared: MemoryLifecycle,
    state: Mutex<IpcForkState<H>>,
}

impl<H: MappingHost> IpcForkParticipant<H> {
    #[must_use]
    pub fn new(catalog: Arc<IpcCatalog>, parent_mappings: Arc<dyn crate::MemoryPort>) -> Self {
        Self {
            shared: MemoryLifecycle::new(Arc::clone(&catalog), parent_mappings),
            catalog,
            state: Mutex::new(IpcForkState {
                next: 1,
                reservations: BTreeMap::new(),
                prepared: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
        }
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<IpcForkChild<H>>> {
        let committed = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
            .remove(&transaction)?;
        committed.shared.finish();
        committed.semaphores.finish();
        Some(committed.child)
    }

    #[must_use]
    pub fn child(&self, transaction: u64) -> Option<Arc<IpcForkChild<H>>> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
            .get(&transaction)
            .map(|committed| Arc::clone(&committed.child))
    }

    pub(crate) fn actor(slot: u32) -> Result<u32, ()> {
        slot.checked_add(1).ok_or(())
    }
}

impl<H: MappingHost + 'static> ForkParticipant for IpcForkParticipant<H> {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Ipc
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.prepared.contains_key(&context.transaction)
            || state.reservations.contains_key(&context.transaction)
            || state.committed.contains_key(&context.transaction)
        {
            return Err(());
        }
        let reservation = state.next;
        state.next = state.next.checked_add(1).ok_or(())?;
        state.reservations.insert(context.transaction, reservation);
        Ok(reservation)
    }

    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Err(())
    }

    fn clone_with_artifacts(
        &self,
        context: ForkContext,
        reservation: u64,
        artifacts: &ForkArtifactExchange,
    ) -> Result<(), ()> {
        let memory = artifacts
            .get::<MemoryChildMapping<H>>(context, ForkParticipantRole::Memory)
            .ok_or(())?;
        let mappings = Arc::new(MemoryMappings::new(Arc::clone(&memory.0)));
        let shared = self
            .shared
            .prepare_owned_fork(
                Self::actor(context.request.parent.slot)?,
                Self::actor(context.request.child.slot)?,
                context.transaction,
                &mappings,
            )
            .map_err(|_| ())?;
        let semaphores = self
            .catalog
            .semaphores()
            .prepare_fork_child(Self::actor(context.request.child.slot)?);
        let child = Arc::new(IpcForkChild {
            catalog: Arc::clone(&self.catalog),
            memory: Arc::clone(&memory.0),
            mappings,
        });
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.reservations.remove(&context.transaction) != Some(reservation) {
            return Err(());
        }
        if state
            .prepared
            .insert(
                context.transaction,
                PreparedIpcFork {
                    reservation,
                    child,
                    shared,
                    semaphores,
                },
            )
            .is_some()
        {
            return Err(());
        }
        Ok(())
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn commit(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let prepared = {
            let mut state = self.state.lock().map_err(|_| ())?;
            let prepared = state.prepared.remove(&context.transaction).ok_or(())?;
            if prepared.reservation != reservation {
                state.prepared.insert(context.transaction, prepared);
                return Err(());
            }
            prepared
        };
        let shared = prepared.shared.commit().map_err(|_| ())?;
        let semaphores = match prepared.semaphores.commit() {
            Ok(committed) => committed,
            Err(_) => {
                shared.rollback().map_err(|_| ())?;
                return Err(());
            }
        };
        let committed = CommittedIpcFork {
            child: prepared.child,
            shared,
            semaphores,
        };
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.committed.insert(context.transaction, committed).is_some() {
            return Err(());
        }
        Ok(())
    }

    fn rollback(&self, context: ForkContext, _: u64) {
        let committed = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.reservations.remove(&context.transaction);
            state.prepared.remove(&context.transaction);
            state.committed.remove(&context.transaction)
        };
        if let Some(committed) = committed {
            let _ = committed.semaphores.rollback();
            let _ = committed.shared.rollback();
        }
    }
}
