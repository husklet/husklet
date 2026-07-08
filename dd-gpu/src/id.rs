//! Typed resource ids (guest-assigned in the IR) + a host-side generational resource table.
//!
//! The guest producer allocates ids from its own monotonically-increasing counters and names them in
//! every command; the host executor keeps the id → live-object mapping. A [`ResourceTable`] gives each
//! backend a uniform way to enforce lifecycle — rejecting a duplicate create, a use-after-free, or a
//! double-free — turning what would be UB on a real driver into a typed [`GpuError`].

use crate::{GpuError, Result};
use std::collections::HashMap;

macro_rules! def_id {
    ($(#[$m:meta])* $name:ident, $kind:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl $name {
            /// Human-readable resource-kind tag, used in error messages and the mock log.
            pub const KIND: &'static str = $kind;
            pub fn raw(self) -> u32 {
                self.0
            }
        }
    };
}

def_id!(/// GPU buffer (vertex/index/uniform/storage, or a CUDA `cudaMalloc` allocation).
    BufferId, "buffer");
def_id!(/// Image / render target / sampled texture.
    TextureId, "texture");
def_id!(/// Texture sampler state.
    SamplerId, "sampler");
def_id!(/// A shader module — SPIR-V words (the committed shader ABI).
    ShaderId, "shader");
def_id!(/// A compiled render or compute pipeline (state + shader entry points).
    PipelineId, "pipeline");
def_id!(/// A bound set of resources (WebGPU bind group / Vulkan descriptor set).
    BindGroupId, "bind_group");
def_id!(/// A presentable output surface (one DDP `Surface`).
    SurfaceId, "surface");
def_id!(/// A timeline fence for host↔guest synchronization.
    FenceId, "fence");

/// A generational slot: `gen` increments each time an id is reused, so a stale reference can be told
/// apart from a live one in diagnostics.
struct Slot<T> {
    gen: u32,
    val: T,
}

/// Host-side id → object map with lifecycle checking. One per resource kind per backend.
pub struct ResourceTable<T> {
    kind: &'static str,
    live: HashMap<u32, Slot<T>>,
    /// Highest generation ever assigned to an id (survives free) — for stale-vs-unknown diagnostics.
    gens: HashMap<u32, u32>,
}

impl<T> ResourceTable<T> {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            live: HashMap::new(),
            gens: HashMap::new(),
        }
    }

    /// Insert a freshly-created resource. Errors if `id` is already live (a duplicate create).
    pub fn insert(&mut self, id: u32, val: T) -> Result<()> {
        if self.live.contains_key(&id) {
            return Err(GpuError::DuplicateId { kind: self.kind, id });
        }
        let gen = self.gens.get(&id).map(|g| g + 1).unwrap_or(1);
        self.gens.insert(id, gen);
        self.live.insert(id, Slot { gen, val });
        Ok(())
    }

    /// Look up a live resource. Errors (`UnknownId`) if it was never created or was freed.
    pub fn get(&self, id: u32) -> Result<&T> {
        self.live
            .get(&id)
            .map(|s| &s.val)
            .ok_or(GpuError::UnknownId { kind: self.kind, id })
    }

    pub fn get_mut(&mut self, id: u32) -> Result<&mut T> {
        let kind = self.kind;
        self.live
            .get_mut(&id)
            .map(|s| &mut s.val)
            .ok_or(GpuError::UnknownId { kind, id })
    }

    /// Remove (destroy) a live resource, returning it. Errors on double-free / use-after-free.
    pub fn remove(&mut self, id: u32) -> Result<T> {
        self.live
            .remove(&id)
            .map(|s| s.val)
            .ok_or(GpuError::UnknownId { kind: self.kind, id })
    }

    pub fn contains(&self, id: u32) -> bool {
        self.live.contains_key(&id)
    }

    /// Current generation of a live id (for diagnostics / fence correlation).
    pub fn generation(&self, id: u32) -> Option<u32> {
        self.live.get(&id).map(|s| s.gen)
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Iterate live (id, &value) pairs — used by backends to release everything on teardown.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.live.iter().map(|(k, s)| (*k, &s.val))
    }
}
