use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicU32, Ordering};

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
}

impl Default for Ids {
    fn default() -> Self {
        Self {
            next: AtomicU32::new(1),
        }
    }
}

impl Ids {
    fn next(&self, kind: &'static str) -> hl_gpu::Result<u32> {
        self.next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                id.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| hl_gpu::GpuError::ResourceLimit(kind))
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

    #[test]
    fn frame_rollback_does_not_reuse_a_published_name() {
        let allocator = Arc::new(IrAllocator::new());
        let mut context = GlContext::with_allocator(allocator);
        let frame = context.frame_state();

        assert_eq!(context.alloc_buffer_ir(), Ok(1));
        context.restore_frame_state(frame);
        assert_eq!(context.alloc_buffer_ir(), Ok(2));
    }
}
