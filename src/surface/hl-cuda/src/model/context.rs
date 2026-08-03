//! [`CudaContext`] — the per-context aggregate the CUDA driver operates on.
//!
//! It owns the whole CUDA object model for one context: the device descriptor, the memory-allocation
//! table ([`super::memory::Allocations`]), the module table ([`super::module::Modules`]), the stream
//! table ([`super::stream::StreamTable`]), the launch pipeline cache, and every guest-assigned id
//! counter. Ported from `hl-gpu/src/cuda.rs` (`CudaContext`) — the id-minting and pipeline-cache
//! semantics are kept byte-for-byte so the emitted IR is identical.
//!
//! The context builds NO `Cmd`s and submits nothing; it only mints ids and records bookkeeping. The
//! [`crate::service`] layer calls these methods, then submits the lowered commands through a
//! [`hl_gpu::CommandSink`].

use super::device::{CudaDeviceDesc, DevicePtr};
use super::event::EventTable;
use super::graphics::GraphicsResources;
use super::memory::{Allocations, HostMemory};
use super::module::{Function, Modules};
use super::stream::StreamTable;
use hl_gpu::BufferId;
use std::collections::HashMap;

pub struct CudaContext {
    pub device: CudaDeviceDesc,
    /// Device-memory allocations (unified-VA bump allocator + resolver).
    pub mem: Allocations,
    /// Host-side pinned + registered memory (`cuMemAllocHost` / `cuMemHostRegister` families).
    pub host: HostMemory,
    /// Loaded PTX modules + `cuModuleGetFunction` resolution.
    pub modules: Modules,
    /// CUDA streams (validation + a synchronize target).
    pub streams: StreamTable,
    /// CUDA events (cross-stream ordering markers: `cuEventRecord` / `cuStreamWaitEvent`).
    pub events: EventTable,
    /// CUDA graphics-interop registrations imported into this context.
    pub graphics: GraphicsResources,
    /// Launch pipeline cache: `(module, entry, block)` → `(shader id, pipeline id)`, so a repeated
    /// launch of the same kernel+block reuses the compiled shader/pipeline and emits no new
    /// `CreateShader`/`CreateComputePipeline`. The block dims are part of the key because they bake into
    /// the compiled kernel as the WebGPU/Metal `local_size`.
    pipelines: HashMap<(u32, u32, [u32; 3]), (u32, u32)>,
    /// Module-global backing allocations: `(module id, symbol name)` → `(device pointer, byte size)`,
    /// so `cuModuleGetGlobal` lazily creates one backing buffer per global and returns the same device
    /// pointer on repeat lookups.
    global_allocs: HashMap<(u32, String), (u64, u64)>,

    // guest-assigned id counters (monotonic; one buffer counter shared by allocations + param buffers,
    // exactly as the ported source did).
    next_buffer: u32,
    next_shader: u32,
    next_pipeline: u32,
    next_bind_group: u32,
    next_fence: u32,
    fence_value: u64,
}

impl CudaContext {
    pub fn new(device: CudaDeviceDesc) -> Self {
        Self {
            device,
            mem: Allocations::new(),
            host: HostMemory::new(),
            modules: Modules::new(),
            streams: StreamTable::new(),
            events: EventTable::new(),
            graphics: GraphicsResources::new(),
            pipelines: HashMap::new(),
            global_allocs: HashMap::new(),
            next_buffer: 1,
            next_shader: 1,
            next_pipeline: 1,
            next_bind_group: 1,
            next_fence: 1,
            fence_value: 1,
        }
    }

    // ---- id minting -------------------------------------------------------------------------------

    pub fn alloc_buffer(&mut self) -> u32 {
        let id = self.next_buffer;
        self.next_buffer += 1;
        id
    }

    pub fn alloc_shader(&mut self) -> u32 {
        let id = self.next_shader;
        self.next_shader += 1;
        id
    }

    pub fn alloc_pipeline(&mut self) -> u32 {
        let id = self.next_pipeline;
        self.next_pipeline += 1;
        id
    }

    pub fn alloc_bind_group(&mut self) -> u32 {
        let id = self.next_bind_group;
        self.next_bind_group += 1;
        id
    }

    pub fn alloc_fence(&mut self) -> u32 {
        let id = self.next_fence;
        self.next_fence += 1;
        id
    }

    /// The next timeline value to signal/wait a fence at (monotonic across the context).
    pub fn next_fence_value(&mut self) -> u64 {
        let v = self.fence_value;
        self.fence_value += 1;
        v
    }

    // ---- pipeline cache ---------------------------------------------------------------------------

    /// The cached `(shader, pipeline)` for this `(module, entry, block)`, if a prior launch created it.
    pub fn cached_pipeline(&self, module: u32, entry: u32, block: [u32; 3]) -> Option<(u32, u32)> {
        self.pipelines.get(&(module, entry, block)).copied()
    }

    /// Record a freshly-created `(shader, pipeline)` for this `(module, entry, block)`.
    pub fn cache_pipeline(&mut self, module: u32, entry: u32, block: [u32; 3], v: (u32, u32)) {
        self.pipelines.insert((module, entry, block), v);
    }

    // ---- module-global backing allocations --------------------------------------------------------

    /// The cached `(device pointer, byte size)` a prior `cuModuleGetGlobal` created for `(module, name)`.
    pub fn global_alloc(&self, module: u32, name: &str) -> Option<(u64, u64)> {
        self.global_allocs.get(&(module, name.to_string())).copied()
    }

    /// Record the backing `(device pointer, byte size)` for a module global so repeat lookups reuse it.
    pub fn record_global_alloc(&mut self, module: u32, name: &str, ptr: u64, size: u64) {
        self.global_allocs
            .insert((module, name.to_string()), (ptr, size));
    }

    // ---- convenience delegates --------------------------------------------------------------------

    /// Resolve a device pointer to its backing (buffer id, byte offset), or `None` if dangling.
    pub fn resolve(&self, p: DevicePtr) -> Option<(BufferId, u64)> {
        self.mem.resolve(p)
    }

    /// The (source, entry-name) a launch of `func` forwards as its kernel descriptor.
    pub fn entry_source(&self, func: Function) -> Option<(String, String)> {
        self.modules.entry_source(func)
    }
}
