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

/// The program a draw renders with: its own linked program, else the currently-bound one (gl_shim.c
/// `dpr` fallback to `pr`).
fn dpr_idx(s: &GlState, d: &DrawCall) -> usize {
    if (d.prog as usize) < s.prog.len() && s.prog[d.prog as usize].used {
        d.prog as usize
    } else {
        s.cur_prog as usize
    }
}

/// Build the per-draw pipeline's vertex layouts (gl_shim.c replay pipeline emission).
fn replay_vertex_layouts(s: &GlState, d: &DrawCall, dpr: &crate::state::Program) -> Vec<VertexLayout> {
    let dvdecl = dpr
        .vs_src_from(s)
        .map(|src| collect_vertex_attrs(&src))
        .unwrap_or_default();
    let dnd = dvdecl.len();
    let mut dvcount = dnd;
    for i in 0..MAXATTR {
        if d.attrs[i].enabled && i + 1 > dvcount {
            dvcount = i + 1;
        }
    }
    let (_dslot_vbo, dattr_slot, dslot_stride) = s.draw_vbo_slots(d);
    let dnslot = dslot_stride.len();
    let nvb = dnslot.max(1);
    let mut vbs = Vec::with_capacity(nvb);
    for sl in 0..nvb {
        let mut attrs = Vec::new();
        for l in 0..dvcount.min(MAXATTR) {
            let ls = if dattr_slot[l] >= 0 { dattr_slot[l] } else { 0 };
            if ls as usize != sl {
                continue;
            }
            let (fmt, off) = if d.attrs[l].enabled && dattr_slot[l] >= 0 {
                let a = &d.attrs[l];
                (vertex_format_wire(a.kind, a.size, a.normalized, a.integer), a.offset as u32)
            } else {
                let t = if l < dnd { dvdecl[l].ty.as_str() } else { "vec4" };
                (decl_format_wire(t), 0)
            };
            attrs.push(VertexAttr { location: l as u32, format: fmt, offset: off });
        }
        let stride = if sl < dnslot { dslot_stride[sl] } else { 16 };
        vbs.push(VertexLayout { stride, step_mode: 0, attrs });
    }
    vbs
}

/// Lower a multi-draw / clear+draw frame (gl_shim.c's `replay` branch): per-draw resource ids
/// (`20+d`/`30+d`/`40+d`/`1000+d`/`2000+`/`10000+`), render-pass segmentation by target + clear serial,
/// and load/clear semantics between draws — field-for-field byte-equivalent to gl_shim.c.
fn build_replay_frame(s: &GlState) -> Option<Vec<u8>> {
    let draws = &s.draws;
    let pr = s.prog.get(s.cur_prog as usize)?;
    if pr.msl.is_none() {
        return None; // no shader yet (uncovered frame shape)
    }

    // ---- texture list: sampler-bound textures (deduped) then render-target textures ----
    let mut texlist: Vec<u32> = Vec::new();
    for d in draws.iter().filter(|d| !d.is_clear) {
        let dpr = &s.prog[dpr_idx(s, d)];
        for i in 0..dpr.samp_names.len().min(4) {
            let unit = if (0..8).contains(&d.samp_units[i]) { d.samp_units[i] as usize } else { i };
            let tu = d.tex_units[unit];
            if (tu as usize) < MAXTEX && s.tex[tu as usize].used && !s.tex[tu as usize].data.is_empty() && !texlist.contains(&tu) {
                texlist.push(tu);
            }
        }
    }
    for d in draws {
        let tu = d.target_tex;
        if tu != 0 && (tu as usize) < MAXTEX && s.tex[tu as usize].used && !s.tex[tu as usize].data.is_empty() && !texlist.contains(&tu) {
            texlist.push(tu);
        }
    }

    // ---- frame-level VBO/IBO fallback lists ----
    let mut frame_vbo: Vec<u32> = Vec::new();
    let mut frame_ibo: Vec<u32> = Vec::new();
    for d in draws.iter().filter(|d| !d.is_clear) {
        for i in 0..MAXATTR {
            let a = &d.attrs[i];
            let b = a.buffer as usize;
            if a.enabled && a.buffer != 0 && b < MAXBUF && s.buf[b].used && !s.buf[b].data.is_empty() && !frame_vbo.contains(&a.buffer) {
                frame_vbo.push(a.buffer);
            }
        }
        if d.indexed {
            let b = d.elem_buf as usize;
            if d.elem_buf > 0 && b < MAXBUF && s.buf[b].used && !s.buf[b].data.is_empty() && !frame_ibo.contains(&d.elem_buf) {
                frame_ibo.push(d.elem_buf);
            }
        }
    }

    let has_u = draws.iter().filter(|d| !d.is_clear).any(|d| !s.prog[dpr_idx(s, d)].unis.is_empty());
    let has_bg = has_u || !texlist.is_empty();

    let mut cmds: Vec<Cmd> = Vec::new();

    // 1. VBOs: per-draw snapshots (2000+), then frame fallback (200+).
    for (d_i, d) in draws.iter().enumerate() {
        if d.is_clear {
            continue;
        }
        let (dslot_vbo, _as, _st) = s.draw_vbo_slots(d);
        for (sl, &src) in dslot_vbo.iter().enumerate() {
            if let Some(si) = GlState::draw_vbo_snapshot_index(d, src) {
                let data = &d.snap_vbo[si].data;
                cmds.extend(vertex_buffer_cmds(crate::wireenc::replay_vbo_ir_id(d_i as u32, sl as u32), data));
            }
        }
    }
    for (k, &b) in frame_vbo.iter().enumerate() {
        cmds.extend(vertex_buffer_cmds(200 + k as u32, &s.buf[b as usize].data));
    }
    // 1b. index buffers: per-draw snapshots (10000+), then frame fallback (300+).
    for (d_i, d) in draws.iter().enumerate() {
        if d.is_clear || !d.indexed {
            continue;
        }
        if let Some(ibo) = &d.snap_ibo {
            if !ibo.data.is_empty() {
                cmds.extend(index_buffer_cmds(crate::wireenc::replay_ibo_ir_id(d_i as u32), &ibo.data));
            }
        }
    }
    for (k, &b) in frame_ibo.iter().enumerate() {
        cmds.extend(index_buffer_cmds(300 + k as u32, &s.buf[b as usize].data));
    }
    // 1c. textures.
    let mut tex_upload = vec![false; texlist.len()];
    for (k, &t) in texlist.iter().enumerate() {
        let tex = &s.tex[t as usize];
        cmds.push(create_texture_cmd(t, tex));
        cmds.push(create_sampler_cmd(t, tex));
        cmds.extend(texture_staging_cmds(t, tex));
        tex_upload[k] = true; // fresh frame → uploaded
    }
    // 2. per-draw shader (20+d) + pipeline (30+d).
    for (d_i, d) in draws.iter().enumerate() {
        if d.is_clear {
            continue;
        }
        let dpr = &s.prog[dpr_idx(s, d)];
        let msl = match &dpr.msl {
            Some(m) => m,
            None => continue,
        };
        cmds.push(Cmd::CreateShader { id: 20 + d_i as u32, spirv: msl_words(msl) });
        let vbs = replay_vertex_layouts(s, d, dpr);
        let blend = if d.blend {
            Some(BlendState {
                src_color: blend_factor_wire(d.blend_src_rgb),
                dst_color: blend_factor_wire(d.blend_dst_rgb),
                op_color: blend_op_wire(d.blend_eq_rgb),
                src_alpha: blend_factor_wire(d.blend_src_alpha),
                dst_alpha: blend_factor_wire(d.blend_dst_alpha),
                op_alpha: blend_op_wire(d.blend_eq_alpha),
            })
        } else {
            None
        };
        let fmt = TextureFormat::from_u32(crate::wireenc::color_target_format(d.target_tex)).unwrap();
        let depth = if s.depth {
            Some(DepthState { format: TextureFormat::Depth32Float, depth_write: true, depth_compare: 0 })
        } else {
            None
        };
        let topology = if d.mode == crate::glconst::GL_TRIANGLE_STRIP { Topology::TriangleStrip } else { Topology::TriangleList };
        cmds.push(Cmd::CreateRenderPipeline(
            30 + d_i as u32,
            RenderPipelineDesc {
                vertex: ShaderRef { module: 20 + d_i as u32, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20 + d_i as u32, entry: "fmain".into() }),
                vertex_buffers: vbs,
                color_targets: vec![ColorTargetState { format: fmt, blend, write_mask: 0xf }],
                depth,
                topology,
                cull: 0,
                front_face: 0,
                label: String::new(),
            },
        ));
    }
    // 2b. per-draw uniform buffers (1000+d).
    if has_u {
        for (d_i, d) in draws.iter().enumerate() {
            if d.is_clear {
                continue;
            }
            let dpr = &s.prog[dpr_idx(s, d)];
            if dpr.unis.is_empty() {
                continue;
            }
            cmds.extend(crate::lower::uniform_buffer_cmds(1000 + d_i as u32, &d.ubuf[..dpr.ubuf_size as usize]));
        }
    }
    // per-draw bind groups (40+d).
    if has_bg {
        for (d_i, d) in draws.iter().enumerate() {
            if d.is_clear {
                continue;
            }
            let dpr = &s.prog[dpr_idx(s, d)];
            let mut dtex: Vec<usize> = Vec::new();
            for i in 0..dpr.samp_names.len().min(4) {
                let unit = if (0..8).contains(&d.samp_units[i]) { d.samp_units[i] as usize } else { i };
                let bound = d.tex_units[unit];
                if let Some(ti) = texlist.iter().position(|&x| x == bound) {
                    dtex.push(ti);
                }
            }
            let draw_has_u = !dpr.unis.is_empty();
            let mut entries = Vec::new();
            if draw_has_u {
                entries.push(dd_shim_common::ir::BindEntry {
                    binding: 1,
                    resource: dd_shim_common::ir::BindResource::Buffer { id: 1000 + d_i as u32, offset: 0, size: dpr.ubuf_size as u64 },
                });
            }
            for (k, &ti) in dtex.iter().enumerate() {
                let gltex = texlist[ti];
                entries.push(dd_shim_common::ir::BindEntry { binding: k as u32, resource: dd_shim_common::ir::BindResource::Texture { id: tex_ir_id(gltex) } });
                entries.push(dd_shim_common::ir::BindEntry { binding: k as u32, resource: dd_shim_common::ir::BindResource::Sampler { id: crate::wireenc::sampler_ir_id(gltex) } });
            }
            cmds.push(Cmd::CreateBindGroup(40 + d_i as u32, dd_shim_common::ir::BindGroupDesc { set: 0, entries }));
        }
    }

    // 3. Submit: copies + segmented passes.
    let mut ops: Vec<Enc> = Vec::new();
    for (k, &t) in texlist.iter().enumerate() {
        if !tex_upload[k] {
            continue;
        }
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
    let mut seen_default = false;
    let mut seen_tex = vec![false; MAXTEX];
    let mut clear_default: i32 = -1;
    let mut clear_tex = vec![-1i32; MAXTEX];
    let norm_target = |t: u32| -> u32 {
        if t != 0 && (t as usize) < MAXTEX && s.tex[t as usize].used {
            t
        } else {
            0
        }
    };
    let mut d_i = 0usize;
    while d_i < draws.len() {
        let d = &draws[d_i];
        if d.is_clear {
            ops.push(emit_clear_rect(s, d));
            d_i += 1;
            continue;
        }
        let target = norm_target(d.target_tex);
        let seg_clear = d.clear_serial;
        let seen = if target != 0 { seen_tex[target as usize] } else { seen_default };
        let last_clear = if target != 0 { clear_tex[target as usize] } else { clear_default };
        let mut load = seen && seg_clear == last_clear;
        if target == 0 && !seen && s.default_surface_valid && !s.default_full_clear_since_swap {
            load = true;
        }
        if target != 0 {
            seen_tex[target as usize] = true;
            clear_tex[target as usize] = seg_clear;
        } else {
            seen_default = true;
            clear_default = seg_clear;
        }
        let (tw, th) = (s.draw_target_w(target), s.draw_target_h(target));
        ops.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: if target != 0 { tex_ir_id(target) } else { 1 },
                load: if load { LoadOp::Load } else { LoadOp::Clear },
                clear: d.clear,
                store: true,
            }],
            depth: if s.depth {
                Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0 })
            } else {
                None
            },
        });
        // draw segment: all consecutive draws with the same target + clear serial
        while d_i < draws.len() {
            let d = &draws[d_i];
            if d.is_clear || norm_target(d.target_tex) != target || d.clear_serial != seg_clear {
                break;
            }
            ops.push(Enc::SetPipeline(30 + d_i as u32));
            ops.push(emit_viewport(s, d.viewport, th));
            ops.push(emit_scissor(s, d.scissor_enabled, d.scissor, tw, th));
            if has_bg {
                ops.push(Enc::SetBindGroup { index: 0, group: 40 + d_i as u32 });
            }
            let (dslot_vbo, _as, _st) = s.draw_vbo_slots(d);
            for (sl, &src) in dslot_vbo.iter().enumerate() {
                let buffer = if GlState::draw_vbo_snapshot_index(d, src).is_some() {
                    crate::wireenc::replay_vbo_ir_id(d_i as u32, sl as u32)
                } else {
                    200 + frame_vbo.iter().position(|&x| x == src).unwrap_or(0) as u32
                };
                ops.push(Enc::SetVertexBuffer { slot: sl as u32, buffer, offset: 0 });
            }
            if d.indexed {
                let ifmt = if d.index_type == crate::glconst::GL_UNSIGNED_INT { IndexFormat::U32 } else { IndexFormat::U16 };
                let buffer = if d.snap_ibo.is_some() {
                    crate::wireenc::replay_ibo_ir_id(d_i as u32)
                } else {
                    300 + frame_ibo.iter().position(|&x| x == d.elem_buf).unwrap_or(0) as u32
                };
                ops.push(Enc::SetIndexBuffer { buffer, offset: d.index_offset as u64, format: ifmt });
                ops.push(Enc::DrawIndexed { index_count: d.count as u32, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 });
            } else {
                ops.push(Enc::Draw { vertex_count: d.count as u32, instance_count: 1, first_vertex: d.first as u32, first_instance: 0 });
            }
            d_i += 1;
        }
        ops.push(Enc::EndRenderPass);
    }

    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    Some(encode_stream(&cmds))
}

/// Assemble the frame's IR byte-stream. Handles a **clear-only** frame (all clears → `Submit([ClearRect
/// …])`), a **single-draw** frame (one non-clear draw), and a **multi-draw / clear+draw** frame (the
/// `replay` path) — each byte-equivalent to gl_shim.c's `eglSwapBuffers`.
pub fn build_frame_ir(s: &GlState) -> Option<Vec<u8>> {
    if !s.surf.have || s.draws.is_empty() {
        return None;
    }
    // gl_shim.c: replay_draws = ndraws>1 || (ndraws==1 && draws[0].is_clear).
    let replay = s.draws.len() > 1 || (s.draws.len() == 1 && s.draws[0].is_clear);
    if replay {
        if s.draws.iter().all(|d| d.is_clear) {
            let ops: Vec<Enc> = s.draws.iter().map(|d| emit_clear_rect(s, d)).collect();
            return Some(encode_stream(&[Cmd::Submit(CommandBuffer { encoder: ops, signal: None })]));
        }
        return build_replay_frame(s);
    }
    // Single non-clear draw → gl_shim.c's non-replay path.
    if s.draw_mode >= 0 {
        return build_single_draw_frame(s);
    }
    None
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
