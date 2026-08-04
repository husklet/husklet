use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_event::EventCatalog;
use hl_execution::ExecutionMachine;
use hl_memory::{MappingCoordinator, MappingHost};
use hl_network::NetworkCatalog;
use hl_provider::HandleNamespace;
use hl_task::{ProcessId, ThreadId};

use crate::IpcForkChild;
use crate::RuntimeDescriptorTable;

pub struct ChildResources<H: MappingHost> {
    pub process: ProcessId,
    pub thread: ThreadId,
    pub descriptors: Arc<RuntimeDescriptorTable>,
    pub memory: Arc<MappingCoordinator<H>>,
    pub providers: Arc<HandleNamespace>,
    pub execution: Arc<ExecutionMachine>,
    pub network: Arc<NetworkCatalog>,
    pub event: Arc<EventCatalog>,
    pub ipc: Arc<IpcForkChild<H>>,
}

enum ChildResourceSlot<H: MappingHost> {
    Reserved,
    Published(Arc<ChildResources<H>>),
}

pub struct ChildResourceCatalog<H: MappingHost> {
    capacity: usize,
    children: Arc<Mutex<BTreeMap<ProcessId, ChildResourceSlot<H>>>>,
}

impl<H: MappingHost> ChildResourceCatalog<H> {
    pub fn new(capacity: usize) -> Result<Self, ChildResourceError> {
        if capacity == 0 {
            return Err(ChildResourceError::Capacity);
        }
        Ok(Self {
            capacity,
            children: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn prepare(&self, process: ProcessId) -> Result<PreparedChildResources<H>, ChildResourceError> {
        let mut children = self.children.lock().unwrap_or_else(|error| error.into_inner());
        if children.contains_key(&process) {
            return Err(ChildResourceError::Exists);
        }
        if children.len() >= self.capacity {
            return Err(ChildResourceError::Capacity);
        }
        children.insert(process, ChildResourceSlot::Reserved);
        Ok(PreparedChildResources {
            process,
            children: Arc::clone(&self.children),
            finished: false,
        })
    }

    pub fn take(&self, process: ProcessId) -> Option<Arc<ChildResources<H>>> {
        let slot = self
            .children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&process)?;
        match slot {
            ChildResourceSlot::Published(resources) => Some(resources),
            ChildResourceSlot::Reserved => None,
        }
    }

    pub fn child(&self, process: ProcessId) -> Option<Arc<ChildResources<H>>> {
        let children = self.children.lock().unwrap_or_else(|error| error.into_inner());
        match children.get(&process)? {
            ChildResourceSlot::Published(resources) => Some(Arc::clone(resources)),
            ChildResourceSlot::Reserved => None,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.children.lock().unwrap_or_else(|error| error.into_inner()).len()
    }
}

pub struct PreparedChildResources<H: MappingHost> {
    process: ProcessId,
    children: Arc<Mutex<BTreeMap<ProcessId, ChildResourceSlot<H>>>>,
    finished: bool,
}

impl<H: MappingHost> PreparedChildResources<H> {
    pub fn stage(mut self, resources: ChildResources<H>) -> Result<ReadyChildResources<H>, ChildResourceError> {
        if resources.process != self.process {
            return Err(ChildResourceError::Identity);
        }
        let children = Arc::clone(&self.children);
        {
            let guard = children.lock().unwrap_or_else(|error| error.into_inner());
            if !matches!(guard.get(&self.process), Some(ChildResourceSlot::Reserved)) {
                return Err(ChildResourceError::Stale);
            }
        }
        self.finished = true;
        Ok(ReadyChildResources {
            process: self.process,
            children,
            resources: Some(resources),
        })
    }

    pub fn publish(mut self, resources: ChildResources<H>) -> Result<(), ChildResourceError> {
        if resources.process != self.process {
            return Err(ChildResourceError::Identity);
        }
        let mut children = self.children.lock().unwrap_or_else(|error| error.into_inner());
        let slot = children.get_mut(&self.process).ok_or(ChildResourceError::Stale)?;
        if !matches!(slot, ChildResourceSlot::Reserved) {
            return Err(ChildResourceError::Stale);
        }
        *slot = ChildResourceSlot::Published(Arc::new(resources));
        self.finished = true;
        Ok(())
    }
}

pub struct ReadyChildResources<H: MappingHost> {
    process: ProcessId,
    children: Arc<Mutex<BTreeMap<ProcessId, ChildResourceSlot<H>>>>,
    resources: Option<ChildResources<H>>,
}

impl<H: MappingHost> ReadyChildResources<H> {
    /// Publishes a previously identity-checked exclusive reservation.
    pub fn publish(mut self) {
        let resources = self.resources.take().expect("staged child resources");
        let mut children = self.children.lock().unwrap_or_else(|error| error.into_inner());
        let slot = children.get_mut(&self.process).expect("exclusive child reservation");
        debug_assert!(matches!(slot, ChildResourceSlot::Reserved));
        *slot = ChildResourceSlot::Published(Arc::new(resources));
    }
}

impl<H: MappingHost> Drop for ReadyChildResources<H> {
    fn drop(&mut self) {
        if self.resources.is_some() {
            self.children
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.process);
        }
    }
}

impl<H: MappingHost> Drop for PreparedChildResources<H> {
    fn drop(&mut self) {
        if !self.finished {
            self.children
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.process);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildResourceError {
    Capacity,
    Exists,
    Identity,
    Stale,
}
