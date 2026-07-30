use crate::scene::model::BufferTransform;

/// Affine mapping from normalized surface coordinates to normalized buffer coordinates.
///
/// Metal receives this as two `float4` values:
/// `origin.xy, dx.xy, dy.xy, padding.xy`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Sampling {
    pub(super) map: [f32; 8],
    pub(super) width: u32,
    pub(super) height: u32,
}

impl Sampling {
    pub(super) fn new(
        transform: BufferTransform,
        buffer: (u32, u32),
        crop: Option<(f64, f64, f64, f64)>,
    ) -> Self {
        let (bw, bh) = buffer;
        let (sw, sh) = transform.surface_size(bw as i32, bh as i32);
        let (x, y, width, height) = crop.unwrap_or((0.0, 0.0, sw as f64, sh as f64));
        let left = (x / sw as f64) as f32;
        let top = (y / sh as f64) as f32;
        let right = ((x + width) / sw as f64) as f32;
        let bottom = ((y + height) / sh as f64) as f32;
        let buffer_uv = |x, y| match transform {
            BufferTransform::Normal => (x, y),
            BufferTransform::_90 => (1.0 - y, x),
            BufferTransform::_180 => (1.0 - x, 1.0 - y),
            BufferTransform::_270 => (y, 1.0 - x),
            BufferTransform::Flipped => (1.0 - x, y),
            BufferTransform::Flipped90 => (1.0 - y, 1.0 - x),
            BufferTransform::Flipped180 => (x, 1.0 - y),
            BufferTransform::Flipped270 => (y, x),
        };
        let origin = buffer_uv(left, top);
        let right_top = buffer_uv(right, top);
        let left_bottom = buffer_uv(left, bottom);

        Self {
            map: [
                origin.0,
                origin.1,
                right_top.0 - origin.0,
                right_top.1 - origin.1,
                left_bottom.0 - origin.0,
                left_bottom.1 - origin.1,
                0.0,
                0.0,
            ],
            width: width.round().max(1.0) as u32,
            height: height.round().max(1.0) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_is_identity() {
        assert_eq!(
            Sampling::new(BufferTransform::Normal, (4, 3), None),
            Sampling {
                map: [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                width: 4,
                height: 3,
            }
        );
    }

    #[test]
    fn quarter_turns_swap_dimensions() {
        for transform in [
            BufferTransform::_90,
            BufferTransform::_270,
            BufferTransform::Flipped90,
            BufferTransform::Flipped270,
        ] {
            let sampling = Sampling::new(transform, (4, 3), None);
            assert_eq!((sampling.width, sampling.height), (3, 4));
        }
    }
}
