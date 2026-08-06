use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_execution::{ExecutionMachine, ExecutionSnapshot};

use crate::{ForkContext, ForkParticipant, ForkParticipantRole};

struct ExecutionForkReservation {
    reservation: u64,
    previous: ExecutionSnapshot,
    parent: Option<ExecutionSnapshot>,
    child: Option<Arc<ExecutionMachine>>,
    committed: bool,
}

struct ExecutionForkState {
    next: u64,
    staged: BTreeMap<u64, ExecutionForkReservation>,
    children: BTreeMap<u64, Arc<ExecutionMachine>>,
}

pub struct ExecutionForkParticipant {
    parent: Arc<ExecutionMachine>,
    state: Mutex<ExecutionForkState>,
}

impl ExecutionForkParticipant {
    #[must_use]
    pub fn new(parent: Arc<ExecutionMachine>) -> Self {
        Self {
            parent,
            state: Mutex::new(ExecutionForkState {
                next: 1,
                staged: BTreeMap::new(),
                children: BTreeMap::new(),
            }),
        }
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<ExecutionMachine>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.staged.remove(&transaction);
        state.children.remove(&transaction)
    }

    pub fn child(&self, transaction: u64) -> Option<Arc<ExecutionMachine>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .children
            .get(&transaction)
            .cloned()
    }
}

impl ForkParticipant for ExecutionForkParticipant {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Execution
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        self.parent.freeze().map_err(|_| ())?;
        let Ok(previous) = self.parent.snapshot() else {
            let _ = self.parent.thaw();
            return Err(());
        };
        let mut state = self.state.lock().map_err(|_| ())?;
        let reservation = state.next;
        state.next = state.next.checked_add(1).ok_or(())?;
        state.staged.insert(
            context.transaction,
            ExecutionForkReservation {
                reservation,
                previous,
                parent: None,
                child: None,
                committed: false,
            },
        );
        Ok(reservation)
    }

    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_parent(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        let staged = state.staged.get_mut(&context.transaction).ok_or(())?;
        if staged.reservation != reservation || staged.parent.is_some() {
            return Err(());
        }
        staged.parent = Some(staged.previous.fork_parent().map_err(|_| ())?);
        Ok(())
    }

    fn clone_child(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        let staged = state.staged.get_mut(&context.transaction).ok_or(())?;
        if staged.reservation != reservation || staged.child.is_some() {
            return Err(());
        }
        staged.child = Some(Arc::new(
            ExecutionMachine::new(staged.previous.fork_child().map_err(|_| ())?).map_err(|_| ())?,
        ));
        Ok(())
    }

    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn commit(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        let staged = state.staged.get_mut(&context.transaction).ok_or(())?;
        if staged.reservation != reservation || staged.committed {
            return Err(());
        }
        let parent = staged.parent.take().ok_or(())?;
        self.parent.replace(parent).map_err(|_| ())?;
        staged.committed = true;
        let child = staged.child.take().ok_or(())?;
        self.parent.thaw().map_err(|_| ())?;
        state.children.insert(context.transaction, child);
        Ok(())
    }

    fn rollback(&self, context: ForkContext, _: u64) {
        let staged = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staged
            .remove(&context.transaction);
        if let Some(staged) = staged {
            if staged.committed {
                let _ = self.parent.replace(staged.previous);
            }
            let _ = self.parent.thaw();
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .children
            .remove(&context.transaction);
    }
}
