/// Barycentric weights of pixel-center `c` inside framebuffer-space triangle `fb` (signed area `area`),
/// or `None` if the pixel is outside. Matches the two-sided inside test the oracle uses.
use super::*;

pub(super) fn barycentric(fb: &[[f32; 2]; 3], c: [f32; 2], area: f32) -> Option<[f32; 3]> {
    let e0 = edge(fb[1], fb[2], c);
    let e1 = edge(fb[2], fb[0], c);
    let e2 = edge(fb[0], fb[1], c);
    let inside = (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0);
    if !inside {
        return None;
    }
    Some([e0 / area, e1 / area, e2 / area])
}

/// Composite one source fragment into a target texel: premultiplied linear-light source-over when the
/// target's blend is enabled, else an opaque replace. Extracted so the depth-tested and depth-less paths
/// share byte-identical color math.
///
/// The RGBA `write_mask` (low 4 bits `R<<0|G<<1|B<<2|A<<3`, matching the executor's `ColorWrites` mapping)
/// gates which logical channels the write reaches: a channel whose bit is CLEAR keeps its prior value. The
/// neutral `0xF` writes every channel and takes the fast path — byte-for-byte the pre-mask behavior; the
/// composite (blend or replace) is computed in full and then the masked-out channels are restored, so
/// masking composes correctly with both the blend and the sRGB encode.
pub(super) fn write_fragment(
    texel: &mut [u8],
    order: [usize; 4],
    srgb: bool,
    fmt: TextureFormat,
    blend_enabled: bool,
    write_mask: u32,
    src: [f32; 4],
) -> Result<()> {
    // Snapshot the prior per-channel bytes so masked-out channels can be restored after the write. Only
    // taken when a channel is actually masked out (`write_mask != 0xF`), keeping the neutral path allocation-
    // and copy-free.
    let masked = write_mask & 0xF != 0xF;
    let saved: [u8; 4] = if masked {
        [
            texel[order[0]],
            texel[order[1]],
            texel[order[2]],
            texel[order[3]],
        ]
    } else {
        [0; 4]
    };
    if blend_enabled {
        let a = src[3].clamp(0.0, 1.0);
        let s_lin = |k: usize| {
            if srgb {
                TextureFormat::srgb_to_linear(src[k].clamp(0.0, 1.0))
            } else {
                src[k].clamp(0.0, 1.0)
            }
        };
        let dst = load_texel_linear(texel, order, srgb);
        let out = [
            s_lin(0) * a + dst[0] * (1.0 - a),
            s_lin(1) * a + dst[1] * (1.0 - a),
            s_lin(2) * a + dst[2] * (1.0 - a),
            a + dst[3] * (1.0 - a),
        ];
        store_texel_linear(texel, order, srgb, out);
    } else {
        let bytes = fmt.clear_texel(src)?;
        texel.copy_from_slice(&bytes);
    }
    // Restore the channels the mask leaves untouched (logical R,G,B,A at byte indices `order[0..4]`).
    if masked {
        for k in 0..4 {
            if (write_mask >> k) & 1 == 0 {
                texel[order[k]] = saved[k];
            }
        }
    }
    Ok(())
}

/// One vertex the software oracle's draw path consumes: NDC position `(x, y)` at byte 0, a per-fragment
/// depth `z`, and a straight-alpha color. The stride selects the layout (all offsets little-endian f32):
///
/// * `>= 28`: `x,y,z` at 0/4/8, `color` (rgba) at 12/16/20/24  — 3D position + color (depth-tested draws)
/// * `>= 24`: `x,y`   at 0/4,   `color` (rgba) at 8/12/16/20   — the historical 2D-pos+color layout (`z=0`)
/// * `>= 12`: `x,y,z` at 0/4/8, color defaults to opaque white — 3D position only
/// * else   : `x,y`   at 0/4,   color defaults to opaque white — 2D position only (`z=0`)
///
/// The `>= 24` arm is byte-for-byte the pre-depth behavior, so existing color draws are unchanged; the z
/// component is an additive extension gated on the larger strides.
#[derive(Clone, Copy)]
pub(super) struct DrawVertex {
    pub(super) pos: [f32; 2],
    pub(super) z: f32,
    pub(super) color: [f32; 4],
}

impl DrawVertex {
    /// Barycentric-interpolate this triangle's straight-alpha vertex color at a fragment.
    pub(super) fn interpolate_color(vertices: &[Self; 3], bary: [f32; 3]) -> [f32; 4] {
        let mut color = [0f32; 4];
        for (channel, value) in color.iter_mut().enumerate() {
            *value = bary[0] * vertices[0].color[channel]
                + bary[1] * vertices[1].color[channel]
                + bary[2] * vertices[2].color[channel];
        }
        color
    }
}

pub(super) fn read_vertex(data: &[u8], base: usize, stride: usize) -> DrawVertex {
    // Bounds-tolerant read: an offset past the slice yields 0.0 (submit-time validation already ensures a
    // valid vertex fits, so this only guards against a defensive over-read, never a real payload).
    let f = |o: usize| {
        let i = base + o;
        if i + 4 <= data.len() {
            f32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        } else {
            0.0
        }
    };
    let pos = [f(0), f(4)];
    let (z, color) = if stride >= 28 {
        (f(8), [f(12), f(16), f(20), f(24)])
    } else if stride >= 24 {
        (0.0, [f(8), f(12), f(16), f(20)])
    } else if stride >= 12 {
        (f(8), [1.0, 1.0, 1.0, 1.0])
    } else {
        (0.0, [1.0, 1.0, 1.0, 1.0])
    };
    DrawVertex { pos, z, color }
}

/// Map an NDC position (x right, y up, in [-1,1]) to framebuffer pixel space (origin top-left, y down).
pub(super) fn ndc_to_fb(p: [f32; 2], w: usize, h: usize) -> [f32; 2] {
    [(p[0] * 0.5 + 0.5) * w as f32, (0.5 - p[1] * 0.5) * h as f32]
}

/// Signed area (×2) of triangle `a,b,c` — the edge function used for barycentric coverage.
pub(super) fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Integer pixel bounding box `[minx,maxx) × [miny,maxy)` of a framebuffer-space triangle, clamped to the
/// target dimensions.
pub(super) fn tri_bbox(fb: &[[f32; 2]; 3], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let minxf = fb[0][0].min(fb[1][0]).min(fb[2][0]);
    let maxxf = fb[0][0].max(fb[1][0]).max(fb[2][0]);
    let minyf = fb[0][1].min(fb[1][1]).min(fb[2][1]);
    let maxyf = fb[0][1].max(fb[1][1]).max(fb[2][1]);
    let minx = (minxf.floor().max(0.0) as i64).clamp(0, w as i64) as usize;
    let miny = (minyf.floor().max(0.0) as i64).clamp(0, h as i64) as usize;
    let maxx = (maxxf.ceil() as i64).clamp(0, w as i64) as usize;
    let maxy = (maxyf.ceil() as i64).clamp(0, h as i64) as usize;
    (minx, miny, maxx, maxy)
}
