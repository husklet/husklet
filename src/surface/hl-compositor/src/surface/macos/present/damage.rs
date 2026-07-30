use crate::scene::model::Rect;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Damage {
    Empty,
    Full,
    Regions(Vec<Rect>),
}

const MAX_REGIONS: usize = 64;

impl Damage {
    pub(super) fn map(
        rects: &[Rect],
        origin: (i32, i32),
        logical_size: (i32, i32),
        pixel_size: (u32, u32),
    ) -> Self {
        if rects.is_empty() {
            return Self::Empty;
        }
        if logical_size.0 <= 0 || logical_size.1 <= 0 || pixel_size.0 == 0 || pixel_size.1 == 0 {
            return Self::Full;
        }
        let scale_x = f64::from(pixel_size.0) / f64::from(logical_size.0);
        let scale_y = f64::from(pixel_size.1) / f64::from(logical_size.1);
        let mut mapped = Vec::with_capacity(rects.len().min(MAX_REGIONS));
        let mut bounds: Option<Rect> = None;
        for rect in rects {
            let left = rect.x.saturating_sub(origin.0);
            let top = rect.y.saturating_sub(origin.1);
            let right = rect.right().saturating_sub(origin.0);
            let bottom = rect.bottom().saturating_sub(origin.1);
            let x0 = (f64::from(left) * scale_x)
                .floor()
                .clamp(0.0, f64::from(pixel_size.0)) as u32;
            let y0 = (f64::from(top) * scale_y)
                .floor()
                .clamp(0.0, f64::from(pixel_size.1)) as u32;
            let x1 = (f64::from(right) * scale_x)
                .ceil()
                .clamp(0.0, f64::from(pixel_size.0)) as u32;
            let y1 = (f64::from(bottom) * scale_y)
                .ceil()
                .clamp(0.0, f64::from(pixel_size.1)) as u32;
            if x1 > x0 && y1 > y0 {
                let rect = Rect::new(x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32);
                bounds = Some(match bounds {
                    Some(bounds) => bounds.union(&rect),
                    None => rect,
                });
                if mapped.len() < MAX_REGIONS {
                    mapped.push(rect);
                }
            }
        }
        if mapped.is_empty() {
            Self::Empty
        } else if rects.len() > MAX_REGIONS {
            Self::Regions(vec![bounds.expect("non-empty mapped damage has bounds")])
        } else {
            Self::Regions(mapped)
        }
    }

    pub(super) fn regions(&self) -> Option<&[Rect]> {
        match self {
            Self::Full => None,
            Self::Empty => Some(&[]),
            Self::Regions(regions) => Some(regions),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_separate_regions_and_scales_outward() {
        assert_eq!(
            Damage::map(
                &[Rect::new(1, 2, 3, 4), Rect::new(10, 12, 2, 1)],
                (0, 0),
                (20, 20),
                (40, 30),
            ),
            Damage::Regions(vec![Rect::new(2, 3, 6, 6), Rect::new(20, 18, 4, 2)])
        );
    }

    #[test]
    fn subtracts_origin_and_clamps_to_destination() {
        assert_eq!(
            Damage::map(
                &[Rect::new(8, 17, 10, 10), Rect::new(200, 200, 5, 5)],
                (10, 20),
                (100, 50),
                (200, 100),
            ),
            Damage::Regions(vec![Rect::new(0, 0, 16, 14)])
        );
    }

    #[test]
    fn empty_or_clipped_damage_remains_empty() {
        assert_eq!(Damage::map(&[], (0, 0), (10, 10), (20, 20)), Damage::Empty);
        assert_eq!(
            Damage::map(&[Rect::new(20, 20, 1, 1)], (0, 0), (10, 10), (20, 20)),
            Damage::Empty
        );
    }

    #[test]
    fn hostile_region_count_is_bounded_and_coalesced() {
        let rects = (0..10_000)
            .map(|index| Rect::new(index % 100, index / 100, 1, 1))
            .collect::<Vec<_>>();
        assert_eq!(
            Damage::map(&rects, (0, 0), (100, 100), (100, 100)),
            Damage::Regions(vec![Rect::new(0, 0, 100, 100)])
        );
    }
}
