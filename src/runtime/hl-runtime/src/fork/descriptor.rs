use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_task::ForkCloneFlags;

use crate::{Control, ForkContext, ForkParticipant, ForkParticipantRole, RuntimeDescriptorTable};

struct DescriptorForkState {
    next_reservation: u64,
    staged: BTreeMap<u64, (u64, Option<Arc<RuntimeDescriptorTable>>)>,
    committed: BTreeMap<u64, Arc<RuntimeDescriptorTable>>,
}

pub struct DescriptorForkParticipant {
    control: Arc<Control>,
    parent: Arc<RuntimeDescriptorTable>,
    state: Mutex<DescriptorForkState>,
}

impl DescriptorForkParticipant {
    pub fn new(control: Arc<Control>, parent: Arc<RuntimeDescriptorTable>) -> Self {
        Self {
            control,
            parent,
            state: Mutex::new(DescriptorForkState {
                next_reservation: 1,
                staged: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
        }
    }

    pub fn child(&self, transaction: u64) -> Option<Arc<RuntimeDescriptorTable>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .committed
            .get(&transaction)
            .cloned()
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<RuntimeDescriptorTable>> {
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

impl ForkParticipant for DescriptorForkParticipant {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Descriptors
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
        let child = if context.request.flags.contains(ForkCloneFlags::FILES) {
            self.control.share(&self.parent)
        } else {
            self.control.fork(&self.parent)
        };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.staged.get_mut(&context.transaction).ok_or(())?;
        if entry.0 != reservation || entry.1.is_some() {
            return Err(());
        }
        entry.1 = Some(Arc::new(child));
        Ok(())
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
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
