//! [`Output`]: a display the scene composites onto — its mode, refresh interval, and integer scale.
//!
//! Ported from `hl-compositor`'s `Output`/`OUTPUT_REFRESH_MHZ` handling (`output_logical_size`,
//! `PresentedFrame::from_fallback`'s refresh derivation) but neutral: no Smithay `output::Output`, no
//! `wl_output` global. The logical size drives popup constraint + maximize/fullscreen sizing; the
//! refresh interval drives frame pacing and `wp_presentation` feedback timing.

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
}

impl Output {
    pub fn new(id: OutputId, name: impl Into<String>, mode_w: i32, mode_h: i32, refresh_mhz: i64) -> Output {
        Output { id, name: name.into(), mode_w, mode_h, refresh_mhz, scale: 1 }
    }

    pub fn with_scale(mut self, scale: i32) -> Output {
        self.scale = scale.max(1);
        self
    }

    /// The output's logical size `(w, h)` — device mode divided by the integer scale, each clamped to
    /// at least 1. This is the bound a maximized/fullscreen toplevel is configured to and the target
    /// area popup placement constrains against (mirrors `HlState::output_logical_size`).
    pub fn logical_size(&self) -> (i32, i32) {
        let scale = self.scale.max(1);
        ((self.mode_w / scale).max(1), (self.mode_h / scale).max(1))
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
