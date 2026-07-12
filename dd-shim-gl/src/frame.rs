//! Frame assembly — lower the recorded draw-list into the dd-gpu IR stream at `eglSwapBuffers`,
//! mirroring `gl_shim.c`'s swap-time emission byte-for-byte.
//!
//! This increment implements the **clear path** (a frame whose draw-list is all clears — the
//! translator-free case), which is byte-identical to gl_shim.c and gated live by `tests/pixel_parity`.
//! Frames containing a real draw need the GLSL→shader translation + pipeline/bind-group assembly and
//! return `None` for now (the harness skips them with a notice); the structure they slot into is the
//! `replay`/single-draw emission documented in gl_shim.c's `eglSwapBuffers`.

use dd_shim_common::ir::{
    encode_stream, BlendState, Cmd, ColorAttachment, ColorTargetState, CommandBuffer, DepthAttachment, DepthState, Enc,
    IndexFormat, LoadOp, RenderPipelineDesc, ShaderRef, TextureFormat, Topology, VertexAttr, VertexLayout,
};

use crate::lower::{create_sampler_cmd, create_texture_cmd, index_buffer_cmds, texture_staging_cmds, uniform_buffer_cmds, vertex_buffer_cmds};
use crate::state::{DrawCall, GlState, MAXATTR, MAXBUF, MAXTEX};
use crate::translate::collect_vertex_attrs;
use crate::wireenc::{blend_factor_wire, blend_op_wire, decl_format_wire, tex_ir_id, vertex_format_wire};

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

/// Pack an MSL string into the `CreateShader` word vector (gl_shim.c `ir_shader`): `[len, bytes/4…]`.
fn msl_words(msl: &str) -> Vec<u32> {
    let bytes = msl.as_bytes();
    let len = bytes.len();
    let nwords = 1 + len.div_ceil(4);
    let mut w = Vec::with_capacity(nwords);
    w.push(len as u32);
    for i in 0..nwords - 1 {
        let mut b = [0u8; 4];
        let rem = len - i * 4;
        let take = rem.min(4);
        b[..take].copy_from_slice(&bytes[i * 4..i * 4 + take]);
        w.push(u32::from_le_bytes(b));
    }
    w
}

/// `emit_viewport_h` (gl_shim.c): a `SetViewport` with the GL→Metal Y-flip.
fn emit_viewport(s: &GlState, vp: [i32; 4], target_h: i32) -> Enc {
    let th = if target_h <= 0 { s.surf.height as i32 } else { target_h };
    let (mut x, mut y, mut w, mut h) = (0.0f32, 0.0f32, s.surf.width as f32, th as f32);
    if vp[2] > 0 && vp[3] > 0 {
        x = vp[0] as f32;
        w = vp[2] as f32;
        h = vp[3] as f32;
        y = (th - vp[1] - vp[3]) as f32;
    }
    Enc::SetViewport { x, y, w, h, min_depth: 0.0, max_depth: 1.0 }
}

/// `emit_scissor_h` (gl_shim.c): a `SetScissor` with the Y-flip + clamp to the target.
fn emit_scissor(s: &GlState, enabled: bool, sc: [i32; 4], target_w: i32, target_h: i32) -> Enc {
    let tw = if target_w <= 0 { s.surf.width as i32 } else { target_w };
    let th = if target_h <= 0 { s.surf.height as i32 } else { target_h };
    let (mut x, mut y, mut w, mut h) = (0, 0, tw, th);
    if enabled && sc[2] > 0 && sc[3] > 0 {
        x = sc[0];
        y = th - sc[1] - sc[3];
        w = sc[2];
        h = sc[3];
    }
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
    Enc::SetScissor { x: x as u32, y: y as u32, w: w as u32, h: h as u32 }
}

/// Lower a single-draw frame (one non-clear draw, default framebuffer) to IR, byte-equivalent to
/// gl_shim.c's non-replay `eglSwapBuffers` assembly. Emits the VBO/index/texture/uniform resources, the
/// translated shader + pipeline + bind group, and the render pass. Residency deltas are not modeled —
/// a fresh frame uploads everything, matching gl_shim.c's first frame.
fn build_single_draw_frame(s: &GlState) -> Option<Vec<u8>> {
    let d = &s.draws[0];
    let prog = s.prog.get(s.cur_prog as usize)?;
    if !prog.used {
        return None;
    }
    let msl = prog.msl.as_ref()?;
    let vsrc = s.sh.get(prog.vs as usize).and_then(|sh| sh.src.clone()).unwrap_or_default();
    let vdecl = collect_vertex_attrs(&vsrc);
    let ndecl = vdecl.len();

    // ---- vertex-buffer slot analysis (gl_shim.c) ----
    let mut slot_vbo: Vec<usize> = Vec::new();
    let mut attr_slot = [-1i32; MAXATTR];
    for i in 0..MAXATTR {
        let a = &s.attr[i];
        if !a.enabled {
            continue;
        }
        let b = a.buffer as usize;
        if a.buffer == 0 || b >= MAXBUF || !s.buf[b].used || s.buf[b].data.is_empty() {
            continue;
        }
        let sl = slot_vbo.iter().position(|&x| x == b).unwrap_or_else(|| {
            slot_vbo.push(b);
            slot_vbo.len() - 1
        });
        attr_slot[i] = sl as i32;
    }
    let nslot = slot_vbo.len();
    let mut slot_stride = vec![0u32; nslot.max(1)];
    for i in 0..MAXATTR {
        let sl = attr_slot[i];
        if sl < 0 {
            continue;
        }
        let mut st = s.attr[i].stride as u32;
        if st == 0 {
            st = s.attr[i].size as u32 * 4;
        }
        if st > slot_stride[sl as usize] {
            slot_stride[sl as usize] = st;
        }
    }
    for st in slot_stride.iter_mut() {
        if *st == 0 {
            *st = 24;
        }
    }
    let nvd = (0..MAXATTR).filter(|&i| s.attr[i].enabled).map(|i| i + 1).max().unwrap_or(0);

    // ---- texture list (samplers → bound units), no dedup in the single-draw path ----
    let mut texlist: Vec<u32> = Vec::new();
    for i in 0..prog.samp_names.len().min(4) {
        let unit = if (0..8).contains(&prog.samp_units[i]) { prog.samp_units[i] as usize } else { i };
        let tu = s.tex_unit[unit];
        if (tu as usize) < MAXTEX && s.tex[tu as usize].used && !s.tex[tu as usize].data.is_empty() {
            texlist.push(tu);
        }
    }
    let has_u = !prog.unis.is_empty();
    let has_bg = has_u || !texlist.is_empty();

    let mut cmds: Vec<Cmd> = Vec::new();

    // 1. vertex buffers (200 + slot)
    for (sl, &b) in slot_vbo.iter().enumerate() {
        cmds.extend(vertex_buffer_cmds(200 + sl as u32, &s.buf[b].data));
    }
    // 1b. index buffer (12)
    let indexed = d.indexed;
    if indexed {
        let eb = s.elem_buf as usize;
        if s.elem_buf > 0 && eb < MAXBUF && s.buf[eb].used && !s.buf[eb].data.is_empty() {
            cmds.extend(index_buffer_cmds(12, &s.buf[eb].data));
        }
    }
    // 1c. textures: CreateTexture + CreateSampler + staging upload
    for &t in &texlist {
        let tex = &s.tex[t as usize];
        cmds.push(create_texture_cmd(t, tex));
        cmds.push(create_sampler_cmd(t, tex));
        cmds.extend(texture_staging_cmds(t, tex));
    }
    // 2. shader (20) + pipeline (30)
    cmds.push(Cmd::CreateShader { id: 20, spirv: msl_words(msl) });
    let nvb = nslot.max(1);
    let mut vbs: Vec<VertexLayout> = Vec::with_capacity(nvb);
    for sl in 0..nvb {
        let mut attrs = Vec::new();
        for l in 0..nvd {
            let ls = if l < MAXATTR && attr_slot[l] >= 0 { attr_slot[l] } else { 0 };
            if ls as usize != sl {
                continue;
            }
            let (fmt, off) = if l < MAXATTR && s.attr[l].enabled && attr_slot[l] >= 0 {
                let a = &s.attr[l];
                (vertex_format_wire(a.kind, a.size, a.normalized, a.integer), a.offset as u32)
            } else {
                let t = if l < ndecl { vdecl[l].ty.as_str() } else { "vec4" };
                (decl_format_wire(t), 0)
            };
            attrs.push(VertexAttr { location: l as u32, format: fmt, offset: off });
        }
        let stride = if sl < nslot { slot_stride[sl] } else { 24 };
        vbs.push(VertexLayout { stride, step_mode: 0, attrs });
    }
    let blend = if s.blend {
        Some(BlendState {
            src_color: blend_factor_wire(s.blend_src_rgb),
            dst_color: blend_factor_wire(s.blend_dst_rgb),
            op_color: blend_op_wire(s.blend_eq_rgb),
            src_alpha: blend_factor_wire(s.blend_src_alpha),
            dst_alpha: blend_factor_wire(s.blend_dst_alpha),
            op_alpha: blend_op_wire(s.blend_eq_alpha),
        })
    } else {
        None
    };
    let depth = if s.depth {
        Some(DepthState { format: TextureFormat::Depth32Float, depth_write: true, depth_compare: 0 })
    } else {
        None
    };
    let topology = if d.mode == crate::glconst::GL_TRIANGLE_STRIP { Topology::TriangleStrip } else { Topology::TriangleList };
    cmds.push(Cmd::CreateRenderPipeline(
        30,
        RenderPipelineDesc {
            vertex: ShaderRef { module: 20, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
            vertex_buffers: vbs,
            color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend, write_mask: 0xf }],
            depth,
            topology,
            cull: 0,
            front_face: 0,
            label: String::new(),
        },
    ));
    // 2b. uniform buffer (11)
    if has_u {
        cmds.extend(uniform_buffer_cmds(11, &prog.ubuf[..prog.ubuf_size as usize]));
    }
    // bind group (40)
    if has_bg {
        let mut entries = Vec::new();
        if has_u {
            entries.push(dd_shim_common::ir::BindEntry {
                binding: 1,
                resource: dd_shim_common::ir::BindResource::Buffer { id: 11, offset: 0, size: prog.ubuf_size as u64 },
            });
        }
        for (k, &t) in texlist.iter().enumerate() {
            entries.push(dd_shim_common::ir::BindEntry {
                binding: k as u32,
                resource: dd_shim_common::ir::BindResource::Texture { id: tex_ir_id(t) },
            });
            entries.push(dd_shim_common::ir::BindEntry {
                binding: k as u32,
                resource: dd_shim_common::ir::BindResource::Sampler { id: crate::wireenc::sampler_ir_id(t) },
            });
        }
        cmds.push(Cmd::CreateBindGroup(40, dd_shim_common::ir::BindGroupDesc { set: 0, entries }));
    }

    // 3. Submit: [copies] + Begin + SetPipeline + Viewport + Scissor + [Bind] + SetVB* + [SetIB] + Draw + End
    let mut ops: Vec<Enc> = Vec::new();
    for &t in &texlist {
        let tex = &s.tex[t as usize];
        ops.push(Enc::CopyBufferToTexture {
            src: crate::wireenc::stage_ir_id(t),
            src_offset: 0,
            bytes_per_row: tex.w as u32 * 4,
            dst: tex_ir_id(t),
            mip: 0,
            width: tex.w as u32,
            height: tex.h as u32,
        });
    }
    let target = if d.target_tex != 0 && (d.target_tex as usize) < MAXTEX && s.tex[d.target_tex as usize].used {
        d.target_tex
    } else {
        0
    };
    let load = target == 0 && s.default_surface_valid && !s.default_full_clear_since_swap;
    let (tw, th) = (s.draw_target_w(target), s.draw_target_h(target));
    ops.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: if target != 0 { tex_ir_id(target) } else { 1 },
            load: if load { LoadOp::Load } else { LoadOp::Clear },
            clear: s.clear,
            store: true,
        }],
        depth: if s.depth {
            Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0 })
        } else {
            None
        },
    });
    ops.push(Enc::SetPipeline(30));
    ops.push(emit_viewport(s, s.viewport, th));
    ops.push(emit_scissor(s, s.scissor_enabled, s.scissor, tw, th));
    if has_bg {
        ops.push(Enc::SetBindGroup { index: 0, group: 40 });
    }
    for sl in 0..nslot {
        ops.push(Enc::SetVertexBuffer { slot: sl as u32, buffer: 200 + sl as u32, offset: 0 });
    }
    if indexed {
        let ifmt = if d.index_type == crate::glconst::GL_UNSIGNED_INT { IndexFormat::U32 } else { IndexFormat::U16 };
        ops.push(Enc::SetIndexBuffer { buffer: 12, offset: d.index_offset as u64, format: ifmt });
        ops.push(Enc::DrawIndexed { index_count: d.count as u32, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 });
    } else {
        ops.push(Enc::Draw { vertex_count: d.count as u32, instance_count: 1, first_vertex: d.first as u32, first_instance: 0 });
    }
    ops.push(Enc::EndRenderPass);

    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    Some(encode_stream(&cmds))
}

/// Assemble the frame's IR byte-stream. Handles a **clear-only** frame (all clears → `Submit([ClearRect
/// …])`) and a **single-draw** frame (one non-clear draw on the default framebuffer). Multi-draw /
/// clear+draw frames use gl_shim.c's `replay` path (not yet ported) and return `None`.
pub fn build_frame_ir(s: &GlState) -> Option<Vec<u8>> {
    if !s.surf.have || s.draws.is_empty() {
        return None;
    }
    if s.draws.iter().all(|d| d.is_clear) {
        let ops: Vec<Enc> = s.draws.iter().map(|d| emit_clear_rect(s, d)).collect();
        return Some(encode_stream(&[Cmd::Submit(CommandBuffer { encoder: ops, signal: None })]));
    }
    // Single non-clear draw on the default framebuffer → gl_shim.c's non-replay path.
    if s.draws.len() == 1 && !s.draws[0].is_clear && s.draw_mode >= 0 {
        return build_single_draw_frame(s);
    }
    None // multi-draw / clear+draw → replay path (next)
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
