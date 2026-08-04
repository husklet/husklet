use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::context::Context;
use crate::{Admission, AioError, ContextId, EventBatch};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogLimits {
    pub contexts: usize,
    pub events_per_context: usize,
}

impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            contexts: 64,
            events_per_context: 65_536,
        }
    }
}

struct Slot {
    generation: u32,
    context: Option<Arc<Context>>,
}

pub struct Catalog {
    slots: Mutex<Vec<Slot>>,
    limits: CatalogLimits,
}

impl Catalog {
    #[must_use]
    pub fn new(limits: CatalogLimits) -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            limits,
        }
    }

    pub fn create(&self, capacity: usize) -> Result<ContextId, AioError> {
        if capacity == 0 || capacity > self.limits.events_per_context {
            return Err(AioError::InvalidArgument);
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slot = if let Some(slot) = slots.iter().position(|slot| slot.context.is_none()) {
            slot
        } else {
            if slots.len() == self.limits.contexts {
                return Err(AioError::ResourceLimit);
            }
            slots.push(Slot {
                generation: 0,
                context: None,
            });
            slots.len() - 1
        };
        let generation = slots[slot].generation.wrapping_add(1).max(1);
        slots[slot].generation = generation;
        slots[slot].context = Some(Arc::new(Context::new(capacity)));
        Ok(ContextId::new(slot as u32, generation))
    }

    pub fn admit(&self, id: ContextId) -> Result<Admission, AioError> {
        self.context(id)?.admit()
    }

    pub fn stage(
        &self,
        id: ContextId,
        minimum: usize,
        maximum: usize,
        timeout: Option<Duration>,
        interrupted: impl Fn() -> bool,
    ) -> Result<EventBatch, AioError> {
        self.context(id)?.stage(minimum, maximum, timeout, interrupted)
    }

    pub fn destroy(&self, id: ContextId) -> Result<(), AioError> {
        let context = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            let (slot, generation) = id.parts().ok_or(AioError::InvalidArgument)?;
            let entry = slots.get_mut(slot).ok_or(AioError::InvalidArgument)?;
            if entry.generation != generation {
                return Err(AioError::InvalidArgument);
            }
            entry.context.take().ok_or(AioError::InvalidArgument)?
        };
        context.close();
        Ok(())
    }

    pub fn wake(&self, id: ContextId) -> Result<(), AioError> {
        self.context(id)?.wake();
        Ok(())
    }

    fn context(&self, id: ContextId) -> Result<Arc<Context>, AioError> {
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let (slot, generation) = id.parts().ok_or(AioError::InvalidArgument)?;
        let entry = slots.get(slot).ok_or(AioError::InvalidArgument)?;
        if entry.generation != generation {
            return Err(AioError::InvalidArgument);
        }
        entry.context.clone().ok_or(AioError::InvalidArgument)
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new(CatalogLimits::default())
    }
}
