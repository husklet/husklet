//! Frame assembly — lower the recorded draw-list into the frame's `Cmd` stream at `eglSwapBuffers`.
//!
//! Ported (simplified to the core paths) from `hl-shim-gl/src/frame.rs` (`build_frame_ir`) +
//! `hl-shim-gl/src/lower.rs`. Unlike the C/shim path — which encoded straight to wire bytes — this
//! produces a `Vec<hl_gpu::Cmd>` so [`crate::service::swap`] can submit it through a
//! [`hl_gpu::CommandSink`] (the tested seam), exactly as cuda's services submit `Cmd`s.
//!
//! Two frame shapes are FULLY lowered this pass:
//! * **clear-only** — a frame whose draw-list is all `glClear`s → a render pass that clears the default
//!   target (mirrors gl_shim.c's `ClearRect`-only submit).
//! * **single-draw** — one geometry draw against the default framebuffer → the VBO/index/texture/uniform
//!   uploads + the translated shader + pipeline + bind group + the render pass.
//!
//! Deferred (returns `None`, the caller no-ops the present): multi-draw / clear+draw **replay** frames,
//! offscreen-FBO render targets, and residency-delta upload skipping — the `hl-shim-gl` `build_replay_frame`
//! path, which is wiring on top of this same lowering and lands in a later pass.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use crate::model::program::DrawCall;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, SurfaceDesc, TextureDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{Cmd, CommandBuffer, ShaderPayloadKind};

/// The assembled frame: the `Cmd` stream to submit, plus the `(surface, texture)` to `Present` at the
/// end. Returned by [`build_frame_ir`] for [`crate::service::swap`] to ship.
pub struct Frame {
    /// The resource + submit commands, in submission order (no `Present` — swap appends that).
    pub cmds: Vec<Cmd>,
    /// The default-surface + its render-target texture IR ids to `Present`.
    pub present: (u32, u32),
}

/// Assemble the frame's `Cmd` stream from the recorded draw-list, or `None` if there is nothing (or
/// nothing yet supported) to present. Mints the IR ids it needs from `ctx`.
pub fn build_frame_ir(ctx: &mut GlContext) -> Option<Frame> {
    if !ctx.surf.have || ctx.draws.is_empty() {
        return None;
    }
    let all_clears = ctx.draws.iter().all(|d| d.is_clear);
    if all_clears {
        return Some(build_clear_frame(ctx));
    }
    // A single non-clear draw → the core single-draw path. Multi-draw/replay is deferred.
    if ctx.draws.len() == 1 && !ctx.draws[0].is_clear {
        return build_single_draw_frame(ctx);
    }
    None
}

/// The default render target (mint its `CreateTexture` + `CreateSurface` on first use). Returns
/// `(surface_ir, texture_ir)` and pushes the create commands into `cmds` when they are first needed.
fn ensure_default_target(ctx: &mut GlContext, cmds: &mut Vec<Cmd>) -> (u32, u32) {
    let (w, h) = ctx.target_wh();
    let (surface, texture, needs_create) = ctx.default_target();
    if needs_create {
        cmds.push(Cmd::CreateTexture(
            texture,
            TextureDesc {
                width: w.max(1) as u32,
                height: h.max(1) as u32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT,
                label: "default-fbo".into(),
            },
        ));
        cmds.push(Cmd::CreateSurface(
            surface,
            SurfaceDesc { width: w.max(1) as u32, height: h.max(1) as u32, format: TextureFormat::Bgra8Unorm, hlp_surface: 0 },
        ));
    }
    (surface, texture)
}

/// Clear-only frame: a render pass over the default target that clears it (`LoadOp::Clear`).
fn build_clear_frame(ctx: &mut GlContext) -> Frame {
    let mut cmds: Vec<Cmd> = Vec::new();
    let (surface, texture) = ensure_default_target(ctx, &mut cmds);
    let clear = ctx.draws.last().map(|d| d.clear).unwrap_or([0.0; 4]);
    let ops = vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment { texture, load: LoadOp::Clear, clear, store: true }],
            depth: None,
        },
        Enc::EndRenderPass,
    ];
    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    Frame { cmds, present: (surface, texture) }
}

/// Single-draw frame: the full textured-geometry lowering (VBO + index + textures + shader + pipeline +
/// bind group + the render pass). Byte-shape mirrors gl_shim.c's non-replay `eglSwapBuffers`.
fn build_single_draw_frame(ctx: &mut GlContext) -> Option<Frame> {
    let d = ctx.draws[0].clone();
    let prog_name = if d.prog != 0 { d.prog } else { ctx.cur_prog };
    let prog = ctx.programs.program(prog_name)?.clone();
    let shader_ir = prog.shader_ir.clone()?;
    let vdecl = crate::adapter::glsl::collect_vertex_attrs(&prog.vs_src);
    let ndecl = vdecl.len();

    let mut cmds: Vec<Cmd> = Vec::new();
    let (surface, target_tex) = ensure_default_target(ctx, &mut cmds);

    // ---- vertex-buffer slot analysis (dedup bound buffers into slots) ----
    let mut slot_gl_buf: Vec<u32> = Vec::new();
    let mut attr_slot = [-1i32; crate::model::program::MAX_ATTR];
    for (i, a) in d.attrs.iter().enumerate() {
        if !a.enabled || a.buffer == 0 || !ctx.buffers.has_data(a.buffer) {
            continue;
        }
        let sl = slot_gl_buf.iter().position(|&x| x == a.buffer).unwrap_or_else(|| {
            slot_gl_buf.push(a.buffer);
            slot_gl_buf.len() - 1
        });
        attr_slot[i] = sl as i32;
    }
    let nslot = slot_gl_buf.len();
    let mut slot_stride = vec![0u32; nslot.max(1)];
    for (i, a) in d.attrs.iter().enumerate() {
        let sl = attr_slot[i];
        if sl < 0 {
            continue;
        }
        let mut st = a.stride as u32;
        if st == 0 {
            st = a.size as u32 * 4;
        }
        if st > slot_stride[sl as usize] {
            slot_stride[sl as usize] = st;
        }
    }
    for st in slot_stride.iter_mut() {
        if *st == 0 {
            *st = 16;
        }
    }
    let nvd = d.attrs.iter().enumerate().filter(|(_, a)| a.enabled).map(|(i, _)| i + 1).max().unwrap_or(0);

    // Mint IR buffer ids for the vertex slots + emit their uploads.
    let mut slot_ir: Vec<u32> = Vec::with_capacity(nslot);
    for &gl_buf in &slot_gl_buf {
        let ir = ctx.alloc_buffer_ir();
        slot_ir.push(ir);
        let data = ctx.buffers.get(gl_buf).map(|b| b.data.clone()).unwrap_or_default();
        cmds.push(Cmd::CreateBuffer(ir, BufferDesc { size: data.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: ir, offset: 0, data });
    }

    // Index buffer.
    let mut index_ir = 0u32;
    if d.indexed && d.elem_buf != 0 && ctx.buffers.has_data(d.elem_buf) {
        index_ir = ctx.alloc_buffer_ir();
        let data = ctx.buffers.get(d.elem_buf).map(|b| b.data.clone()).unwrap_or_default();
        cmds.push(Cmd::CreateBuffer(index_ir, BufferDesc { size: data.len() as u64, usage: buffer_usage::INDEX, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: index_ir, offset: 0, data });
    }

    // ---- sampler-bound textures ----
    struct TexBind {
        tex_ir: u32,
        samp_ir: u32,
        stage_ir: u32,
        w: u32,
        h: u32,
    }
    let mut texbinds: Vec<TexBind> = Vec::new();
    for i in 0..prog.samp_names.len().min(4) {
        let unit = if (0..8).contains(&prog.samp_units[i]) { prog.samp_units[i] as usize } else { i };
        let gl_tex = d.tex_units[unit];
        let t = match ctx.textures.get(gl_tex) {
            Some(t) if t.has_data() => t.clone(),
            _ => continue,
        };
        let tex_ir = ctx.alloc_texture_ir();
        let samp_ir = ctx.alloc_sampler_ir();
        let stage_ir = ctx.alloc_buffer_ir();
        cmds.push(Cmd::CreateTexture(
            tex_ir,
            TextureDesc {
                width: t.w as u32,
                height: t.h as u32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::CreateSampler(
            samp_ir,
            SamplerDesc {
                min_filter: t.ir_min_filter(),
                mag_filter: t.ir_mag_filter(),
                mip_filter: Filter::Nearest,
                address_u: t.ir_wrap_s(),
                address_v: t.ir_wrap_t(),
                address_w: AddressMode::ClampToEdge,
            },
        ));
        cmds.push(Cmd::CreateBuffer(stage_ir, BufferDesc { size: t.data.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: stage_ir, offset: 0, data: t.data.clone() });
        texbinds.push(TexBind { tex_ir, samp_ir, stage_ir, w: t.w as u32, h: t.h as u32 });
    }
    let has_u = prog.has_uniforms();
    let has_bg = has_u || !texbinds.is_empty();

    // ---- shader + pipeline ----
    let shader_ir_id = ctx.alloc_shader_ir();
    cmds.push(Cmd::CreateShader { id: shader_ir_id, kind: ShaderPayloadKind::LegacyMsl, spirv: shader_ir });

    let nvb = nslot.max(1);
    let mut vbs: Vec<VertexLayout> = Vec::with_capacity(nvb);
    for sl in 0..nvb {
        let mut attrs = Vec::new();
        for l in 0..nvd {
            let ls = if l < crate::model::program::MAX_ATTR && attr_slot[l] >= 0 { attr_slot[l] } else { 0 };
            if ls as usize != sl {
                continue;
            }
            let (fmt, off) = if l < crate::model::program::MAX_ATTR && d.attrs[l].enabled && attr_slot[l] >= 0 {
                let a = &d.attrs[l];
                (vertex_format_wire(a.kind, a.size, a.normalized, a.integer), a.offset as u32)
            } else {
                let t = if l < ndecl { vdecl[l].ty.as_str() } else { "vec4" };
                (decl_format_wire(t), 0)
            };
            attrs.push(VertexAttr { location: l as u32, format: fmt, offset: off });
        }
        let stride = if sl < nslot { slot_stride[sl] } else { 16 };
        vbs.push(VertexLayout { stride, step_mode: 0, attrs });
    }
    let blend = if d.blend {
        // Default GL_FUNC_ADD SRC_ALPHA/ONE_MINUS_SRC_ALPHA-style state; the wire values are opaque
        // WebGPU factors (1 = One as a neutral default for this pass).
        Some(BlendState { src_color: 1, dst_color: 1, op_color: 0, src_alpha: 1, dst_alpha: 1, op_alpha: 0 })
    } else {
        None
    };
    let topology = if d.mode == GL_TRIANGLE_STRIP { Topology::TriangleStrip } else { Topology::TriangleList };
    let pipeline_ir = ctx.alloc_pipeline_ir();
    cmds.push(Cmd::CreateRenderPipeline(
        pipeline_ir,
        RenderPipelineDesc {
            vertex: ShaderRef { module: shader_ir_id, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: shader_ir_id, entry: "fmain".into() }),
            vertex_buffers: vbs,
            color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend, write_mask: 0xf }],
            depth: None,
            topology,
            cull: 0,
            front_face: 0,
            label: String::new(),
        },
    ));

    // ---- uniform buffer + bind group ----
    let mut uniform_ir = 0u32;
    if has_u {
        uniform_ir = ctx.alloc_buffer_ir();
        let ubuf = prog.ubuf[..prog.ubuf_size.max(0) as usize].to_vec();
        cmds.push(Cmd::CreateBuffer(uniform_ir, BufferDesc { size: ubuf.len() as u64, usage: buffer_usage::UNIFORM, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: uniform_ir, offset: 0, data: ubuf });
    }
    let mut bind_group_ir = 0u32;
    if has_bg {
        bind_group_ir = ctx.alloc_bind_group_ir();
        let mut entries = Vec::new();
        if has_u {
            entries.push(BindEntry { binding: 1, resource: BindResource::Buffer { id: uniform_ir, offset: 0, size: prog.ubuf_size as u64 } });
        }
        for (k, tb) in texbinds.iter().enumerate() {
            entries.push(BindEntry { binding: k as u32, resource: BindResource::Texture { id: tb.tex_ir } });
            entries.push(BindEntry { binding: k as u32, resource: BindResource::Sampler { id: tb.samp_ir } });
        }
        cmds.push(Cmd::CreateBindGroup(bind_group_ir, BindGroupDesc { set: 0, entries }));
    }

    // ---- submit: texture copies + the render pass ----
    let mut ops: Vec<Enc> = Vec::new();
    for tb in &texbinds {
        ops.push(Enc::CopyBufferToTexture {
            src: tb.stage_ir,
            src_offset: 0,
            bytes_per_row: tb.w * 4,
            dst: tb.tex_ir,
            mip: 0,
            width: tb.w,
            height: tb.h,
        });
    }
    ops.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment { texture: target_tex, load: LoadOp::Clear, clear: d.clear, store: true }],
        depth: None,
    });
    ops.push(Enc::SetPipeline(pipeline_ir));
    ops.push(emit_viewport(ctx, &d));
    ops.push(emit_scissor(ctx, &d));
    if has_bg {
        ops.push(Enc::SetBindGroup { index: 0, group: bind_group_ir });
    }
    for (sl, &ir) in slot_ir.iter().enumerate() {
        ops.push(Enc::SetVertexBuffer { slot: sl as u32, buffer: ir, offset: 0 });
    }
    if d.indexed && index_ir != 0 {
        let ifmt = if d.index_type == GL_UNSIGNED_INT {
            hl_gpu::protocol::model::enums::IndexFormat::U32
        } else {
            hl_gpu::protocol::model::enums::IndexFormat::U16
        };
        ops.push(Enc::SetIndexBuffer { buffer: index_ir, offset: d.index_offset as u64, format: ifmt });
        ops.push(Enc::DrawIndexed { index_count: d.count as u32, instance_count: d.instance_count, first_index: 0, base_vertex: d.base_vertex, first_instance: d.first_instance });
    } else {
        ops.push(Enc::Draw { vertex_count: d.count as u32, instance_count: d.instance_count, first_vertex: d.first as u32, first_instance: d.first_instance });
    }
    ops.push(Enc::EndRenderPass);

    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    Some(Frame { cmds, present: (surface, target_tex) })
}

/// `SetViewport` with the GL→Metal Y-flip (`gl_shim.c` `emit_viewport_h`).
fn emit_viewport(ctx: &GlContext, d: &DrawCall) -> Enc {
    let (_, th) = ctx.target_wh();
    let (mut x, mut y, mut w, mut h) = (0.0f32, 0.0f32, ctx.surf.width as f32, th as f32);
    if d.viewport[2] > 0 && d.viewport[3] > 0 {
        x = d.viewport[0] as f32;
        w = d.viewport[2] as f32;
        h = d.viewport[3] as f32;
        y = (th - d.viewport[1] - d.viewport[3]) as f32;
    }
    Enc::SetViewport { x, y, w, h, min_depth: 0.0, max_depth: 1.0 }
}

/// `SetScissor` with the Y-flip + clamp (`gl_shim.c` `emit_scissor_h`).
fn emit_scissor(ctx: &GlContext, d: &DrawCall) -> Enc {
    let (tw, th) = ctx.target_wh();
    let (mut x, mut y, mut w, mut h) = (0, 0, tw, th);
    if d.scissor_enabled && d.scissor[2] > 0 && d.scissor[3] > 0 {
        x = d.scissor[0];
        y = th - d.scissor[1] - d.scissor[3];
        w = d.scissor[2];
        h = d.scissor[3];
    }
    x = x.clamp(0, tw);
    y = y.clamp(0, th);
    if x + w > tw {
        w = tw - x;
    }
    if y + h > th {
        h = th - y;
    }
    Enc::SetScissor { x: x as u32, y: y as u32, w: w.max(0) as u32, h: h.max(0) as u32 }
}

/// Vertex-attribute format packing (`gl_shim.c` `vertex_format_wire`):
/// `comps | (kind<<8) | (normalized<<16) | (integer<<17)`, comps clamped to [1,4].
fn vertex_format_wire(kind_enum: u32, comps: i32, normalized: bool, integer: bool) -> u32 {
    let comps = comps.clamp(1, 4) as u32;
    let kind = match kind_enum {
        GL_UNSIGNED_BYTE => 1,
        GL_BYTE => 2,
        GL_UNSIGNED_SHORT => 3,
        GL_SHORT => 4,
        GL_UNSIGNED_INT => 5,
        GL_INT => 6,
        GL_HALF_FLOAT => 7,
        _ => 0, // GL_FLOAT and unknown
    };
    comps | (kind << 8) | ((normalized as u32) << 16) | ((integer as u32) << 17)
}

/// Vertex-attribute format from a GLSL declaration type string (`gl_shim.c` `decl_format_wire`).
fn decl_format_wire(t: &str) -> u32 {
    let comps: u32 = if t.contains("vec2") {
        2
    } else if t.contains("vec3") {
        3
    } else if t.starts_with("float") {
        1
    } else {
        4
    };
    let integer = t.starts_with("ivec") || t.starts_with("uvec");
    let kind: u32 = if t.starts_with("ivec") {
        6
    } else if t.starts_with("uvec") {
        5
    } else {
        0
    };
    comps | (kind << 8) | ((integer as u32) << 17)
}
