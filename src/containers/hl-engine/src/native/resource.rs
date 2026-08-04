//! Instance-scoped ownership and RAII publication for native host resources.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub struct HostResourceContext {
    live: AtomicUsize,
}

impl HostResourceContext {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            live: AtomicUsize::new(0),
        })
    }

    #[must_use]
    pub fn reserve(self: &Arc<Self>) -> HostResourceReservation {
        HostResourceReservation {
            context: Arc::clone(self),
        }
    }

    #[cfg(test)]
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }
}

#[must_use = "a native resource reservation must be published or dropped"]
pub struct HostResourceReservation {
    context: Arc<HostResourceContext>,
}

impl HostResourceReservation {
    pub fn publish<R: Send + Sync + 'static>(self, resource: R) -> HostResourceLease {
        self.context.live.fetch_add(1, Ordering::AcqRel);
        HostResourceLease {
            context: self.context,
            resource: Some(Box::new(resource)),
        }
    }
}

pub struct HostResourceLease {
    context: Arc<HostResourceContext>,
    resource: Option<Box<dyn Send + Sync>>,
}

impl HostResourceLease {
    #[must_use]
    pub fn context(&self) -> Arc<HostResourceContext> {
        Arc::clone(&self.context)
    }
}

impl std::fmt::Debug for HostResourceLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostResourceLease")
            .field("context", &Arc::as_ptr(&self.context))
            .finish()
    }
}

impl Drop for HostResourceLease {
    fn drop(&mut self) {
        drop(self.resource.take());
        self.context.live.fetch_sub(1, Ordering::AcqRel);
    }
}
