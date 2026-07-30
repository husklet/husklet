use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// The resource families the executor addresses, each in its own identifier namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Buffer,
    Texture,
    Sampler,
    Shader,
    Pipeline,
    BindGroup,
    Surface,
    Fence,
}

/// Display-scoped allocator for executor resource identifiers.
///
/// Identifier families are independent because the protocol addresses each resource kind in its own
/// namespace. `0` is reserved as the exhaustion sentinel and is never issued as a live identifier.
#[derive(Debug)]
pub struct IrAllocator {
    exhausted: std::sync::atomic::AtomicBool,
    buffers: Ids,
    textures: Ids,
    samplers: Ids,
    shaders: Ids,
    pipelines: Ids,
    bind_groups: Ids,
    surfaces: Ids,
    fences: Ids,
    frames: AtomicU64,
}

impl Default for IrAllocator {
    fn default() -> Self {
        Self {
            exhausted: std::sync::atomic::AtomicBool::new(false),
            buffers: Ids::default(),
            textures: Ids::default(),
            samplers: Ids::default(),
            shaders: Ids::default(),
            pipelines: Ids::default(),
            bind_groups: Ids::default(),
            surfaces: Ids::default(),
            fences: Ids::default(),
            frames: AtomicU64::new(1),
        }
    }
}

impl IrAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffer(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.buffers, "buffer")
    }

    pub fn texture(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.textures, "texture")
    }

    pub fn sampler(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.samplers, "sampler")
    }

    pub fn shader(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.shaders, "shader")
    }

    pub fn pipeline(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.pipelines, "pipeline")
    }

    pub fn bind_group(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.bind_groups, "bind group")
    }

    pub fn surface(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.surfaces, "surface")
    }

    pub fn fence(&self) -> hl_gpu::Result<u32> {
        self.allocate(&self.fences, "fence")
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Relaxed)
    }

    /// Return a name to its family for reissue.
    ///
    /// Only for a name that was never published: hl-gpu executes a batch inside an all-tables transaction
    /// and rolls its id tables back EXACTLY to the pre-frame state when the batch NACKs, so a rejected
    /// batch leaves no host object holding the name. Reissuing it is what lets a retry emit the identical
    /// resource-creation stream instead of leaking a name per rejection.
    pub fn release(&self, kind: Resource, id: u32) {
        self.family(kind).release(id);
    }

    fn family(&self, kind: Resource) -> &Ids {
        match kind {
            Resource::Buffer => &self.buffers,
            Resource::Texture => &self.textures,
            Resource::Sampler => &self.samplers,
            Resource::Shader => &self.shaders,
            Resource::Pipeline => &self.pipelines,
            Resource::BindGroup => &self.bind_groups,
            Resource::Surface => &self.surfaces,
            Resource::Fence => &self.fences,
        }
    }

    pub fn frame(&self) -> hl_gpu::Result<hl_gpu::FrameSerial> {
        let serial = self
            .frames
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| hl_gpu::GpuError::ResourceLimit("frame serial"))?;
        hl_gpu::FrameSerial::new(serial)
    }

    fn allocate(&self, ids: &Ids, kind: &'static str) -> hl_gpu::Result<u32> {
        let result = ids.next(kind);
        if result.is_err() {
            self.exhausted.store(true, Ordering::Relaxed);
        }
        result
    }
}

#[derive(Debug)]
struct Ids {
    next: AtomicU32,
    /// Names released by a rolled-back frame, reissued in allocation order so a retry reproduces the
    /// rejected batch exactly.
    free: Mutex<VecDeque<u32>>,
}

impl Default for Ids {
    fn default() -> Self {
        Self {
            next: AtomicU32::new(1),
            free: Mutex::new(VecDeque::new()),
        }
    }
}

impl Ids {
    fn next(&self, kind: &'static str) -> hl_gpu::Result<u32> {
        if let Some(id) = self.pool().pop_front() {
            return Ok(id);
        }
        self.next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                id.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| hl_gpu::GpuError::ResourceLimit(kind))
    }

    fn release(&self, id: u32) {
        // `0` is the exhaustion sentinel and was never a live name.
        if id != 0 {
            self.pool().push_back(id);
        }
    }

    fn pool(&self) -> std::sync::MutexGuard<'_, VecDeque<u32>> {
        self.free.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::context::GlContext;
    use std::sync::Arc;

    #[test]
    fn families_have_independent_namespaces() {
        let ids = IrAllocator::new();

        assert_eq!(ids.buffer(), Ok(1));
        assert_eq!(ids.texture(), Ok(1));
        assert_eq!(ids.buffer(), Ok(2));
        assert_eq!(ids.texture(), Ok(2));
    }

    #[test]
    fn exhaustion_never_wraps_to_a_live_identifier() {
        let ids = Ids {
            next: AtomicU32::new(u32::MAX),
            free: Mutex::new(VecDeque::new()),
        };

        assert_eq!(
            ids.next("buffer"),
            Err(hl_gpu::GpuError::ResourceLimit("buffer"))
        );
        assert_eq!(
            ids.next("buffer"),
            Err(hl_gpu::GpuError::ResourceLimit("buffer"))
        );
    }

    #[test]
    fn exhaustion_is_sticky_for_every_identifier_family() {
        let exhausted = || Ids {
            next: AtomicU32::new(u32::MAX),
            free: Mutex::new(VecDeque::new()),
        };
        let allocator = IrAllocator {
            exhausted: std::sync::atomic::AtomicBool::new(false),
            buffers: exhausted(),
            textures: exhausted(),
            samplers: exhausted(),
            shaders: exhausted(),
            pipelines: exhausted(),
            bind_groups: exhausted(),
            surfaces: exhausted(),
            fences: exhausted(),
            frames: AtomicU64::new(1),
        };

        assert!(allocator.buffer().is_err());
        assert!(allocator.texture().is_err());
        assert!(allocator.sampler().is_err());
        assert!(allocator.shader().is_err());
        assert!(allocator.pipeline().is_err());
        assert!(allocator.bind_group().is_err());
        assert!(allocator.surface().is_err());
        assert!(allocator.fence().is_err());
        assert!(allocator.is_exhausted());
    }

    /// A rolled-back frame returns the names it issued, in issue order, so the retry reproduces the
    /// rejected batch exactly. Safe because hl-gpu rolls its id tables back to the pre-frame state on a
    /// NACK (see `runtime::service::dispatch`), so a rejected name never reached a live host object.
    #[test]
    fn frame_rollback_reissues_the_names_the_rejected_batch_never_published() {
        let allocator = Arc::new(IrAllocator::new());
        let mut context = GlContext::with_allocator(allocator);
        let frame = context.frame_state();

        assert_eq!(context.alloc_buffer_ir(), Ok(1));
        assert_eq!(context.alloc_buffer_ir(), Ok(2));
        assert_eq!(context.alloc_texture_ir(), Ok(1));
        context.restore_frame_state(frame);

        assert_eq!(context.alloc_buffer_ir(), Ok(1), "reissued in issue order");
        assert_eq!(context.alloc_buffer_ir(), Ok(2));
        assert_eq!(context.alloc_texture_ir(), Ok(1), "per-family namespaces");
        assert_eq!(context.alloc_buffer_ir(), Ok(3), "then the counter resumes");
    }

    /// A COMMITTED name is never reissued: the ledger only holds the frame being lowered, so a frame that
    /// was accepted leaves nothing to release.
    #[test]
    fn a_committed_name_is_not_reissued_by_a_later_rollback() {
        let allocator = Arc::new(IrAllocator::new());
        let mut context = GlContext::with_allocator(allocator);

        let committed = context.frame_state();
        assert_eq!(context.alloc_buffer_ir(), Ok(1));
        drop(committed); // the sink accepted this batch, so it is never restored

        let rejected = context.frame_state();
        assert_eq!(context.alloc_buffer_ir(), Ok(2));
        context.restore_frame_state(rejected);
        assert_eq!(
            context.alloc_buffer_ir(),
            Ok(2),
            "only the rejected frame's name comes back"
        );
    }
}
