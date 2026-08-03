use super::device::DevicePtr;
use hl_gpu::{BufferId, ExportId, TextureId};
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
    pub map_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsImage {
    pub texture: TextureId,
    pub export: ExportId,
    pub mapped: bool,
    pub map_flags: u32,
    pub registration_flags: u32,
    pub kind: GraphicsImageKind,
    pub mip_levels: u32,
    pub layers: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsImageKind { D2, Cube, D2Array, D3 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImportedArrayHandle(pub u64);

/// A mapped subresource view that aliases the imported hl-GPU texture; it owns no pixel copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportedArray {
    pub texture: TextureId,
    pub resource: GraphicsResource,
    pub mip: u32,
    pub layer: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsObject {
    Buffer(GraphicsBuffer),
    Image(GraphicsImage),
}

#[derive(Default)]
pub struct GraphicsResources {
    next: u64,
    entries: HashMap<GraphicsResource, GraphicsObject>,
    next_array: u64,
    arrays: HashMap<ImportedArrayHandle, ImportedArray>,
}

impl GraphicsResources {
    pub fn new() -> Self {
        Self { next: 1, entries: HashMap::new(), next_array: 1, arrays: HashMap::new() }
    }

    pub fn insert(&mut self, buffer: GraphicsBuffer) -> GraphicsResource {
        let resource = GraphicsResource(self.next);
        self.next += 1;
        self.entries.insert(resource, GraphicsObject::Buffer(buffer));
        resource
    }

    pub fn insert_image(&mut self, image: GraphicsImage) -> GraphicsResource {
        let resource = GraphicsResource(self.next);
        self.next += 1;
        self.entries.insert(resource, GraphicsObject::Image(image));
        resource
    }

    pub fn get(&self, resource: GraphicsResource) -> Option<&GraphicsBuffer> {
        match self.entries.get(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer), GraphicsObject::Image(_) => None }
    }

    pub fn get_mut(&mut self, resource: GraphicsResource) -> Option<&mut GraphicsBuffer> {
        match self.entries.get_mut(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer), GraphicsObject::Image(_) => None }
    }

    pub fn remove(&mut self, resource: GraphicsResource) -> Option<GraphicsBuffer> {
        match self.entries.remove(&resource)? { GraphicsObject::Buffer(buffer) => Some(buffer), GraphicsObject::Image(image) => { self.entries.insert(resource, GraphicsObject::Image(image)); None } }
    }

    pub fn object(&self, resource: GraphicsResource) -> Option<&GraphicsObject> { self.entries.get(&resource) }
    pub fn object_mut(&mut self, resource: GraphicsResource) -> Option<&mut GraphicsObject> { self.entries.get_mut(&resource) }
    pub fn remove_object(&mut self, resource: GraphicsResource) -> Option<GraphicsObject> { self.entries.remove(&resource) }

    pub fn mapped_array(&mut self, resource: GraphicsResource, mip: u32, layer: u32) -> Option<ImportedArrayHandle> {
        let image = match self.entries.get(&resource)? { GraphicsObject::Image(image) if image.mapped => image, _ => return None };
        if mip >= image.mip_levels || match image.kind { GraphicsImageKind::D2 | GraphicsImageKind::D3 => layer != 0, GraphicsImageKind::Cube | GraphicsImageKind::D2Array => layer >= image.layers } { return None; }
        if let Some((&handle, _)) = self.arrays.iter().find(|(_, array)| array.resource == resource && array.mip == mip && array.layer == layer) { return Some(handle); }
        let handle = ImportedArrayHandle(self.next_array);
        self.next_array += 1;
        self.arrays.insert(handle, ImportedArray { texture: image.texture, resource, mip, layer });
        Some(handle)
    }

    pub fn array(&self, handle: ImportedArrayHandle) -> Option<&ImportedArray> { self.arrays.get(&handle) }

    pub fn invalidate_array(&mut self, resource: GraphicsResource) {
        self.arrays.retain(|_, array| array.resource != resource);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(id: u32) -> GraphicsBuffer {
        GraphicsBuffer { buffer: BufferId(id), export: ExportId(id as u64), pointer: DevicePtr(id as u64), bytes: 4, mapped: false, map_flags: 0 }
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
