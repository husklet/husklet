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
    pub fn contains(&self, other: &Rect) -> bool {
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

    pub(crate) fn violates_x(&self, bounds: &Rect) -> bool {
        self.x < bounds.x || self.right() > bounds.right()
    }

    pub(crate) fn violates_y(&self, bounds: &Rect) -> bool {
        self.y < bounds.y || self.bottom() > bounds.bottom()
    }

    /// Translate by `(dx, dy)` — used to lift a surface-local damage/opaque rect into root space by a
    /// (client-controlled) subsurface/popup offset. Saturating so an absurd offset cannot overflow the
    /// origin (a wrapped coordinate would place the rect on the wrong side of the plane).
    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            w: self.w,
            h: self.h,
        }
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
        Rect {
            x,
            y,
            w: right.saturating_sub(x),
            h: bottom.saturating_sub(y),
        }
    }
}

/// Terminal bound on rects retained between presents. A surface whose tree cannot present (minimized,
/// occluded, or a synchronized subsurface whose parent never commits again) keeps accumulating every
/// `wl_surface.damage` / `damage_buffer` its client commits, so an unbounded list is guest-controlled
/// memory growth. Past the bound the region collapses to its bounding box: a superset is always safe.
const MAX_DAMAGE_RECTS: usize = 64;

/// An accumulator for the changed regions of a surface across commits (the neutral analogue of the
/// per-surface `wl_surface.damage` / `damage_buffer` accumulation Smithay extends and the compositor
/// drains each commit in `ingest_buffer`). Rects are kept as a flat list bounded by
/// [`MAX_DAMAGE_RECTS`]; [`Self::bounding_box`] collapses them to the single upload hint carried on
/// `SurfaceBuffer::damage`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DamageRegion {
    rects: Vec<Rect>,
}

impl DamageRegion {
    pub fn new() -> DamageRegion {
        DamageRegion { rects: Vec::new() }
    }

    /// Accumulate one damaged rect (empties are ignored so a bounding box never widens spuriously).
    /// At [`MAX_DAMAGE_RECTS`] the retained rects collapse into their bounding box first, so a client
    /// that damages without ever presenting cannot grow this list without bound. Returns whether a
    /// visible rect landed — the caller's proof that a damage-only commit changed something.
    pub fn add(&mut self, rect: Rect) -> bool {
        if rect.is_empty() {
            return false;
        }
        if self.rects.len() >= MAX_DAMAGE_RECTS {
            let collapsed = self.bounding_box().unwrap_or_default();
            self.rects.clear();
            self.rects.push(collapsed);
        }
        self.rects.push(rect);
        true
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

#[cfg(test)]
mod tests {
    use super::{DamageRegion, Rect};

    /// An absolute retained-rect ceiling, stated independently of [`super::MAX_DAMAGE_RECTS`] so raising
    /// the bound cannot silently satisfy the test it exists to pass.
    const RETAINED_CEILING: usize = 256;

    #[test]
    fn accumulated_damage_is_bounded_and_still_covers_every_rect() {
        // A surface whose tree never presents (minimized / occluded) keeps every committed damage rect.
        // The retained list must stay bounded while the coverage it reports stays a superset.
        let mut region = DamageRegion::new();
        for i in 0..10_000 {
            assert!(region.add(Rect::new(i, 2 * i, 3, 4)));
            assert!(
                region.rects().len() <= RETAINED_CEILING,
                "retained damage grew to {} rects",
                region.rects().len()
            );
        }
        let bounds = region.bounding_box().expect("damage was accumulated");
        assert!(bounds.contains(&Rect::new(0, 0, 3, 4)));
        assert!(bounds.contains(&Rect::new(9_999, 19_998, 3, 4)));
    }

    #[test]
    fn empty_rect_is_not_accumulated() {
        let mut region = DamageRegion::new();
        assert!(!region.add(Rect::new(0, 0, 0, 0)));
        assert!(!region.add(Rect::new(5, 5, -3, 10)));
        assert!(region.is_empty());
    }
}
