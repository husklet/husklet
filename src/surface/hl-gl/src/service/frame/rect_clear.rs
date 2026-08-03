use super::*;
use crate::model::context::ClearPipelineKey;
use hl_gpu::protocol::model::enums::compare;
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};

/// The internal clear shaders, in the HOST dialect (desktop GLSL 460, what naga's `glsl-in` receives) —
/// not the app-facing GLSL-ES the translator produces, because these never pass through it.
///
/// The vertex stage generates a full-target triangle from `gl_VertexID` alone, so the draw binds no vertex
/// buffer and needs no layout. The fragment stage emits `vec4(1.0)`; the value actually written is that
/// multiplied by the blend constant (see [`clear_blend`]), which is how the clear COLOUR reaches the
/// target without a uniform buffer — and a uniform buffer would mean a bind group keyed on a GL program
/// this draw does not have.
///
/// Both entry points are `main` IN SOURCE and are renamed host-side to the pipeline-bound `vmain`/`fmain`
/// (`GlslDescriptor::entry`) — the same contract the translated application shaders keep. naga's `glsl-in`
/// takes a STAGE, not an entry name, and parses only `main`: a source function named `vmain` is "Missing
/// entry point", which fails the submit and loses the share group.
const CLEAR_VS: &str = "#version 460\n\
void main() {\n\
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));\n\
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n\
}\n";

fn clear_fs(color_target_count: usize) -> String {
    let mut source = String::from("#version 460\n");
    for slot in 0..color_target_count {
        source.push_str(&format!(
            "layout(location = {slot}) out vec4 hl_clear_colour{slot};\n"
        ));
    }
    source.push_str("void main() {\n");
    for slot in 0..color_target_count {
        source.push_str(&format!(" hl_clear_colour{slot} = vec4(1.0);\n"));
    }
    source.push_str("}\n");
    source
}

/// `src = CONSTANT, dst = ZERO, op = ADD`, so the written value is exactly the blend constant.
fn clear_blend() -> BlendState {
    use hl_gpu::protocol::model::enums::blend_factor;
    BlendState {
        src_color: blend_factor::CONSTANT,
        dst_color: blend_factor::ZERO,
        op_color: 0,
        src_alpha: blend_factor::CONSTANT,
        dst_alpha: blend_factor::ZERO,
        op_alpha: 0,
    }
}

/// Lower one `glClear` that an attachment load op cannot express into a draw that paints exactly the
/// planes, channels, bits and rect it names (see [`DrawCall::needs_rect_clear`]).
///
/// Three encoder operations carry the three clear values, which is what keeps this to one static shader
/// pair rather than a shader per clear value:
///
/// * DEPTH — the viewport's depth range is collapsed (`min_depth == max_depth == clear_depth`), so every
///   fragment's window depth is exactly the clear value whatever the vertices say.
/// * STENCIL — `Enc::SetStencilReference` with a `REPLACE` pass op.
/// * COLOUR — `Enc::SetBlendConstant` with the blend above.
///
/// The write masks are the pipeline's, so a partial `glColorMask` or `glStencilMask` writes exactly its
/// channels or bits, and a plane this clear does not name is masked off entirely rather than being
/// touched. Returns the ops to append inside the current render pass.
pub(super) fn lower_rect_clear(
    ctx: &mut GlContext,
    d: &DrawCall,
    target_fmt: TextureFormat,
    depth_fmt: Option<TextureFormat>,
    tw: i32,
    th: i32,
    bottom_up: bool,
    cmds: &mut Vec<Cmd>,
) -> Option<Vec<Enc>> {
    let writes_colour = d.clears_color() || d.color_clear_is_partial();
    let writes_depth = d.clears_depth();
    let writes_stencil = d.clears_stencil();
    if !writes_colour && !writes_depth && !writes_stencil {
        return None;
    }
    // A plane this clear does not name must not be disturbed, so its mask is zero rather than absent.
    let color_write_mask = if writes_colour { d.color_mask & 0xf } else { 0 };
    // The constant-factor blend is what carries the clear COLOUR, and a pipeline that names a constant
    // factor obliges every draw through it to have a blend constant set. A depth- or stencil-only clear
    // sets none — it has no colour to carry — so it must not name one either: the colour target takes a
    // plain replace with a zero write mask instead. Keying this on the write mask rather than on
    // `writes_colour` keeps it consistent with `ClearPipelineKey`, which carries the mask.
    let blends_colour = color_write_mask != 0;
    let stencil_write_mask = if writes_stencil {
        d.stencil_write_mask_front & 0xff
    } else {
        0
    };
    // A depth/stencil-writing clear needs the pass to actually carry that attachment. When it does not,
    // there is nothing to write and the clear is dropped rather than lowered against a missing plane.
    if (writes_depth || writes_stencil) && depth_fmt.is_none() {
        return None;
    }

    let (vs_ir, fs_ir, needs_shaders) = ctx.clear_shader_ir(1).ok()?;
    if needs_shaders {
        for (id, stage, entry, source) in [
            (vs_ir, glsl_stage::VERTEX, "vmain", CLEAR_VS.to_string()),
            (fs_ir, glsl_stage::FRAGMENT, "fmain", clear_fs(1)),
        ] {
            cmds.push(Cmd::CreateShader {
                id,
                kind: ShaderPayloadKind::Glsl,
                spirv: GlslDescriptor {
                    stage,
                    entry: entry.into(),
                    source,
                }
                .to_words(),
            });
        }
    }

    let depth = depth_fmt.map(|format| DepthState {
        format,
        depth_write: writes_depth,
        // The clear must land on every fragment it covers, so the depth TEST is defeated rather than
        // consulted — a clear is not depth-tested.
        depth_compare: compare::ALWAYS,
        stencil_front: stencil_face(writes_stencil),
        stencil_back: stencil_face(writes_stencil),
        stencil_read_mask: 0xff,
        stencil_write_mask,
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
    });
    let key = ClearPipelineKey {
        color_formats: [target_fmt.to_u32(), 0, 0, 0],
        color_target_count: 1,
        depth_format: depth_fmt.map(|f| f.to_u32()).unwrap_or(0),
        color_write_masks: [color_write_mask, 0, 0, 0],
        depth_write: writes_depth,
        stencil_write_mask,
    };
    let (pipeline_ir, needs_pipeline) = ctx.clear_pipeline_ir(key).ok()?;
    if needs_pipeline {
        cmds.push(Cmd::CreateRenderPipeline(
            pipeline_ir,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: vs_ir,
                    entry: "vmain".into(),
                },
                fragment: Some(ShaderRef {
                    module: fs_ir,
                    entry: "fmain".into(),
                }),
                vertex_buffers: Vec::new(),
                color_targets: vec![ColorTargetState {
                    format: target_fmt,
                    blend: blends_colour.then(clear_blend),
                    write_mask: color_write_mask,
                }],
                depth: depth.clone(),
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: "gl-rect-clear".into(),
            },
        ));
    }

    let mut ops = Vec::with_capacity(6);
    // The collapsed depth range IS the depth clear value. A clear that does not name depth still needs a
    // viewport, and collapsing it to the current value would write depth on a masked-off plane — so an
    // unnamed depth plane keeps the ordinary range and is protected by `depth_write: false` above.
    let depth_value = d.clear_depth.clamp(0.0, 1.0);
    let (min_depth, max_depth) = if writes_depth {
        (depth_value, depth_value)
    } else {
        (0.0, 1.0)
    };
    ops.push(Enc::SetViewport {
        x: 0.0,
        y: 0.0,
        w: tw as f32,
        h: th as f32,
        min_depth,
        max_depth,
    });
    ops.push(clear_scissor_enc(d, tw, th, bottom_up));
    if writes_stencil {
        ops.push(Enc::SetStencilReference {
            reference: (d.clear_stencil.max(0) as u32) & 0xff,
        });
    }
    if blends_colour {
        ops.push(Enc::SetBlendConstant {
            color: d.clear.map(|value| value as f32),
        });
    }
    ops.push(Enc::SetPipeline(pipeline_ir));
    ops.push(Enc::Draw {
        vertex_count: 3,
        instance_count: 1,
        first_vertex: 0,
        first_instance: 0,
    });
    Some(ops)
}

/// Lower the color portion of an MRT clear as one fullscreen/scissored draw. Each target receives its own
/// write mask; zero masks preserve attachments that the clear did not select.
pub(super) fn lower_mrt_color_clear(
    ctx: &mut GlContext,
    d: &DrawCall,
    target_formats: &[TextureFormat],
    tw: i32,
    th: i32,
    bottom_up: bool,
    cmds: &mut Vec<Cmd>,
) -> Option<Vec<Enc>> {
    let mut masks = [0u32; 4];
    for slot in 0..target_formats.len().min(4) {
        let selected = match d.clear_draw_buffer {
            Some(index) => index as usize == slot,
            None => d.draw_buffer_mask & (1u32 << slot) != 0,
        };
        if selected {
            masks[slot] = d.color_mask_for_slot(slot as u32);
        }
    }
    if masks.iter().all(|mask| *mask == 0) {
        return None;
    }

    let target_count = target_formats.len().min(4);
    let (vs_ir, fs_ir, needs_shaders) = ctx.clear_shader_ir(target_count as u32).ok()?;
    if needs_shaders {
        for (id, stage, entry, source) in [
            (vs_ir, glsl_stage::VERTEX, "vmain", CLEAR_VS.to_string()),
            (fs_ir, glsl_stage::FRAGMENT, "fmain", clear_fs(target_count)),
        ] {
            cmds.push(Cmd::CreateShader {
                id,
                kind: ShaderPayloadKind::Glsl,
                spirv: GlslDescriptor {
                    stage,
                    entry: entry.into(),
                    source,
                }
                .to_words(),
            });
        }
    }
    let mut formats = [0u32; 4];
    for (slot, format) in target_formats.iter().take(4).enumerate() {
        formats[slot] = format.to_u32();
    }
    let key = ClearPipelineKey {
        color_formats: formats,
        color_target_count: target_formats.len().min(4) as u32,
        depth_format: 0,
        color_write_masks: masks,
        depth_write: false,
        stencil_write_mask: 0,
    };
    let (pipeline_ir, needs_pipeline) = ctx.clear_pipeline_ir(key).ok()?;
    if needs_pipeline {
        cmds.push(Cmd::CreateRenderPipeline(
            pipeline_ir,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: vs_ir,
                    entry: "vmain".into(),
                },
                fragment: Some(ShaderRef {
                    module: fs_ir,
                    entry: "fmain".into(),
                }),
                vertex_buffers: Vec::new(),
                color_targets: target_formats
                    .iter()
                    .enumerate()
                    .map(|(slot, &format)| ColorTargetState {
                        format,
                        blend: (masks[slot] != 0).then(clear_blend),
                        write_mask: masks[slot],
                    })
                    .collect(),
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: "gl-mrt-rect-clear".into(),
            },
        ));
    }
    Some(vec![
        Enc::SetViewport {
            x: 0.0,
            y: 0.0,
            w: tw as f32,
            h: th as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        },
        clear_scissor_enc(d, tw, th, bottom_up),
        Enc::SetBlendConstant {
            color: d.clear.map(|value| value as f32),
        },
        Enc::SetPipeline(pipeline_ir),
        Enc::Draw {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        },
    ])
}

/// `ALWAYS`/`REPLACE` when this clear writes stencil, and a face that changes nothing when it does not.
fn stencil_face(writes: bool) -> StencilFaceState {
    use hl_gpu::protocol::model::enums::stencil_op as so;
    StencilFaceState {
        compare: compare::ALWAYS,
        fail_op: so::KEEP,
        depth_fail_op: so::KEEP,
        pass_op: if writes { so::REPLACE } else { so::KEEP },
    }
}

/// The rect a clear paints, as an `Enc::SetScissor`: its scissor box when the scissor test is on, and the
/// whole target when it is not. Rows convert from GL's bottom-left origin exactly as [`emit_scissor`] does
/// for an ordinary draw.
fn clear_scissor_enc(d: &DrawCall, tw: i32, th: i32, bottom_up: bool) -> Enc {
    if !d.scissor_enabled {
        return Enc::SetScissor {
            x: 0,
            y: 0,
            w: tw.max(0) as u32,
            h: th.max(0) as u32,
        };
    }
    let [sx, sy, sw, sh] = d.scissor;
    let top = if bottom_up {
        sy
    } else {
        th.saturating_sub(sy.saturating_add(sh))
    };
    let x0 = sx.clamp(0, tw);
    let x1 = sx.saturating_add(sw).clamp(0, tw);
    let y0 = top.clamp(0, th);
    let y1 = top.saturating_add(sh).clamp(0, th);
    Enc::SetScissor {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}
