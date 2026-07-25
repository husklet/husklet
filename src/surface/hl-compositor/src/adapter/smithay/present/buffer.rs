use super::*;

/// Client pixels deposited by the adapter, unpacked to tight top-left RGBA8888.
#[derive(Clone, Debug)]
pub struct StoredBuffer {
    pub width: i32,
    pub height: i32,
    /// Tight `width*height*4` pixels, row-major, top-left origin. Channel order is selected by `bgra`.
    pub rgba: Vec<u8>,
    /// The byte vector is BGRA rather than RGBA. Kept explicit so the macOS backend can consume native
    /// ARGB wl_shm memory without two full-frame channel swaps; headless capture canonicalizes on deposit.
    pub bgra: bool,
    /// Changed rectangles in buffer-pixel coordinates. `None` requests a conservative full upload.
    pub damage: Option<Vec<Rect>>,
}

impl StoredBuffer {
    /// Tight RGBA byte count, rejecting invalid or overflowing dimensions.
    pub(super) fn tight_bytes(&self) -> Option<usize> {
        if self.width <= 0 || self.height <= 0 {
            return None;
        }
        (self.width as i64)
            .checked_mul(self.height as i64)?
            .checked_mul(4)
            .and_then(|bytes| usize::try_from(bytes).ok())
    }

    /// Rotate or flip the client buffer into surface space.
    pub(super) fn transformed(&self, transform: BufferTransform) -> Vec<u8> {
        let bw = self.width as usize;
        let (ow, oh) = transform.surface_size(self.width, self.height);
        let (ow_u, oh_u) = (ow as usize, oh as usize);
        let mut out = vec![0u8; ow_u * oh_u * 4];
        for by in 0..self.height {
            for bx in 0..self.width {
                let (sx, sy) = transform.map_point(bx, by, self.width, self.height);
                let si = (by as usize * bw + bx as usize) * 4;
                let di = (sy as usize * ow_u + sx as usize) * 4;
                out[di..di + 4].copy_from_slice(&self.rgba[si..si + 4]);
            }
        }
        out
    }
}

/// Nearest-neighbour sample the source rectangle `src = (x, y, w, h)` (in BUFFER PIXELS) of `buf` into a
/// tight `dw`×`dh` RGBA image — the `wp_viewport` crop+scale a real backend rasterizes. Each destination
/// pixel maps through its center to a source coordinate, floored to a source texel (clamped in-bounds).
/// With integer crop rectangles and integer scale ratios the mapping is exact.
///
/// The caller ([`Presenter::present`]) gates dimensions: `buf` is non-empty and consistent, and
/// `dw`/`dh` are in `1..=MAX_PRESENT_DIM`, so `buf.height - 1` / `buf.width - 1` are non-negative (a
/// zero-dimension buffer would otherwise make `clamp(0, -1)` panic) and the `usize` index math cannot
/// overflow or slice out of bounds.
pub(super) fn resample_nearest(
    buf: &StoredBuffer,
    src: (f64, f64, f64, f64),
    dw: i32,
    dh: i32,
) -> Vec<u8> {
    let (sx, sy, sw, sh) = src;
    let bw = buf.width as usize;
    let (dw_u, dh_u) = (dw as usize, dh as usize);
    let mut out = vec![0u8; dw_u * dh_u * 4];
    for dy in 0..dh {
        let v = sy + (dy as f64 + 0.5) / dh as f64 * sh;
        let by = (v.floor() as i32).clamp(0, buf.height - 1);
        for dx in 0..dw {
            let u = sx + (dx as f64 + 0.5) / dw as f64 * sw;
            let bx = (u.floor() as i32).clamp(0, buf.width - 1);
            let si = (by as usize * bw + bx as usize) * 4;
            let di = (dy as usize * dw_u + dx as usize) * 4;
            out[di..di + 4].copy_from_slice(&buf.rgba[si..si + 4]);
        }
    }
    out
}
