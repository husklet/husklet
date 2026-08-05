use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_memory::{MappingCoordinator, MappingHost};
use hl_task::ForkCloneFlags;

use crate::{ForkArtifactExchange, ForkContext, ForkParticipant, ForkParticipantRole};

pub struct MemoryChildMapping<H: MappingHost>(pub Arc<MappingCoordinator<H>>);

pub trait MemoryForkHost<H>: Send + Sync {
    fn child_host(&self, context: ForkContext) -> Result<H, ()>;
}

impl<H, T: MemoryForkHost<H> + ?Sized> MemoryForkHost<H> for Arc<T> {
    fn child_host(&self, context: ForkContext) -> Result<H, ()> {
        (**self).child_host(context)
    }
}

pub trait PrivateFutexReset: Send + Sync {
    fn reset_private_futexes(&self, context: ForkContext) -> Result<(), ()>;
}

impl<T: PrivateFutexReset + ?Sized> PrivateFutexReset for Arc<T> {
    fn reset_private_futexes(&self, context: ForkContext) -> Result<(), ()> {
        (**self).reset_private_futexes(context)
    }
}

struct MemoryForkState<H> {
    next_reservation: u64,
    staged: BTreeMap<u64, (u64, Option<Arc<MappingCoordinator<H>>>)>,
    committed: BTreeMap<u64, Arc<MappingCoordinator<H>>>,
}

pub struct MemoryForkParticipant<H, F, R> {
    parent: Arc<MappingCoordinator<H>>,
    host: F,
    reset: R,
    state: Mutex<MemoryForkState<H>>,
}

impl<H, F, R> MemoryForkParticipant<H, F, R> {
    pub fn new(parent: Arc<MappingCoordinator<H>>, host: F, reset: R) -> Self {
        Self {
            parent,
            host,
            reset,
            state: Mutex::new(MemoryForkState {
                next_reservation: 1,
                staged: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
        }
    }

    pub fn child(&self, transaction: u64) -> Option<Arc<MappingCoordinator<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .committed
            .get(&transaction)
            .cloned()
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<MappingCoordinator<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .committed
            .remove(&transaction)
    }

    #[must_use]
    pub fn staged_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staged
            .len()
    }
}

impl<H, F, R> ForkParticipant for MemoryForkParticipant<H, F, R>
where
    H: MappingHost + 'static,
    F: MemoryForkHost<H> + 'static,
    R: PrivateFutexReset + 'static,
{
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Memory
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let reservation = state.next_reservation;
        state.next_reservation = state.next_reservation.checked_add(1).ok_or(())?;
        state.staged.insert(context.transaction, (reservation, None));
        Ok(reservation)
    }

    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_child(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.clone_child_mapping(context, reservation).map(|_| ())
    }

    fn clone_with_artifacts(
        &self,
        context: ForkContext,
        reservation: u64,
        artifacts: &ForkArtifactExchange,
    ) -> Result<(), ()> {
        let child = self.clone_child_mapping(context, reservation)?;
        artifacts.publish(
            context,
            ForkParticipantRole::Memory,
            reservation,
            Arc::new(MemoryChildMapping(child)),
        )
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn repair_child(&self, context: ForkContext, _: u64) -> Result<(), ()> {
        if !context.request.flags.contains(ForkCloneFlags::VM) {
            self.reset.reset_private_futexes(context)?;
        }
        Ok(())
    }

    fn commit(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (actual, child) = state.staged.remove(&context.transaction).ok_or(())?;
        if actual != reservation {
            return Err(());
        }
        state.committed.insert(context.transaction, child.ok_or(())?);
        Ok(())
    }

    fn rollback(&self, context: ForkContext, _: u64) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.staged.remove(&context.transaction);
        state.committed.remove(&context.transaction);
    }
}

impl<H, F, R> MemoryForkParticipant<H, F, R>
where
    H: MappingHost + 'static,
    F: MemoryForkHost<H> + 'static,
    R: PrivateFutexReset + 'static,
{
    fn clone_child_mapping(&self, context: ForkContext, reservation: u64) -> Result<Arc<MappingCoordinator<H>>, ()> {
        let child = if context.request.flags.contains(ForkCloneFlags::VM) {
            self.parent.clone()
        } else {
            let host = self.host.child_host(context)?;
            Arc::new(self.parent.fork_restore(host).map_err(|_| ())?)
        };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.staged.get_mut(&context.transaction).ok_or(())?;
        if entry.0 != reservation || entry.1.is_some() {
            return Err(());
        }
        entry.1 = Some(Arc::clone(&child));
        Ok(child)
    }
}
