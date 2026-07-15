//! Geometry + damage value types: an integer [`Rect`] in logical (y-down) surface space and a
//! [`DamageRegion`] accumulator.
//!
//! Ported from the damage / occlusion algorithms in `hl-compositor`'s `handlers/compositor.rs`
//! (`region_covers_rect`, `opaque_covers_root_rect`, the per-commit damage drain) and the
//! `(i32,i32,i32,i32)` damage tuple carried on `hl-display`'s `SurfaceBuffer::damage`. Kept neutral:
//! no Smithay `Rectangle<i32, Logical>` — a plain `Rect` the scene owns.

/// An axis-aligned rectangle in logical (y-down) coordinates. `w`/`h` may be zero (an empty rect);
/// negative sizes are treated as empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// The exclusive right edge (`x + w`). Saturating so a hostile origin+extent near `i32::MAX` clamps
    /// to the axis end instead of overflowing (a wrapped negative edge would corrupt every containment /
    /// occlusion decision built on it).
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    /// The exclusive bottom edge (`y + h`). Saturating for the same reason as [`Self::right`].
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    /// A rect with no area (either dimension `<= 0`) covers/contains nothing.
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Whether the logical point `(px, py)` lies inside the rect (left/top inclusive, right/bottom
    /// exclusive — the half-open convention pixel coverage uses).
    pub fn contains_point(&self, px: i32, py: i32) -> bool {
        !self.is_empty() && px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    /// Whether this rect provably contains the WHOLE of `other`. An empty `other` is never contained
    /// (there is nothing to prove opaque — matches `region_covers_rect`'s empty-target rule).
    pub fn contains_rect(&self, other: &Rect) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }

    /// Whether the two rects overlap in a positive area.
    pub fn intersects(&self, other: &Rect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    /// Translate by `(dx, dy)` — used to lift a surface-local damage/opaque rect into root space by a
    /// (client-controlled) subsurface/popup offset. Saturating so an absurd offset cannot overflow the
    /// origin (a wrapped coordinate would place the rect on the wrong side of the plane).
    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x.saturating_add(dx), y: self.y.saturating_add(dy), w: self.w, h: self.h }
    }

    /// The smallest rect containing both inputs. An empty input is ignored (the other is returned);
    /// two empties give an empty rect.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        // Saturating: with both edges already saturated, `right - x` of a full-plane span can exceed
        // `i32::MAX`, so clamp the extent rather than overflow.
        Rect { x, y, w: right.saturating_sub(x), h: bottom.saturating_sub(y) }
    }
}

/// An accumulator for the changed regions of a surface across commits (the neutral analogue of the
/// per-surface `wl_surface.damage` / `damage_buffer` accumulation Smithay extends and the compositor
/// drains each commit in `ingest_buffer`). Rects are kept as a flat list; [`Self::bounding_box`]
/// collapses them to the single upload hint carried on `SurfaceBuffer::damage`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DamageRegion {
    rects: Vec<Rect>,
}

impl DamageRegion {
    pub fn new() -> DamageRegion {
        DamageRegion { rects: Vec::new() }
    }

    /// Accumulate one damaged rect (empties are ignored so a bounding box never widens spuriously).
    pub fn add(&mut self, rect: Rect) {
        if !rect.is_empty() {
            self.rects.push(rect);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Drop every accumulated rect (called once the surface's tree has been presented).
    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// The single rectangle bounding all accumulated damage, or `None` when nothing is damaged. This is
    /// the conservative upload hint the present path carries: a superset is always safe.
    pub fn bounding_box(&self) -> Option<Rect> {
        let mut it = self.rects.iter().copied().filter(|r| !r.is_empty());
        let first = it.next()?;
        Some(it.fold(first, |acc, r| acc.union(&r)))
    }
}
