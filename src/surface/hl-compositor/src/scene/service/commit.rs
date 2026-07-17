//! `commit_surface`: apply one `wl_surface.commit` to the scene and report whether the surface's
//! visible content changed (so the caller can skip a redundant present).
//!
//! Ported from `hl-compositor`'s `ingest_buffer` (the CPU half of damage tracking): take the committed
//! buffer assignment, drain the commit's damage into the surface, update viewport / opaque / input
//! regions, count a requested frame callback, and mark the surface dirty iff its pixels changed. The
//! Smithay `with_states` / `RepackCache` / GPU-import specifics are dropped — this is geometry + damage
//! bookkeeping only.

use crate::scene::model::{BufferState, BufferTransform, Rect, Scene, SurfaceId, Viewport};

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
    /// `wl_surface.set_buffer_transform` to apply, if the commit set it (double-buffered, re-read every
    /// commit like `viewport`, so clearing back to `Normal` reverts too).
    pub buffer_transform: Option<BufferTransform>,
    /// `wl_surface.set_opaque_region`: `Some(region)` replaces it (`None` inside = cleared); the outer
    /// `None` means the commit did not touch the opaque region.
    pub opaque_region: Option<Option<Rect>>,
    /// `wl_surface.set_input_region`, same convention as `opaque_region`.
    pub input_region: Option<Option<Rect>>,
    /// The commit requested a `wl_surface.frame` callback.
    pub frame_callback: bool,
    /// `xdg_toplevel.set_title` mirrored for the presenter, if the commit changed it.
    pub title: Option<String>,
    /// Current committed xdg window geometry. The outer option means this commit supplies the state;
    /// the inner option is the protocol's unset value.
    pub window_geometry: Option<Option<Rect>>,
}

impl Default for Commit {
    fn default() -> Commit {
        Commit {
            buffer: BufferChange::Keep,
            damage: Vec::new(),
            viewport: None,
            buffer_transform: None,
            opaque_region: None,
            input_region: None,
            frame_callback: false,
            title: None,
            window_geometry: None,
        }
    }
}

impl Commit {
    /// A bare frame-callback-only commit (no buffer, no damage) — the case that must fire a callback
    /// without forcing a present.
    pub fn frame_callback_only() -> Commit {
        Commit {
            frame_callback: true,
            ..Commit::default()
        }
    }

    /// A commit that attaches `buffer` and fully damages it.
    pub fn attach(buffer: BufferState) -> Commit {
        Commit {
            buffer: BufferChange::New(buffer),
            ..Commit::default()
        }
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

    // Track whether a double-buffered GEOMETRY input (buffer transform / viewport) actually changed value
    // this commit. Against an already-committed buffer that alone re-presents the SAME pixels under a new
    // orientation/crop — even with no fresh attach and no damage — so a transform-only or viewport-only
    // commit must still mark the surface dirty (else the rotation/crop would not appear until the next
    // buffer). A no-op re-set (same value) is not a change and must not force a spurious present.
    let mut geometry_changed = false;

    // Non-content state first. A geometry input that changed value re-presents (handled below); the
    // opaque/input regions and title never on their own are a reason to present.
    if let Some(vp) = commit.viewport {
        geometry_changed |= surface.viewport != vp;
        surface.viewport = vp;
    }
    if let Some(transform) = commit.buffer_transform {
        geometry_changed |= surface.transform != transform;
        surface.transform = transform;
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
    if let Some(geometry) = commit.window_geometry {
        geometry_changed |= surface.window_geometry != geometry;
        surface.window_geometry = geometry;
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

    // A geometry-only change (new transform/viewport) re-presents the retained buffer under the new
    // orientation/crop, so it counts as a change too — but only when a buffer is actually committed
    // (nothing to re-present otherwise). `surface` was reborrowed inside the match, so re-fetch it.
    let has_buffer = scene.get(sid).is_some_and(|s| s.buffer.is_some());
    let changed = changed || (geometry_changed && has_buffer);

    if changed {
        scene.mark_dirty(sid);
    }
    changed
}
