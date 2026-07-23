use super::*;

/// The 128-byte std140 bytes of GskGpu's `PushConstants { mat4 mvp; mat3x4 clip; vec2 scale; }` for a render
/// pass targeting a `w`×`h` (device-pixel) target — reconstructed when GTK's GskGL renderer never delivers
/// the block's contents over any observed GL upload (see the call site in `lower_draw_n`).
///
/// std140 layout: `mat4 mvp` at offset 0 (four column vec4s, column-major), `mat3x4 clip` at offset 64 (three
/// column vec4s), `vec2 scale` at offset 112. The mvp is the orthographic projection GskGpu uses for a pass:
/// device-pixel coordinates with a top-left origin map to GL clip space, Y-flipped (`x' = 2x/w − 1`,
/// `y' = 1 − 2y/h`). The clip's first column carries the clip rect as `(x, y, w, h)` (the shader reads
/// `push.clip[0]` and forms bounds `(x, y, x+w, y+h)`); a full-target rect (with a 1px margin so edge coverage
/// is not trimmed) disables clipping. `scale` is the device pixel scale (1 on our compositor, so `in_rect`
/// logical units already equal device pixels).
impl Frame {
    pub(super) fn gsk_globals_std140(w: f32, h: f32) -> Vec<u8> {
        let mut m = [0f32; 32];
        // mat4 mvp @0 (column-major): cols 0..3 at floats 0,4,8,12.
        m[0] = 2.0 / w; // col0.x
        m[5] = -2.0 / h; // col1.y (top-left origin → GL clip space)
        m[10] = 1.0; // col2.z
        m[12] = -1.0; // col3.x
        m[13] = 1.0; // col3.y
        m[15] = 1.0; // col3.w
                     // mat3x4 clip @64 (floats 16..28): clip[0] = (x, y, w, h) covering the whole target; clip[1]/clip[2] = 0.
        m[16] = -1.0;
        m[17] = -1.0;
        m[18] = w + 2.0;
        m[19] = h + 2.0;
        // vec2 scale @112 (floats 28..30).
        m[28] = 1.0;
        m[29] = 1.0;
        m.iter().flat_map(|f| f.to_le_bytes()).collect()
    }
}
