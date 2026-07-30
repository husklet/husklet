use crate::scene::model::{BufferTransform, Rect};

pub(super) fn surface(
    damage: &[Rect],
    width: i32,
    height: i32,
    scale: i32,
    transform: BufferTransform,
) -> Vec<Rect> {
    let scale = scale.max(1);
    damage
        .iter()
        .filter_map(|rect| {
            let x0 = rect.x.max(0).min(width);
            let y0 = rect.y.max(0).min(height);
            let x1 = rect.right().max(0).min(width);
            let y1 = rect.bottom().max(0).min(height);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let corners = [
                transform.map_point(x0, y0, width, height),
                transform.map_point(x1 - 1, y0, width, height),
                transform.map_point(x0, y1 - 1, width, height),
                transform.map_point(x1 - 1, y1 - 1, width, height),
            ];
            let left = corners.iter().map(|point| point.0).min()?;
            let top = corners.iter().map(|point| point.1).min()?;
            let right = corners.iter().map(|point| point.0).max()?.saturating_add(1);
            let bottom = corners.iter().map(|point| point.1).max()?.saturating_add(1);
            let logical_right = right.saturating_add(scale - 1) / scale;
            let logical_bottom = bottom.saturating_add(scale - 1) / scale;
            let logical_left = left / scale;
            let logical_top = top / scale;
            Some(Rect::new(
                logical_left,
                logical_top,
                logical_right.saturating_sub(logical_left),
                logical_bottom.saturating_sub(logical_top),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_buffer_damage_through_scale_and_rotation() {
        assert_eq!(
            surface(&[Rect::new(2, 4, 6, 8)], 20, 10, 2, BufferTransform::Normal,),
            vec![Rect::new(1, 2, 3, 3)]
        );
        assert_eq!(
            surface(&[Rect::new(2, 1, 4, 3)], 10, 8, 1, BufferTransform::_90,),
            vec![Rect::new(1, 4, 3, 4)]
        );
    }

    #[test]
    fn clamps_hostile_rectangles_before_mapping() {
        assert_eq!(
            surface(
                &[Rect::new(-4, -2, 8, 6), Rect::new(20, 20, 2, 2)],
                10,
                10,
                1,
                BufferTransform::Normal,
            ),
            vec![Rect::new(0, 0, 4, 4)]
        );
    }
}
