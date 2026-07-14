//! [`Surface`]: one node of the neutral scene graph — its committed buffer metadata, damage, opaque /
//! input regions, role, and pending frame-callback count.
//!
//! Ported from `hl-compositor`'s per-surface state (`buffers`, `repacks`, `dirty`, `titles`,
//! `visibility`, opaque/input regions in `SurfaceAttributes`) and `hl-display`'s `SurfaceBuffer` value
//! shape — flattened into one plain struct the scene owns, with NO Smithay `wl_surface`/`with_states`.

use super::damage::{DamageRegion, Rect};
use super::window::{PopupPlacement, SurfaceRole};

/// Compositor-global surface identity — the neutral analogue of `hl-compositor`'s monotonic host `sid`
/// (`HlState::surface_ids`), which is collision-free across a client's protocol-id reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u32);

/// Pixel format of a committed buffer. `Xrgb8888` is opaque (alpha forced to 1); `Argb8888` honours
/// its premultiplied alpha. Mirrors `hl-display`'s `SurfaceBuffer::format` convention (1 ⇒ XRGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Argb8888,
    Xrgb8888,
}

impl Format {
    /// Whether the format is fully opaque (drives the conservative occlusion / blend fast path).
    pub fn is_opaque(self) -> bool {
        matches!(self, Format::Xrgb8888)
    }
}

/// `wp_viewport` state: an optional source crop over the backing texture and an optional destination
/// logical size. Mirrors `ViewportCachedState` (src/dst) as read in `snapshot_surface`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    /// Source crop `(x, y, w, h)` in logical (post buffer-scale) coordinates, if any.
    pub src: Option<(f64, f64, f64, f64)>,
    /// Destination logical size `(w, h)`, if any.
    pub dst: Option<(i32, i32)>,
}

/// A committed buffer's neutral metadata: its backing texture size, format, and buffer scale. No
/// pixels — the neutral policy composes/paces on geometry alone; a real adapter attaches the actual
/// texture behind this. Mirrors the dimensions carried on `hl-display`'s `SurfaceBuffer`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BufferState {
    pub tex_w: i32,
    pub tex_h: i32,
    pub format: Format,
    /// `wl_surface.set_buffer_scale` (HiDPI). Logical size is `tex / scale` absent a viewport.
    pub buffer_scale: i32,
    /// Whether the pixels live in a host GPU allocation (IOSurface/dmabuf) rather than CPU shm. A GPU
    /// root presents zero-copy and its children become overlay layers (see `present_tree`).
    pub gpu: bool,
}

impl BufferState {
    /// On-screen logical size `(w, h)` after `wp_viewport` and buffer scale, following
    /// `logical_size_and_uv`: a viewport `dst` wins; else a `src` crop's size; else `tex / scale`.
    pub fn logical_size(&self, viewport: &Viewport) -> (i32, i32) {
        match (viewport.dst, viewport.src) {
            (Some((dw, dh)), _) if dw > 0 && dh > 0 => (dw, dh),
            (None, Some((_, _, sw, sh))) if sw > 0.0 && sh > 0.0 => {
                ((sw.round() as i32).max(1), (sh.round() as i32).max(1))
            }
            _ => {
                let s = self.buffer_scale.max(1);
                ((self.tex_w / s).max(1), (self.tex_h / s).max(1))
            }
        }
    }
}

/// Host-observed visibility of a surface's native window. A non-visible root must NOT present (its
/// frame pacing is withheld). Mirrors `hl-display::present::SurfaceVisibility`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Occluded,
    Minimized,
}

impl Visibility {
    /// Whether a surface in this state may be presented this cycle (only [`Visibility::Visible`]).
    /// Mirrors `visibility_allows_present`.
    pub fn allows_present(self) -> bool {
        matches!(self, Visibility::Visible)
    }
}

/// A surface's committed content unpacked into the value the [`crate::scene::port::Presenter`] port
/// consumes — the neutral analogue of `hl-display`'s `SurfaceBuffer` (geometry only; a real adapter
/// attaches the actual texture/IOSurface behind `surface`). Produced by `service/compose.rs`.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentableImage {
    pub surface: SurfaceId,
    /// Logical on-screen size after `wp_viewport` + buffer transform/scale.
    pub width: i32,
    pub height: i32,
    pub format: Format,
    /// Pixels live in a host GPU allocation (zero-copy) rather than CPU shm.
    pub gpu: bool,
    /// If this image is an `xdg_popup` presented as its own native window, where to open it. Inert on
    /// the composite-into-parent path. Mirrors `SurfaceBuffer::popup`.
    pub popup: Option<PopupPlacement>,
}

/// One node of the scene graph.
#[derive(Clone, Debug)]
pub struct Surface {
    pub id: SurfaceId,
    /// What this surface IS in the window tree (toplevel / popup / subsurface / cursor / roleless).
    pub role: SurfaceRole,
    /// The last committed buffer metadata, retained across a bufferless commit exactly as
    /// `HlState::buffers` retains the last `wl_buffer` (so a re-composite can redraw a child that did
    /// not re-attach). `None` before the first attach or after an explicit detach.
    pub buffer: Option<BufferState>,
    pub viewport: Viewport,
    /// Damage accumulated by commits since this surface's tree was last presented.
    pub damage: DamageRegion,
    /// `wl_surface.set_opaque_region`, in surface-local logical space. Drives conservative occlusion in
    /// `compose`'s `tree_dirty` (ported from `opaque_covers_root_rect`). A single rect is the neutral
    /// simplification of a full `wl_region` (a conservative subset is always safe).
    pub opaque_region: Option<Rect>,
    /// `wl_surface.set_input_region`, surface-local. `None` ⇒ the whole surface accepts input.
    pub input_region: Option<Rect>,
    /// Window title, mirrored for the presenter to label a native window (`HlState::titles`).
    pub title: String,
    /// `wl_surface.frame` callbacks requested since the last pace, as a count (the neutral analogue of
    /// the retained `WlCallback` objects). Drained/fired or retained by the schedule service.
    pub pending_callbacks: u32,
}

impl Surface {
    pub fn new(id: SurfaceId) -> Surface {
        Surface {
            id,
            role: SurfaceRole::None,
            buffer: None,
            viewport: Viewport::default(),
            damage: DamageRegion::new(),
            opaque_region: None,
            input_region: None,
            title: String::new(),
            pending_callbacks: 0,
        }
    }

    /// On-screen logical size `(w, h)`, or `None` when no buffer is committed. The size half of a
    /// present snapshot (mirrors `surface_logical_size`).
    pub fn logical_size(&self) -> Option<(i32, i32)> {
        self.buffer.map(|b| b.logical_size(&self.viewport))
    }

    /// Whether surface-local point `(x, y)` is input-sensitive: inside the surface bounds AND inside the
    /// input region (or no region set). Mirrors `logical_region_accepts` + the bounds check in
    /// `input_surface_at_offset`.
    pub fn accepts_input_at(&self, x: i32, y: i32) -> bool {
        let Some((w, h)) = self.logical_size() else {
            return false;
        };
        if !Rect::new(0, 0, w, h).contains_point(x, y) {
            return false;
        }
        self.input_region.is_none_or(|r| r.contains_point(x, y))
    }
}
