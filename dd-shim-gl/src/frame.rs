//! Frame assembly — lower the recorded draw-list into the dd-gpu IR stream at `eglSwapBuffers`,
//! mirroring `gl_shim.c`'s swap-time emission byte-for-byte.
//!
//! This increment implements the **clear path** (a frame whose draw-list is all clears — the
//! translator-free case), which is byte-identical to gl_shim.c and gated live by `tests/pixel_parity`.
//! Frames containing a real draw need the GLSL→shader translation + pipeline/bind-group assembly and
//! return `None` for now (the harness skips them with a notice); the structure they slot into is the
//! `replay`/single-draw emission documented in gl_shim.c's `eglSwapBuffers`.

use dd_shim_common::ir::{encode_stream, Cmd, CommandBuffer, Enc};

use crate::state::{DrawCall, GlState, MAXTEX};
use crate::wireenc::tex_ir_id;

/// `emit_clear_rect` (gl_shim.c): a scissor/clear rect lowered to a `ClearRect` encoder op, with the
/// GL→Metal Y-flip (`y = target_h - y - h`) and clamping to the target.
fn emit_clear_rect(s: &GlState, d: &DrawCall) -> Enc {
    let mut target = d.target_tex;
    if target as usize >= MAXTEX || !s.tex.get(target as usize).map(|t| t.used).unwrap_or(false) {
        target = 0;
    }
    let (tw, th) = (s.draw_target_w(target), s.draw_target_h(target));
    let mut x = d.clear_rect[0];
    let mut y = th - d.clear_rect[1] - d.clear_rect[3];
    let mut w = d.clear_rect[2];
    let mut h = d.clear_rect[3];
    if x < 0 {
        w += x;
        x = 0;
    }
    if y < 0 {
        h += y;
        y = 0;
    }
    if x > tw {
        x = tw;
    }
    if y > th {
        y = th;
    }
    if x + w > tw {
        w = tw - x;
    }
    if y + h > th {
        h = th - y;
    }
    w = w.max(0);
    h = h.max(0);
    Enc::ClearRect {
        texture: if target != 0 { tex_ir_id(target) } else { 1 },
        x: x as u32,
        y: y as u32,
        w: w as u32,
        h: h as u32,
        color: d.clear,
    }
}

/// Assemble the frame's IR byte-stream, or `None` when the frame contains a real draw (shader/pipeline
/// path pending). A clear-only frame lowers to a single `Submit([ClearRect…])`, exactly as gl_shim.c.
pub fn build_frame_ir(s: &GlState) -> Option<Vec<u8>> {
    if !s.surf.have || s.draws.is_empty() {
        return None;
    }
    if s.draws.iter().any(|d| !d.is_clear) {
        return None; // real draw → needs the GLSL translator + pipeline emission (next step)
    }
    let ops: Vec<Enc> = s.draws.iter().map(|d| emit_clear_rect(s, d)).collect();
    Some(encode_stream(&[Cmd::Submit(CommandBuffer { encoder: ops, signal: None })]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_shim_common::wire::Encoder;

    #[test]
    fn clear_only_frame_is_byte_identical_to_c_shim() {
        // A full-window clear at 640x480 with color (0.1,0.2,0.3,1.0).
        let mut s = GlState::default();
        s.surf = crate::state::Surface { have: true, id: 1, width: 640, height: 480 };
        s.clear = [0.1, 0.2, 0.3, 1.0];
        // glClear(COLOR) with no scissor → full-target clear rect (as gl_shim.c records it).
        s.record_clear_call(0, 0, 640, 480);
        let got = build_frame_ir(&s).expect("clear-only frame");

        // gl_shim.c: iu8(19) iu32(1) [ iu8(17) iu32(1) iu32(0) iu32(0) iu32(640) iu32(480)
        //            ifl(.1) ifl(.2) ifl(.3) ifl(1) ] iu8(0)
        let mut e = Encoder::new();
        e.u8(19); // SUBMIT
        e.u32(1); // 1 op
        e.u8(17); // CLEAR_RECT
        e.u32(1); // texture id 1 (default surface)
        e.u32(0);
        e.u32(0);
        e.u32(640);
        e.u32(480);
        e.f32(0.1);
        e.f32(0.2);
        e.f32(0.3);
        e.f32(1.0);
        e.bool(false); // signal None
        assert_eq!(got, e.into_vec(), "clear-only IR must be byte-identical to gl_shim.c");
    }
}
