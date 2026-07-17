//! The guest↔engine ioctl ABI + connection constants the transport carries handles against.
//!
//! These types describe the render-node `HL_IOCTL_GPU_ALLOC` ioctl and the surface it hands back — the
//! *out-of-band handle* side of the wire (a dma-buf fd correlated to a surface id), NOT the protocol IR.
//! Ported byte-for-byte from `hl-shim`'s `transport.rs`: the `#[repr(C)]` layout, the ioctl request code,
//! and the dma-buf modifier constants must match the engine's `mem.c` handler and the pinned guest exactly.

/// Default host GPU-exec socket path (overridable via `$HL_GPU_EXEC`); matches the shipped `gl_shim.c`.
pub const DEFAULT_EXEC_SOCK: &str = "/run/user/0/hl-gpu-0";

/// Guest render node the [`super::super::adapter::unix::renderd::alloc`] ioctl targets.
pub const RENDER_NODE: &str = "/dev/dri/renderD128";

/// The `HL_IOCTL_GPU_ALLOC` request code and dma-buf constants (must match `hl_gpu.h` / the engine's
/// `mem.c` handler and `gl_shim.c`). These describe the guest↔engine ioctl ABI, not the hl-gpu IR.
pub const HL_IOCTL_GPU_ALLOC: u64 = 0xC020_DD01;
pub const HL_DMABUF_MOD_MAGIC: u32 = 0x6464;
pub const DRM_FMT_XRGB8888: u32 = 0x3432_5258;

/// Mirror of the C `struct hl_gpu_alloc` the ioctl reads/writes. `#[repr(C)]` pins the field order and
/// padding so the 32-byte layout matches the engine handler byte-for-byte (0xC02**0**DD01 → 0x20 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuAlloc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
    pub id: u32,
    pub fd: i32,
    pub ptr: u64,
}

/// A rendered frame's target surface, as registered with the engine via
/// [`super::super::adapter::unix::renderd::alloc`]. This is the handle the transport's submit header names
/// so the host executor knows which output to present to.
#[derive(Clone, Copy, Debug, Default)]
pub struct Surface {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The dma-buf fd the ioctl handed back (for the wayland `SCM_RIGHTS` commit); -1 when unset.
    pub fd: i32,
    /// The allocation generation for [`id`](Self::id), stamped by the host at allocation time. Because
    /// the engine recycles a macOS IOSurface id across allocations, the guest echoes this generation in
    /// the dmabuf modifier (`modifier_hi` bits 17..=31) so the compositor can reject a stale reference
    /// (a modifier whose generation no longer matches the id's live allocation). 0 == unversioned.
    pub generation: u32,
}

impl Surface {
    pub fn from_alloc(a: &GpuAlloc) -> Self {
        Surface {
            id: a.id,
            width: a.width,
            height: a.height,
            stride: a.stride,
            fd: a.fd,
            // The engine returns the allocation generation in the `format` field on OUTPUT (it is an
            // input-only field otherwise), keeping the 32-byte ioctl ABI unchanged. Mask to the 15-bit
            // modifier field. 0 (an old engine / the gl_shim oracle) stays unversioned.
            generation: a.format & 0x7fff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_alloc_layout_is_32_bytes() {
        // The ioctl request 0xC020DD01 encodes a 0x20-byte payload; the struct must match.
        assert_eq!(core::mem::size_of::<GpuAlloc>(), 0x20);
    }

    #[test]
    fn surface_from_alloc_masks_generation() {
        let a = GpuAlloc {
            id: 3,
            width: 8,
            height: 8,
            stride: 32,
            fd: 9,
            format: 0x8005,
            ptr: 0,
        };
        let s = Surface::from_alloc(&a);
        assert_eq!((s.id, s.width, s.height, s.stride, s.fd), (3, 8, 8, 32, 9));
        assert_eq!(s.generation, 0x8005 & 0x7fff);
    }
}
