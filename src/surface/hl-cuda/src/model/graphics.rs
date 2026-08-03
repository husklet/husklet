use super::device::DevicePtr;
use hl_gpu::{BufferId, ExportId};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphicsResource(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsBuffer {
    pub buffer: BufferId,
    pub export: ExportId,
    pub pointer: DevicePtr,
    pub bytes: u64,
    pub mapped: bool,
}

#[derive(Default)]
pub struct GraphicsResources {
    next: u64,
    entries: HashMap<GraphicsResource, GraphicsBuffer>,
}

impl GraphicsResources {
    pub fn new() -> Self {
        Self { next: 1, entries: HashMap::new() }
    }

    pub fn insert(&mut self, buffer: GraphicsBuffer) -> GraphicsResource {
        let resource = GraphicsResource(self.next);
        self.next += 1;
        self.entries.insert(resource, buffer);
        resource
    }

    pub fn get(&self, resource: GraphicsResource) -> Option<&GraphicsBuffer> {
        self.entries.get(&resource)
    }

    pub fn get_mut(&mut self, resource: GraphicsResource) -> Option<&mut GraphicsBuffer> {
        self.entries.get_mut(&resource)
    }

    pub fn remove(&mut self, resource: GraphicsResource) -> Option<GraphicsBuffer> {
        self.entries.remove(&resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(id: u32) -> GraphicsBuffer {
        GraphicsBuffer { buffer: BufferId(id), export: ExportId(id as u64), pointer: DevicePtr(id as u64), bytes: 4, mapped: false }
    }

    #[test]
    fn retired_handles_are_never_reused() {
        let mut resources = GraphicsResources::new();
        let first = resources.insert(buffer(1));
        resources.remove(first).unwrap();
        let second = resources.insert(buffer(2));
        assert_ne!(first, second);
        assert!(resources.get(first).is_none());
    }
}
