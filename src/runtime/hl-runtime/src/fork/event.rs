use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_event::EventCatalog;

use crate::{ForkContext, ForkParticipant, ForkParticipantRole};

struct EventForkReservation {
    reservation: u64,
    child: Option<Arc<EventCatalog>>,
}

struct EventForkState {
    next: u64,
    staged: BTreeMap<u64, EventForkReservation>,
    children: BTreeMap<u64, Arc<EventCatalog>>,
}

/// Publishes the event catalog identity already retained by shared event OFDs.
///
/// Descriptor fork shares each `CatalogBoundEvent` open-file description.
/// Sharing this catalog preserves its generation-qualified object IDs and the
/// eventfd, timerfd, signalfd, inotify, and epoll subscription state behind
/// those OFDs.
pub struct EventForkParticipant {
    parent: Arc<EventCatalog>,
    state: Mutex<EventForkState>,
}

impl EventForkParticipant {
    #[must_use]
    pub fn new(parent: Arc<EventCatalog>) -> Self {
        Self {
            parent,
            state: Mutex::new(EventForkState {
                next: 1,
                staged: BTreeMap::new(),
                children: BTreeMap::new(),
            }),
        }
    }

    pub fn take_child(&self, transaction: u64) -> Option<Arc<EventCatalog>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .children
            .remove(&transaction)
    }

    #[must_use]
    pub fn child(&self, transaction: u64) -> Option<Arc<EventCatalog>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .children
            .get(&transaction)
            .cloned()
    }

    #[cfg(test)]
    pub(crate) fn staged_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staged
            .len()
    }
}

impl ForkParticipant for EventForkParticipant {
    fn role(&self) -> ForkParticipantRole {
        ForkParticipantRole::Event
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.staged.contains_key(&context.transaction) || state.children.contains_key(&context.transaction) {
            return Err(());
        }
        let reservation = state.next;
        state.next = state.next.checked_add(1).ok_or(())?;
        state.staged.insert(
            context.transaction,
            EventForkReservation {
                reservation,
                child: None,
            },
        );
        Ok(reservation)
    }

    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }

    fn clone_child(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        let staged = state.staged.get_mut(&context.transaction).ok_or(())?;
        if staged.reservation != reservation || staged.child.is_some() {
            return Err(());
        }
        staged.child = Some(Arc::clone(&self.parent));
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
        let staged = state.staged.remove(&context.transaction).ok_or(())?;
        if staged.reservation != reservation {
            state.staged.insert(context.transaction, staged);
            return Err(());
        }
        let child = staged.child.ok_or(())?;
        if state.children.insert(context.transaction, child).is_some() {
            return Err(());
        }
        Ok(())
    }

    fn rollback(&self, context: ForkContext, _: u64) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.staged.remove(&context.transaction);
        state.children.remove(&context.transaction);
    }
}
