use crate::result::{VK_ERROR_OUT_OF_DATE_KHR, VK_ERROR_SURFACE_LOST_KHR, VK_SUCCESS};

/// Whether the readback plane carries a real frame (not the all-zero fill [`pixels_to_xrgb8888`] returns
/// when the readback was too short). A valid convert always stamps the `X` byte `0xFF`, so a genuine frame
/// is never all-zero — this rejects a failed readback rather than committing a blank buffer.
pub(super) struct FramePlane;
impl FramePlane {
    pub(super) fn is_present(xrgb: &[u8]) -> bool {
        xrgb.iter().any(|&b| b != 0)
    }
}

/// `WL_SHM_FORMAT_XRGB8888` — the byte order [`pixels_to_xrgb8888`] packs into.
pub(super) const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// `WL_MARSHAL_FLAG_DESTROY` — passed to `wl_proxy_marshal_flags` for destructor requests (the proxy is
/// freed as part of the marshal).
pub(super) const WL_MARSHAL_FLAG_DESTROY: u32 = 1;

// ---- wire opcodes (from wayland.xml; stable across versions) ----
pub(super) const OP_DISPLAY_GET_REGISTRY: u32 = 1;
pub(super) const OP_REGISTRY_BIND: u32 = 0;
pub(super) const OP_SHM_CREATE_POOL: u32 = 0;
pub(super) const OP_SHM_POOL_CREATE_BUFFER: u32 = 0;
pub(super) const OP_SHM_POOL_DESTROY: u32 = 1;
pub(super) const OP_BUFFER_DESTROY: u32 = 0;
pub(super) const OP_SURFACE_ATTACH: u32 = 1;
pub(super) const OP_SURFACE_DAMAGE: u32 = 2;
pub(super) const OP_SURFACE_COMMIT: u32 = 6;

// ==================================================================================================
// readback → wl_shm pixel convert (Vulkan top-left, format-aware — no vertical flip)
// ==================================================================================================

/// Convert a presented swapchain image's readback plane (tight-packed `w`×`h`, **top-left** origin, in the
/// image's native texel order) into the `WL_SHM_FORMAT_XRGB8888` little-endian byte order a `wl_shm` buffer
/// wants (`[B,G,R,X]` per texel, **top-left** origin).
///
/// A Vulkan swapchain image is top-left origin, so — unlike GL's `rgba_to_xrgb8888` — there is NO vertical
/// flip. `source_is_bgra` selects the channel reorder: a `Bgra8` source is already `[B,G,R,A]` (copy the
/// three color bytes, force `X`); an `Rgba8` source is `[R,G,B,A]` (swap R↔B). The `A` byte is discarded
/// (opaque XRGB) — the alpha byte is forced to `0xFF` so a genuine frame is never mistaken for the
/// all-zero "readback failed" fill.
pub fn pixels_to_xrgb8888(src: &[u8], w: usize, h: usize, source_is_bgra: bool) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    if src.len() < w * h * 4 {
        return out;
    }
    for i in 0..(w * h) {
        let s = i * 4;
        let d = i * 4;
        let (b, g, r) = if source_is_bgra {
            (src[s], src[s + 1], src[s + 2]) // [B,G,R,A]
        } else {
            (src[s + 2], src[s + 1], src[s]) // [R,G,B,A] → B,G,R
        };
        // XRGB8888 little-endian in memory is [B, G, R, X].
        out[d] = b;
        out[d + 1] = g;
        out[d + 2] = r;
        out[d + 3] = 0xFF;
    }
    out
}

/// A typed outcome for the fallible app-surface present. A library/symbol/global gap is a *soft* failure
/// (the caller maps it to `VK_SUCCESS` — the readback happened, the on-surface attach was skipped); a live
/// marshal/flush gap is a *hard* failure the caller surfaces as `VK_ERROR_OUT_OF_DATE_KHR` /
/// `VK_ERROR_SURFACE_LOST_KHR`. Never a fake present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WlAppError {
    /// No app `wl_surface*` was captured (not a wayland window) — soft.
    NoSurface,
    /// `libwayland-client.so.0` is not already mapped in this process (`RTLD_NOLOAD` miss) — soft.
    LibraryMissing,
    /// A required proxy/queue/interface symbol was absent from `libwayland-client` — soft.
    SymbolMissing(&'static str),
    /// `wl_proxy_get_display` on the app surface returned null — soft.
    NoDisplay,
    /// `wl_display_create_queue` / `wl_proxy_create_wrapper` returned null — soft.
    QueueSetup,
    /// The compositor never advertised `wl_shm` on the app's registry — soft.
    NoShmGlobal,
    /// Native surface identity is unavailable; SHM fallback remains valid.
    NoIdentity,
    /// The readback plane was smaller than `w*h*4` (or all-zero) — hard.
    BadSize,
    /// Allocating / mapping the shm memfd failed — hard.
    ShmAlloc,
    /// A `wl_proxy_marshal_flags` constructor returned null — hard.
    Marshal,
    /// `wl_display_flush` reported a socket error — hard.
    Flush,
}

impl WlAppError {
    /// Whether this is a *soft* failure (the presenter is simply unavailable and the caller keeps the
    /// readback-only present, returning `VK_SUCCESS`) vs a *hard* live-present failure.
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            WlAppError::NoSurface
                | WlAppError::LibraryMissing
                | WlAppError::SymbolMissing(_)
                | WlAppError::NoDisplay
                | WlAppError::QueueSetup
                | WlAppError::NoShmGlobal
                | WlAppError::NoIdentity
        )
    }

    /// Map this present outcome onto the `VkResult` `vkQueuePresentKHR` returns for the swapchain. A soft
    /// error is `VK_SUCCESS` (the readback ran; only the on-surface attach was skipped, i.e. an
    /// offscreen/headless present). A connection/allocation loss (`Flush`/`ShmAlloc`) is
    /// `VK_ERROR_SURFACE_LOST_KHR`; a per-frame marshal/size failure is `VK_ERROR_OUT_OF_DATE_KHR` (the app
    /// recreates its swapchain).
    pub fn to_vk_result(&self) -> i32 {
        if self.is_unavailable() {
            return VK_SUCCESS;
        }
        match self {
            WlAppError::Flush | WlAppError::ShmAlloc => VK_ERROR_SURFACE_LOST_KHR,
            _ => VK_ERROR_OUT_OF_DATE_KHR,
        }
    }
}

pub type WlAppResult<T> = Result<T, WlAppError>;
