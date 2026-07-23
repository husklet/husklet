use super::*;

/// Reads committed Wayland buffers into the neutral compositor's stored RGBA representation.
pub(super) struct BufferReader<'a> {
    buffer: &'a WlBuffer,
}

impl<'a> BufferReader<'a> {
    pub(super) fn new(buffer: &'a WlBuffer) -> Self {
        Self { buffer }
    }

    pub(super) fn shm_rgba(&self) -> Option<(StoredBuffer, Format)> {
        let result = with_buffer_contents(self.buffer, |ptr, len, data| {
            let (w, h, stride, offset) = (data.width, data.height, data.stride, data.offset);
            // `w * 4` is computed in `i64` so a hostile width near `i32::MAX` (reachable with a large `wl_shm`
            // pool) cannot overflow the row-stride check itself before the geometry is rejected.
            if w <= 0 || h <= 0 || (stride as i64) < w as i64 * 4 || offset < 0 {
                return None;
            }
            // Highest byte read = offset + (h-1)*stride + w*4; must fit the mapping. All widened to `usize`
            // (each factor is now known positive) so the bound check can never overflow before it fires.
            let last_row = offset as usize + (h as usize - 1) * stride as usize;
            if last_row
                .checked_add(w as usize * 4)
                .map(|m| m > len)
                .unwrap_or(true)
            {
                return None;
            }
            // `format` is the neutral opaque/alpha distinction (drives blend); `swap_rb` selects channel
            // order; `has_alpha` whether the 4th byte is honoured or forced opaque.
            let (format, bgra, has_alpha) = match data.format {
                wl_shm::Format::Xrgb8888 => (Format::Xrgb8888, true, false),
                wl_shm::Format::Abgr8888 => (Format::Argb8888, false, true),
                wl_shm::Format::Xbgr8888 => (Format::Xrgb8888, false, false),
                // Argb8888 and any other advertised/unknown format fall through to ARGB semantics.
                _ => (Format::Argb8888, true, true),
            };
            // `w`/`h` are positive and `w·h·4 <= len` (the bound check above), so this `usize` product is
            // bounded by the mapping size and cannot overflow.
            let mut rgba = vec![0u8; w as usize * h as usize * 4];
            for y in 0..h {
                let row = offset as isize + y as isize * stride as isize;
                let di = y as usize * w as usize * 4;
                // Both supported channel orders already match a backend-native four-byte layout. Copy each
                // tight row wholesale; only XRGB/XBGR need a small alpha-fix pass. This replaces the former
                // scalar BGRA→RGBA conversion which macOS immediately reversed before upload.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr.offset(row),
                        rgba.as_mut_ptr().add(di),
                        w as usize * 4,
                    );
                }
                if !has_alpha {
                    for pixel in rgba[di..di + w as usize * 4].chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                }
            }
            Some((
                StoredBuffer {
                    width: w,
                    height: h,
                    rgba,
                    bgra,
                    damage: None,
                },
                format,
            ))
        });
        result.ok().flatten()
    }

    /// Turn a committed `wp_single_pixel_buffer_v1` `wl_buffer` into a tight 1×1 top-left RGBA8888 pixel — the
    /// solid-color quad Chrome/Ozone + video players attach without a shm pool. The buffer carries only a
    /// 4-channel color (no pixels, no fd); [`get_single_pixel_buffer`] returns it and `rgba8888()` collapses the
    /// 32-bit-per-channel wire values to 8-bit R,G,B,A (already the byte order [`StoredBuffer`] stores). A
    /// 1×1 buffer composites like any other — a client that also attaches a `wp_viewport` dst scales it to fill
    /// its surface. Returns `None` for any non-single-pixel buffer (the caller reaches this only after the shm
    /// and dmabuf reads both returned `None`). The neutral [`Format`] is opaque `Xrgb8888` when the color is
    /// fully opaque, else alpha `Argb8888`, so the presenter blends a translucent single-pixel quad correctly.
    pub(super) fn single_pixel_rgba(&self) -> Option<(StoredBuffer, Format)> {
        let data = get_single_pixel_buffer(self.buffer).ok()?;
        let rgba = data.rgba8888();
        let format = if data.has_alpha() {
            Format::Argb8888
        } else {
            Format::Xrgb8888
        };
        Some((
            StoredBuffer {
                width: 1,
                height: 1,
                rgba: rgba.to_vec(),
                bgra: false,
                damage: None,
            },
            format,
        ))
    }
}

/// The DRM format+modifier pairs the `zwp_linux_dmabuf_v1` global advertises AND the importer accepts:
/// single-plane **LINEAR** ARGB8888 + XRGB8888. LINEAR is the ONE modifier a software (no-GPU) presenter
/// can honestly import — the buffer is plain byte-linear memory the compositor `pread`s and unpacks like
/// shm. No tiled/vendor modifiers are advertised because there is no GPU here to detile them; a client
/// that needs those reads an empty tranche for them and falls back to `wl_shm`. The byte-swapped
/// ABGR/XBGR fourccs are additionally ACCEPTED at import (mirroring the shm read path) but not advertised,
/// since ARGB/XRGB is the universal pair GTK/Qt/Chrome negotiate.
fn dmabuf_formats() -> [DrmFormat; 2] {
    [
        DrmFormat {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        },
        DrmFormat {
            code: Fourcc::Xrgb8888,
            modifier: Modifier::Linear,
        },
    ]
}

/// The device advertised by dmabuf feedback is the virtual render node supplied by the composed Husklet
/// display runtime. Its DRM identity is stable even though allocations are host-backed IOSurfaces.
const HUSKLET_RENDER_NODE: DmabufDeviceId = DmabufDeviceId::from_linux_dev_t((226u64 << 8) | 128);

/// Stand up a v4 dmabuf global for the composed host-GPU path. Clients use the feedback device to find
/// `/dev/dri/renderD128`; the engine implements that node's discovery and allocation ioctls, while this
/// compositor imports the resulting linear/IOSurface-backed buffers. Keeping the device identity here
/// aligned with the engine wire contract is required for Chromium's Wayland buffer manager.
pub(super) struct DmabufAdapter<'a> {
    display: &'a DisplayHandle,
}

impl<'a> DmabufAdapter<'a> {
    pub(super) fn new(display: &'a DisplayHandle) -> Self {
        Self { display }
    }

    pub(super) fn state(&self) -> DmabufState {
        let mut state = DmabufState::new();
        match DmabufFeedbackBuilder::new(HUSKLET_RENDER_NODE, dmabuf_formats()).build() {
            Ok(feedback) => {
                let _global: DmabufGlobal =
                    state.create_global_with_default_feedback::<HlState>(self.display, &feedback);
            }
            Err(error) => {
                eprintln!(
                "hl-compositor: dmabuf feedback unavailable ({error}); advertising the v3 linear-import contract"
            );
                let _global: DmabufGlobal =
                    state.create_global::<HlState>(self.display, dmabuf_formats());
            }
        }
        state
    }
}

/// Read a committed **LINEAR** dmabuf `wl_buffer`'s pixels into tight top-left RGBA8888 by `pread`ing its
/// single plane fd — the genuine CPU import path for a byte-linear buffer on a software backend (no GPU
/// detile step). This is a REAL fd import: the bytes come off the client's plane fd, not a fabricated
/// copy. Returns `None` for a non-dmabuf buffer (the caller only reaches this after the shm read already
/// returned `None`), or for any dmabuf the importer would not have accepted (non-LINEAR
/// / multi-plane / unsupported fourcc / malformed geometry / backing too small). The four fourcc channel
/// orders map exactly as in [`read_shm_rgba`].
impl<'a> BufferReader<'a> {
    pub(super) fn dmabuf_rgba(&self) -> Option<(StoredBuffer, Format)> {
        use std::os::unix::fs::FileExt;
        let dmabuf = get_dmabuf(self.buffer).ok()?;
        let drm = dmabuf.format();
        // Only the single-plane LINEAR buffers we advertised/accepted are CPU-importable here.
        if drm.modifier != Modifier::Linear || dmabuf.num_planes() != 1 {
            return None;
        }
        let (format, swap_rb, has_alpha) = match drm.code {
            Fourcc::Xrgb8888 => (Format::Xrgb8888, false, false),
            Fourcc::Abgr8888 => (Format::Argb8888, true, true),
            Fourcc::Xbgr8888 => (Format::Xrgb8888, true, false),
            Fourcc::Argb8888 => (Format::Argb8888, false, true),
            // An unsupported fourcc should never reach here (import rejects it), but be defensive.
            _ => return None,
        };
        let (w, h) = (dmabuf.width() as i32, dmabuf.height() as i32);
        let stride = dmabuf.strides().next()? as i64;
        let offset = dmabuf.offsets().next()? as i64;
        // Same geometry guard as the shm path (all widened to i64 so a hostile near-i32::MAX width cannot
        // overflow the row-stride check before it fires): reject a stride/offset that under-describes a row.
        if w <= 0 || h <= 0 || stride < w as i64 * 4 || offset < 0 {
            return None;
        }
        // Highest byte we will read = offset + (h-1)*stride + w*4; compute the read span with checked math so
        // an overflowing geometry is rejected (None) rather than panicking.
        let span = offset
            .checked_add((h as i64 - 1).checked_mul(stride)?)?
            .checked_add(w as i64 * 4)?;
        // Duplicate the BORROWED plane fd (the `Dmabuf` keeps ownership) and `read_at` the pixel region. A
        // LINEAR dmabuf's plane fd is a plain CPU-readable memory object (here backed by the client's own
        // file/memfd), so `read_exact_at` on the dup'd fd is the no-mmap equivalent of the shm mapping read;
        // a short/undersized backing makes `read_exact_at` fail, which we map to `None` (backing too small).
        let fd = dmabuf.handles().next()?;
        let file = std::fs::File::from(fd.try_clone_to_owned().ok()?);
        let read_len = (span - offset) as usize;
        let mut raw = vec![0u8; read_len];
        file.read_exact_at(&mut raw, offset as u64).ok()?;
        // `raw[0]` corresponds to file offset `offset`, so pixel (x, y) begins at `raw[y*stride + x*4]`.
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h {
            let row = (y as i64 * stride) as usize;
            for x in 0..w {
                let si = row + (x as usize) * 4;
                let c0 = raw[si];
                let g = raw[si + 1];
                let c2 = raw[si + 2];
                let a = if has_alpha { raw[si + 3] } else { 255 };
                // ARGB memory is `[B, G, R, A]` (c0=B, c2=R); *BGR memory is `[R, G, B, A]` (c0=R, c2=B).
                let (r, b) = if swap_rb { (c0, c2) } else { (c2, c0) };
                let di = ((y * w + x) * 4) as usize;
                rgba[di] = r;
                rgba[di + 1] = g;
                rgba[di + 2] = b;
                rgba[di + 3] = a;
            }
        }
        Some((
            StoredBuffer {
                width: w,
                height: h,
                rgba,
                bgra: false,
                damage: None,
            },
            format,
        ))
    }
}
