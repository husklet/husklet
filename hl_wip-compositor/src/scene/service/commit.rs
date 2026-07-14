//! `commit_surface`: apply one `wl_surface.commit` to the scene and report whether the surface's
//! visible content changed (so the caller can skip a redundant present).
//!
//! Ported from `hl-compositor`'s `ingest_buffer` (the CPU half of damage tracking): take the committed
//! buffer assignment, drain the commit's damage into the surface, update viewport / opaque / input
//! regions, count a requested frame callback, and mark the surface dirty iff its pixels changed. The
//! Smithay `with_states` / `RepackCache` / GPU-import specifics are dropped — this is geometry + damage
//! bookkeeping only.

use crate::scene::model::{BufferState, Rect, Scene, SurfaceId, Viewport};

/// What a commit does to the surface's attached buffer. Mirrors Smithay's `BufferAssignment` plus a
/// "no change this commit" case (a frame-callback-only or region-only commit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BufferChange {
    /// A new buffer was attached.
    New(BufferState),
    /// The buffer was explicitly detached (`wl_surface.attach(null)`).
    Removed,
    /// No buffer change this commit (the last one persists).
    Keep,
}

/// The double-buffered state a single commit carries. A field left at its default makes no change.
#[derive(Clone, Debug)]
pub struct Commit {
    pub buffer: BufferChange,
    /// Damage rects in surface-local logical space (`wl_surface.damage` / `damage_buffer`, unified).
    pub damage: Vec<Rect>,
    /// `wp_viewport` state to apply, if the commit set it.
    pub viewport: Option<Viewport>,
    /// `wl_surface.set_opaque_region`: `Some(region)` replaces it (`None` inside = cleared); the outer
    /// `None` means the commit did not touch the opaque region.
    pub opaque_region: Option<Option<Rect>>,
    /// `wl_surface.set_input_region`, same convention as `opaque_region`.
    pub input_region: Option<Option<Rect>>,
    /// The commit requested a `wl_surface.frame` callback.
    pub frame_callback: bool,
    /// `xdg_toplevel.set_title` mirrored for the presenter, if the commit changed it.
    pub title: Option<String>,
}

impl Default for Commit {
    fn default() -> Commit {
        Commit {
            buffer: BufferChange::Keep,
            damage: Vec::new(),
            viewport: None,
            opaque_region: None,
            input_region: None,
            frame_callback: false,
            title: None,
        }
    }
}

impl Commit {
    /// A bare frame-callback-only commit (no buffer, no damage) — the case that must fire a callback
    /// without forcing a present.
    pub fn frame_callback_only() -> Commit {
        Commit { frame_callback: true, ..Commit::default() }
    }

    /// A commit that attaches `buffer` and fully damages it.
    pub fn attach(buffer: BufferState) -> Commit {
        Commit { buffer: BufferChange::New(buffer), ..Commit::default() }
    }

    pub fn with_damage(mut self, rect: Rect) -> Commit {
        self.damage.push(rect);
        self
    }
}

/// Apply `commit` to surface `sid` in `scene`. Returns whether the surface's visible content changed
/// (a new/removed buffer, or damage against an existing buffer) — the signal that a present is needed.
/// A frame-callback-only or region-only commit returns `false` (nothing new to show) but still records
/// the pending callback. Mirrors `ingest_buffer`'s return contract.
pub fn commit_surface(scene: &mut Scene, sid: SurfaceId, commit: Commit) -> bool {
    let Some(surface) = scene.get_mut(sid) else {
        return false;
    };

    // Non-content state first (never on its own a reason to present).
    if let Some(vp) = commit.viewport {
        surface.viewport = vp;
    }
    if let Some(region) = commit.opaque_region {
        surface.opaque_region = region;
    }
    if let Some(region) = commit.input_region {
        surface.input_region = region;
    }
    if let Some(title) = commit.title {
        surface.title = title;
    }
    if commit.frame_callback {
        surface.pending_callbacks = surface.pending_callbacks.saturating_add(1);
    }

    let changed = match commit.buffer {
        BufferChange::Removed => {
            let had = surface.buffer.take().is_some();
            surface.damage.clear();
            had
        }
        BufferChange::New(buffer) => {
            surface.buffer = Some(buffer);
            for rect in &commit.damage {
                surface.damage.add(*rect);
            }
            true
        }
        BufferChange::Keep => {
            if surface.buffer.is_some() {
                // Accumulate this commit's damage and report a change only if a REAL (non-empty) rect
                // landed: `DamageRegion::add` drops empty/negative rects, so a damage-only commit whose
                // rects are all degenerate (e.g. a `wl_surface.damage(0,0,0,0)`) adds nothing visible and
                // must not force a spurious full-surface present.
                let before = surface.damage.rects().len();
                for rect in &commit.damage {
                    surface.damage.add(*rect);
                }
                surface.damage.rects().len() > before
            } else {
                // No buffer attached: damage against a non-existent buffer changes nothing visible.
                false
            }
        }
    };

    if changed {
        scene.mark_dirty(sid);
    }
    changed
}
