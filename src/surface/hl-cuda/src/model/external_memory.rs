use std::collections::HashMap;

use hl_gpu::{BufferId, ExportId};

use super::device::DevicePtr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalMemory(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    None,
    Live(DevicePtr),
    Freed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportedMemory {
    pub buffer: BufferId,
    pub export: ExportId,
    pub bytes: u64,
    pub mapping: Mapping,
}

pub struct ExternalMemories {
    next: u64,
    live: HashMap<ExternalMemory, ImportedMemory>,
}

impl ExternalMemories {
    pub fn new() -> Self {
        Self {
            next: 1,
            live: HashMap::new(),
        }
    }
    pub fn insert(&mut self, memory: ImportedMemory) -> ExternalMemory {
        let handle = ExternalMemory(self.next);
        self.next += 1;
        self.live.insert(handle, memory);
        handle
    }
    pub fn get(&self, handle: ExternalMemory) -> Option<&ImportedMemory> {
        self.live.get(&handle)
    }
    pub fn get_mut(&mut self, handle: ExternalMemory) -> Option<&mut ImportedMemory> {
        self.live.get_mut(&handle)
    }
    pub fn remove(&mut self, handle: ExternalMemory) -> Option<ImportedMemory> {
        self.live.remove(&handle)
    }
    pub fn release_pointer(&mut self, pointer: DevicePtr) -> bool {
        let Some(memory) = self
            .live
            .values_mut()
            .find(|memory| memory.mapping == Mapping::Live(pointer))
        else {
            return false;
        };
        memory.mapping = Mapping::Freed;
        true
    }
}

impl Default for ExternalMemories {
    fn default() -> Self {
        Self::new()
    }
}
