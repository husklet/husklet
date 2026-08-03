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
    pub map_state: GraphicsMapState,
    pub map_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsMapState { Unmapped, Mapped, Poisoned }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsObject {
    Buffer(GraphicsBuffer),
}

#[derive(Default)]
pub struct GraphicsResources {
    next: u64,
    entries: HashMap<GraphicsResource, GraphicsObject>,
}

impl GraphicsResources {
    pub fn new() -> Self {
        Self { next: 1, entries: HashMap::new() }
    }

    pub fn insert(&mut self, buffer: GraphicsBuffer) -> GraphicsResource {
        let resource = GraphicsResource(self.next);
        self.next += 1;
        self.entries.insert(resource, GraphicsObject::Buffer(buffer));
        resource
    }

    pub fn get(&self, resource: GraphicsResource) -> Option<&GraphicsBuffer> {
        match self.entries.get(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer) }
    }

    pub fn get_mut(&mut self, resource: GraphicsResource) -> Option<&mut GraphicsBuffer> {
        match self.entries.get_mut(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer) }
    }

    pub fn remove(&mut self, resource: GraphicsResource) -> Option<GraphicsBuffer> {
        match self.entries.remove(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer) }
    }

    pub fn object(&self, resource: GraphicsResource) -> Option<&GraphicsObject> { self.entries.get(&resource) }
    pub fn object_mut(&mut self, resource: GraphicsResource) -> Option<&mut GraphicsObject> { self.entries.get_mut(&resource) }
    pub fn remove_object(&mut self, resource: GraphicsResource) -> Option<GraphicsObject> { self.entries.remove(&resource) }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(id: u32) -> GraphicsBuffer {
        GraphicsBuffer { buffer: BufferId(id), export: ExportId(id as u64), pointer: DevicePtr(id as u64), bytes: 4, map_state: GraphicsMapState::Unmapped, map_flags: 0 }
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
