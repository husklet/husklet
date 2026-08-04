use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_provider::{Close, HandleNamespace, NamespaceForkPlan};

use crate::{ForkContext, ForkParticipant, ForkParticipantRole};

struct ProviderForkState {
    next_reservation: u64,
    staged: BTreeMap<u64, (u64, Option<NamespaceForkPlan>)>,
    committed: BTreeMap<u64, Arc<HandleNamespace>>,
}

pub struct ProviderForkParticipant {
    parent: Arc<HandleNamespace>,
    state: Mutex<ProviderForkState>,
}

impl ProviderForkParticipant {
    pub fn new(parent: Arc<HandleNamespace>) -> Self {
        Self {
            parent,
            state: Mutex::new(ProviderForkState {
                next_reservation: 1,
                staged: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
        }
    }

    pub fn child(&self, transaction: u64) -> Option<Arc<HandleNamespace>> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
            .get(&transaction)
            .cloned()
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<HandleNamespace>> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
            .remove(&transaction)
    }

    pub fn exit_child(&self, transaction: u64) -> Vec<Close> {
        let child = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
            .remove(&transaction);
        child.map_or_else(Vec::new, |namespace| namespace.revoke())
    }

    pub const fn exec_child(&self, _transaction: u64) {
        // Provider handles have no descriptor-local CLOEXEC bit. Descriptor
        // exec sweep retires projected OFDs, which then close their handles.
    }

    #[must_use]
    pub fn staged_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .staged
            .len()
    }
}

impl ForkParticipant for ProviderForkParticipant {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Provider
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        let plan = self.parent.begin_fork().map_err(|_| ())?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = state.staged.get_mut(&context.transaction).ok_or(())?;
        if entry.0 != reservation || entry.1.is_some() {
            return Err(());
        }
        entry.1 = Some(plan);
        Ok(())
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn commit(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (actual, plan) = state.staged.remove(&context.transaction).ok_or(())?;
        if actual != reservation {
            return Err(());
        }
        let child = Arc::new(plan.ok_or(())?.commit());
        state.committed.insert(context.transaction, child);
        Ok(())
    }

    fn rollback(&self, context: ForkContext, _: u64) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.staged.remove(&context.transaction);
        state.committed.remove(&context.transaction);
    }
}
