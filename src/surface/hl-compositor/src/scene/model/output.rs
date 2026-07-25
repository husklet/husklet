//! [`Output`]: a display the scene composites onto — its mode, refresh interval, and integer scale.
//!
//! Ported from `hl-compositor`'s `Output`/`OUTPUT_REFRESH_MHZ` handling (`output_logical_size`,
//! `PresentedFrame::from_fallback`'s refresh derivation) but neutral: no Smithay `output::Output`, no
//! `wl_output` global. The logical size drives popup constraint + maximize/fullscreen sizing; the
//! refresh interval drives frame pacing and `wp_presentation` feedback timing.

use super::surface::BufferTransform;

/// Stable identity of an output in the scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);

/// A composited display. `mode_w`/`mode_h` are device pixels; `scale` is the integer output scale
/// (`wl_output.scale`), so the logical size a toplevel is sized to is `mode / scale`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub id: OutputId,
    pub name: String,
    pub mode_w: i32,
    pub mode_h: i32,
    /// Refresh rate in millihertz (e.g. `60_000` = 60 Hz), mirroring `OUTPUT_REFRESH_MHZ`.
    pub refresh_mhz: i64,
    pub scale: i32,
    /// This output's top-left position in the global LOGICAL layout (`wl_output.geometry.x/y` and
    /// xdg-output `logical_position`). A single-output compositor sits at `(0, 0)`; a multi-output layout
    /// places each output side by side (e.g. a 1920-wide output at `(0, 0)` and its neighbour at
    /// `(1920, 0)`). Position-based surface→output routing (which `wl_output` a surface enters) tests a
    /// point against each output's `logical_rect`.
    pub pos_x: i32,
    pub pos_y: i32,
    /// `wl_output.transform` this output advertises — how the compositor's logical space is rotated/
    /// flipped onto the physical panel. A 90°/270° transform swaps the logical width/height a client
    /// lays out in (and that `wl_output`/xdg-output report).
    pub transform: BufferTransform,
}

impl Output {
    pub fn new(
        id: OutputId,
        name: impl Into<String>,
        mode_w: i32,
        mode_h: i32,
        refresh_mhz: i64,
    ) -> Output {
        Output {
            id,
            name: name.into(),
            mode_w,
            mode_h,
            refresh_mhz,
            scale: 1,
            pos_x: 0,
            pos_y: 0,
            transform: BufferTransform::Normal,
        }
    }

    pub fn with_scale(mut self, scale: i32) -> Output {
        self.scale = scale.max(1);
        self
    }

    /// Place this output's top-left at `(x, y)` in the global logical layout.
    pub fn with_position(mut self, x: i32, y: i32) -> Output {
        self.pos_x = x;
        self.pos_y = y;
        self
    }

    pub fn with_transform(mut self, transform: BufferTransform) -> Output {
        self.transform = transform;
        self
    }

    /// This output's rectangle in the global logical layout — its `(pos_x, pos_y)` origin plus its
    /// [`Self::logical_size`]. The bound position-based routing tests a global logical point against.
    pub fn logical_rect(&self) -> (i32, i32, i32, i32) {
        let (w, h) = self.logical_size();
        (self.pos_x, self.pos_y, w, h)
    }

    /// Whether global logical point `(x, y)` falls inside this output's [`Self::logical_rect`].
    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        let (ox, oy, w, h) = self.logical_rect();
        x >= ox && y >= oy && x < ox + w && y < oy + h
    }

    /// The output's logical size `(w, h)` — device mode divided by the integer scale, each clamped to
    /// at least 1, then swapped for a 90°/270° output transform (the rotated screen presents its mode's
    /// height as logical width and vice versa). This is the bound a maximized/fullscreen toplevel is
    /// configured to and the target area popup placement constrains against (mirrors
    /// `HlState::output_logical_size`), and what `wl_output`/xdg-output report.
    pub fn logical_size(&self) -> (i32, i32) {
        let scale = self.scale.max(1);
        let (w, h) = ((self.mode_w / scale).max(1), (self.mode_h / scale).max(1));
        self.transform.surface_size(w, h)
    }

    /// Refresh interval in nanoseconds (`0` when the rate is unknown / non-positive). Derived the same
    /// way `PresentedFrame::from_fallback` does: `1e12 / refresh_mHz`.
    pub fn refresh_nanos(&self) -> u64 {
        if self.refresh_mhz > 0 {
            1_000_000_000_000u64 / self.refresh_mhz as u64
        } else {
            0
        }
    }
}
