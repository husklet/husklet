use super::*;

// Kept parallel with lower_draw_n so the common one-target path stays obvious at each frame call site.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_draw(
    ctx: &mut GlContext,
    d: &DrawCall,
    target_fmt: TextureFormat,
    depth_fmt: Option<TextureFormat>,
    tw: i32,
    th: i32,
    cmds: &mut Vec<Cmd>,
    fbo_tex_ir: &std::collections::HashMap<(u32, u64), u32>,
    snapshots: &mut SnapshotTextures,
) -> Option<DrawCommands> {
    lower_draw_n(
        ctx, d, target_fmt, depth_fmt, 1, tw, th, cmds, fbo_tex_ir, snapshots,
    )
}

/// [`lower_draw`] with an explicit color-target count `n_color_targets` (≥ 1). One target is the ordinary
/// path (every caller but the MRT frame passes 1, byte-identical to before); `n > 1` builds a pipeline with
/// `n` identical color targets so a `glDrawBuffers` MRT draw writes all attachments of a multi-target pass.
/// `depth_fmt` is the pass's depth-attachment format ([`pass_depth_format`]) that every depth/stencil
/// pipeline in the pass must share; `None` for a pass with no depth attachment.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_draw_n(
    ctx: &mut GlContext,
    d: &DrawCall,
    target_fmt: TextureFormat,
    depth_fmt: Option<TextureFormat>,
    n_color_targets: usize,
    tw: i32,
    th: i32,
    cmds: &mut Vec<Cmd>,
    fbo_tex_ir: &std::collections::HashMap<(u32, u64), u32>,
    snapshots: &mut SnapshotTextures,
) -> Option<DrawCommands> {
    let d = d.clone();
    // `glCullFace(GL_FRONT_AND_BACK)` discards every triangle in GL. There is no WebGPU cull mode for it,
    // so the draw produces no primitives and is dropped here rather than lowered as a back-face cull.
    if d.discards_every_primitive() && d.transform_feedback.is_none() {
        return None;
    }
    if d.transform_feedback
        .as_ref()
        .is_some_and(|capture| capture.vertices == 0)
    {
        return Some(DrawCommands {
            copies: Vec::new(),
            ops: Vec::new(),
        });
    }
    let prog_name = if d.prog != 0 {
        d.prog
    } else {
        ctx.local.cur_prog
    };
    // The three lookups below can each fail, and each failure DROPS the draw. They log rather than
    // returning silently, for the reason the indexed-draw site below already does: from the application's
    // side a draw that quietly did nothing is indistinguishable from one that was legitimately culled, so
    // a discard with no signal makes the frame's missing geometry unattributable.
    //
    // The missing-IR cases are the ones that matter most. A program with no vertex or fragment
    // representation is one whose TRANSLATION failed earlier; by the time it reaches here the draw's
    // disappearance carries no link back to that failure, so a shader-translation defect presents as
    // missing geometry and the investigation starts at the renderer instead of at the translator.
    //
    // Deliberately NOT an error: ES 3.0 §2.11.3 leaves drawing with no usable program undefined but does
    // not require `GL_INVALID_OPERATION`, and `glUseProgram(0)` followed by a draw is legal. Inventing an
    // error here would make a conformant application fail. A log is the honest signal.
    let Some(prog) = ctx.programs.program(prog_name).cloned() else {
        hl_log::hl_warn!(
            hl_log::tag::GL,
            "dropping draw with no such program program={prog_name} current={} fbo={} mode={:#x} count={}",
            ctx.local.cur_prog,
            d.fbo,
            d.mode,
            d.count
        );
        return None;
    };
    let Some(vs_ir) = prog.vs_ir.clone() else {
        // ERROR, not warn: warn/info/debug are compiled out of a release build entirely, so a warning
        // here would make this loss attributable in a debug build only — and this is the first link of a
        // chain that ends in a window region the user sees the desktop through. At error it survives the
        // build and an operator can reach it with `HL_LOG=gl`. Latched once per context because a broken
        // program is redrawn every frame; the first occurrence carries the identifying detail.
        if !ctx.local.missing_shader_ir_reported {
            ctx.local.missing_shader_ir_reported = true;
            hl_log::hl_error!(
                hl_log::tag::GL,
                "dropping draw whose program has no vertex IR — its translation failed earlier. Its \
                 pixels will be MISSING from this frame, and on a freshly minted target that region is \
                 transparent rather than stale. program={prog_name} linked={} link_gen={} fbo={} \
                 mode={:#x} count={}. Reported once per context.",
                prog.linked,
                prog.link_gen,
                d.fbo,
                d.mode,
                d.count
            );
        }
        return None;
    };
    let Some(fs_ir) = prog.fs_ir.clone() else {
        // ERROR, not warn: warn/info/debug are compiled out of a release build entirely, so a warning
        // here would make this loss attributable in a debug build only — and this is the first link of a
        // chain that ends in a window region the user sees the desktop through. At error it survives the
        // build and an operator can reach it with `HL_LOG=gl`. Latched once per context because a broken
        // program is redrawn every frame; the first occurrence carries the identifying detail.
        if !ctx.local.missing_shader_ir_reported {
            ctx.local.missing_shader_ir_reported = true;
            hl_log::hl_error!(
                hl_log::tag::GL,
                "dropping draw whose program has no fragment IR — its translation failed earlier. Its \
                 pixels will be MISSING from this frame, and on a freshly minted target that region is \
                 transparent rather than stale. program={prog_name} linked={} link_gen={} fbo={} \
                 mode={:#x} count={}. Reported once per context.",
                prog.linked,
                prog.link_gen,
                d.fbo,
                d.mode,
                d.count
            );
        }
        return None;
    };
    if d.indexed {
        let has_index_source = if d.elem_buf == 0 {
            !d.client_indices.is_empty()
        } else {
            d.buffers
                .iter()
                .find(|buffer| buffer.name == d.elem_buf)
                .is_some_and(|buffer| !buffer.data.is_empty())
                || ctx.buffers.has_data(d.elem_buf)
        };
        if !has_index_source {
            hl_log::hl_warn!(
                hl_log::tag::GL,
                "dropping indexed draw without captured index data buffer={} offset={} count={} type={:#x}",
                d.elem_buf,
                d.index_offset,
                d.count,
                d.index_type
            );
            return None;
        }
    }
    let vdecl = crate::adapter::glsl::Source::new(&prog.vs_src).vertex_attrs();
    let ndecl = vdecl.len();
    let VertexLowering {
        nslot,
        slot_stride,
        slot_base,
        slot_ir,
        slot_bytes,
        attr_slot,
        nvd,
        client_slots,
        expanded_indices,
        index_ir,
    } = lower_vertices(ctx, &prog, &d, cmds).ok()?;
    let texbinds = lower_textures(ctx, &d, &prog, cmds, fbo_tex_ir, snapshots).ok()?;
    let has_u = prog.has_uniforms();
    let has_bg = has_u || !texbinds.is_empty();
    let blend_source_scale = mixed_constant_blend_source_scale(&d);

    // ---- shaders + pipeline ----
    // The vertex and fragment GLSL are forwarded as two separate `Glsl` shader modules (each carries one
    // stage's source led by GLSL_MAGIC); the render pipeline binds them by their `vmain`/`fmain` entries.
    // naga's `glsl-in` compiles one stage per module, so the two stages are distinct modules (not one
    // combined module as the old pre-translated-MSL path used).
    // Program-keyed shader residency: a linked GskGpu program's two shader modules are `CreateShader`d ONCE
    // (per link generation) and re-referenced by their stable IR ids on every later draw/frame — so a reused
    // program costs ZERO host naga compiles after the first sight. Before this cache the builder minted fresh
    // ids + re-emitted `CreateShader` for the SAME program on every draw (a GTK frame reuses ~11 programs
    // across ~260 draws → ~520 redundant compiles). See `GlContext::program_shader_ir`.
    // Row order of this draw's target — the one place the convention branches; see
    // `RenderPasses::stores_bottom_up_rows` for the contract. An imported external image (Chrome's scanout
    // EGLImage, attached to a non-zero FBO and published at `glFlush`) must carry true GL FBO texel order,
    // so it takes the clip reflection, its reversed winding, and un-converted window rows. Every internal
    // target — including the default framebuffer — stores rows top-down and takes none of that.
    let bottom_up = RenderPasses::stores_bottom_up_rows(ctx, d.target);
    let sample_transforms: Vec<(String, bool, [u32; 4])> = texbinds
        .iter()
        .filter(|binding| {
            binding.flip_y
                || binding.swizzle
                    != [
                        crate::model::glconst::GL_RED,
                        crate::model::glconst::GL_GREEN,
                        crate::model::glconst::GL_BLUE,
                        crate::model::glconst::GL_ALPHA,
                    ]
        })
        .map(|binding| (binding.sampler.clone(), binding.flip_y, binding.swizzle))
        .collect();
    let uses_frag_coord = prog.fs_src.contains("gl_FragCoord");
    let origin_upper_left = prog.fs_src.contains("origin_upper_left");
    let pixel_center_integer = prog.fs_src.contains("pixel_center_integer");
    let correct_frag_coord =
        uses_frag_coord && (!origin_upper_left || pixel_center_integer) && th > 0;
    // `correct_frag_coord` restores GL's bottom-left `gl_FragCoord.y` from WebGPU's top-left fragment
    // position, which is what a top-down target needs. A `bottom_up` target already exposes GL rows
    // directly, so the conversion is redundant there; it is left applied because Chrome is the only such
    // target today and its behavior is deliberately unchanged by this fix.
    // The clip reflection below is independent from sampling a rendered FBO: that texture still needs its
    // own texel-order conversion in the sampler regardless of the destination.
    let mut sample_variant = sample_transforms.iter().fold(
        0xcbf2_9ce4_8422_2325u64,
        |mut hash, (name, flip, swizzle)| {
            for byte in name.bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            hash ^= u64::from(*flip);
            hash = hash.wrapping_mul(0x100_0000_01b3);
            for component in swizzle {
                hash ^= u64::from(*component);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            hash
        },
    );
    if correct_frag_coord {
        sample_variant ^= th as u64;
        sample_variant = sample_variant.wrapping_mul(0x100_0000_01b3);
        sample_variant ^= u64::from(origin_upper_left);
        sample_variant = sample_variant.wrapping_mul(0x100_0000_01b3);
        sample_variant ^= u64::from(pixel_center_integer);
    }
    sample_variant ^= u64::from(bottom_up) << 63;
    if let Some(scale) = blend_source_scale {
        for component in scale {
            sample_variant ^= u64::from(component.to_bits());
            sample_variant = sample_variant.wrapping_mul(0x100_0000_01b3);
        }
    }
    let (vs_id, fs_id, shaders_new) = ctx
        .program_shader_ir(prog_name, sample_variant, prog.link_gen)
        .ok()?;
    if shaders_new {
        let vs_ir = {
            use hl_gpu::protocol::model::kernel::GlslDescriptor;
            let mut descriptor = GlslDescriptor::from_words(&vs_ir)
                .and_then(|result| result.ok())
                .expect("linked GL vertex shader is a GLSL descriptor");
            // The clip-volume remap is unconditional: GL clips to -w <= z <= w and the host to
            // 0 <= z <= w, so without it every vertex at negative clip z is discarded — offscreen
            // exactly as much as on a presented target. The Y flip, by contrast, is an orientation
            // fix that only a directly-presented target needs.
            descriptor.source = crate::adapter::glsl::Source::new(&descriptor.source).clip_depth();
            if bottom_up {
                descriptor.source =
                    crate::adapter::glsl::Source::new(&descriptor.source).present_coordinates();
            }
            descriptor.to_words()
        };
        cmds.push(Cmd::CreateShader {
            id: vs_id,
            kind: ShaderPayloadKind::Glsl,
            spirv: vs_ir,
        });
        let fs_ir = if sample_transforms.is_empty()
            && !correct_frag_coord
            && blend_source_scale.is_none()
        {
            fs_ir
        } else {
            use hl_gpu::protocol::model::kernel::GlslDescriptor;
            let mut descriptor = GlslDescriptor::from_words(&fs_ir)
                .and_then(|result| result.ok())
                .expect("linked GL fragment shader is a GLSL descriptor");
            if !sample_transforms.is_empty() {
                descriptor.source = crate::adapter::glsl::Source::new(&descriptor.source)
                    .transform_texture_samplers(&sample_transforms);
            }
            if correct_frag_coord {
                descriptor.source = crate::adapter::glsl::Source::new(&descriptor.source)
                    .fragment_coordinates(th, origin_upper_left, pixel_center_integer);
            }
            if let Some(scale) = blend_source_scale {
                descriptor.source = crate::adapter::glsl::Source::new(&descriptor.source)
                    .scale_fragment_outputs(scale);
            }
            descriptor.to_words()
        };
        cmds.push(Cmd::CreateShader {
            id: fs_id,
            kind: ShaderPayloadKind::Glsl,
            spirv: fs_ir,
        });
    }

    // Locations fed by an appended client slot (see below) are NOT folded into a VBO slot's layout.
    let mut client_loc = [false; crate::model::program::MAX_ATTR];
    for slot in &client_slots {
        if (slot.location as usize) < crate::model::program::MAX_ATTR {
            client_loc[slot.location as usize] = true;
        }
    }
    // Declare exactly the vertex-buffer slots that the draw binds. In particular, a fullscreen
    // `gl_VertexID`/`gl_InstanceID` draw has no vertex buffers: inventing an empty slot 0 makes wgpu require
    // `SetVertexBuffer(0)` even though the shader consumes no vertex attributes, and rejects the pass with
    // `MissingVertexBuffer`.
    let nvb = nslot;
    let mut vbs: Vec<VertexLayout> = Vec::with_capacity(nvb + client_slots.len());
    for sl in 0..nvb {
        // The base offset hoisted into this slot's bind offset (see `slot_base`); subtracted from each
        // attribute so its in-stride offset stays in `[0, stride)`. Phantom slots (`sl >= nslot`) carry 0.
        let base = if sl < nslot { slot_base[sl] } else { 0 };
        let mut attrs = Vec::new();
        for l in 0..nvd {
            if prog.vertex_attr_components(l).is_none() {
                continue;
            }
            if l < crate::model::program::MAX_ATTR && client_loc[l] {
                continue; // fed by an appended client slot, not this VBO slot
            }
            let ls = if l < crate::model::program::MAX_ATTR && attr_slot[l] >= 0 {
                attr_slot[l]
            } else {
                0
            };
            if ls as usize != sl {
                continue;
            }
            let (fmt, off) =
                if l < crate::model::program::MAX_ATTR && d.attrs[l].enabled && attr_slot[l] >= 0 {
                    let a = &d.attrs[l];
                    (
                        vertex_format_wire(a.kind, a.size, a.normalized, a.integer),
                        (a.offset as u32).saturating_sub(base),
                    )
                } else {
                    let t = if l < ndecl {
                        vdecl[l].ty.as_str()
                    } else {
                        "vec4"
                    };
                    (Pipeline::decl_format(t), 0)
                };
            for location in prog.host_attr_locations(l) {
                attrs.push(VertexAttr {
                    location,
                    format: fmt,
                    offset: off,
                });
            }
        }
        let stride = if sl < nslot { slot_stride[sl] } else { 16 };
        // A vertex-buffer slot steps per-instance (step_mode 1) when any attribute it feeds carries a
        // non-zero `glVertexAttribDivisor`. This model has one step rate per slot, so a divisor `N>1`
        // (fractional instancing rate) collapses to per-instance stepping — an honest partial lowering.
        let step_mode = (0..crate::model::program::MAX_ATTR)
            .any(|l| attr_slot[l] == sl as i32 && d.attrs[l].enabled && d.attrs[l].divisor > 0)
            as u32;
        vbs.push(VertexLayout {
            stride,
            step_mode,
            attrs,
        });
    }
    // Append one layout per client-side array — a single attribute at offset 0, tightly-packed stride.
    for cs in &client_slots {
        vbs.push(VertexLayout {
            stride: cs.stride,
            step_mode: cs.step_mode,
            attrs: prog
                .host_attr_locations(cs.location as usize)
                .into_iter()
                .map(|location| VertexAttr {
                    location,
                    format: cs.format,
                    offset: 0,
                })
                .collect(),
        });
    }
    // Fixed-function state → the pipeline's blend / depth / cull descriptor (the values a real app set via
    // glBlendFunc / glDepthFunc / glCullFace / glFrontFace, mapped to their opaque WebGPU wire enums).
    let blend = if d.blend {
        let src_rgb = if blend_source_scale.is_some() {
            GL_ONE
        } else {
            d.blend_src_rgb
        };
        Some(BlendState {
            src_color: Pipeline::blend_factor(src_rgb),
            dst_color: Pipeline::blend_factor(d.blend_dst_rgb),
            op_color: Pipeline::blend_op(d.blend_eq_rgb),
            src_alpha: Pipeline::blend_factor(d.blend_src_alpha),
            dst_alpha: Pipeline::blend_factor(d.blend_dst_alpha),
            op_alpha: Pipeline::blend_op(d.blend_eq_alpha),
        })
    } else {
        None
    };
    // Depth/stencil test → a pipeline depth state carrying the depth compare + write mask AND the front/back
    // stencil test+ops + read/write masks. When the PASS has a depth(+stencil) attachment (`depth_fmt` is
    // `Some`, because SOME draw in it is depth/stencil-tested), EVERY pipeline in the pass MUST carry a depth
    // state of that exact format — wgpu rejects a `depth: None` pipeline used in a pass that has a
    // depth-stencil attachment. A draw that itself neither depth- nor stencil-tests gets a NEUTRAL state
    // (never writes depth, `ALWAYS` compare, stencil `DISABLED`), so it renders unaffected while staying
    // format-compatible with the attachment. A pass with NO depth attachment (the common 2D path) keeps
    // `None` — unchanged.
    let depth = depth_fmt.map(|fmt| Pipeline::depth_state(fmt, &d));
    let topology = PrimitiveAssembly::topology(d.mode);
    // MSAA sample count the pipeline must declare so a multisampled attachment actually resolves (the GL
    // analogue of the Vulkan `sample_count` drop). Sourced from the bound draw framebuffer's attachments;
    // see `framebuffer_sample_count` for this model's (single-sampled-only) status.
    let sample_count = ctx.local.framebuffers.sample_count(d.fbo);
    // One color target for the ordinary path; `n_color_targets` identical targets for a `glDrawBuffers` MRT
    // pass (each attachment shares the draw's format + blend). The fragment shader's `layout(location = k)`
    // outputs map onto target `k` (see `adapter::glsl::translate_render`).
    // A slot deselected by `glDrawBuffers(…, GL_NONE, …)` receives no fragment output at all, which is a
    // zero write mask on that target — the attachment keeps whatever it already held.
    let color_targets: Vec<ColorTargetState> = (0..n_color_targets.max(1))
        .map(|slot| ColorTargetState {
            format: target_fmt,
            blend: blend.clone(),
            write_mask: if d.draw_buffer_mask & (1u32 << slot.min(31)) != 0 {
                d.color_mask & 0xf
            } else {
                0
            },
        })
        .collect();
    let cull = if d.cull_enabled {
        Pipeline::cull(d.cull_face)
    } else {
        0
    };
    // Reflecting clip Y reverses triangle winding. Swap the declared front face for the reflected
    // (bottom-up) targets so GL culling remains unchanged.
    let mut front_face = Pipeline::front_face(d.front_face);
    if bottom_up {
        front_face ^= 1;
    }
    let distinct_face_state = d.stencil
        && (d.stencil_read_mask_front != d.stencil_read_mask_back
            || d.stencil_write_mask_front != d.stencil_write_mask_back
            || d.stencil_ref_front != d.stencil_ref_back);
    let triangle_faces = matches!(d.mode, GL_TRIANGLES | GL_TRIANGLE_STRIP | 0x0006);
    let variants = if distinct_face_state && triangle_faces && !d.cull_enabled {
        vec![
            (
                depth_fmt.map(|fmt| Pipeline::depth_state_for_face(fmt, &d, false)),
                2,
                d.stencil_ref_front,
            ),
            (
                depth_fmt.map(|fmt| Pipeline::depth_state_for_face(fmt, &d, true)),
                1,
                d.stencil_ref_back,
            ),
        ]
    } else if distinct_face_state && d.cull_enabled && d.cull_face == GL_FRONT {
        vec![(
            depth_fmt.map(|fmt| Pipeline::depth_state_for_face(fmt, &d, true)),
            cull,
            d.stencil_ref_back,
        )]
    } else {
        vec![(depth.clone(), cull, d.stencil_ref_front)]
    };
    let mut pipeline_irs = Vec::with_capacity(variants.len());
    for (variant_depth, variant_cull, stencil_ref) in variants {
        let state_key = pipeline_state_key(
            &vbs,
            &color_targets,
            &variant_depth,
            topology,
            variant_cull,
            front_face,
            sample_count,
        ) ^ sample_variant.rotate_left(17);
        let (pipeline_ir, pipe_new) = ctx
            .program_pipeline_ir(prog_name, state_key, prog.link_gen)
            .ok()?;
        if pipe_new {
            cmds.push(Cmd::CreateRenderPipeline(
                pipeline_ir,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: vs_id,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: fs_id,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vbs.clone(),
                    color_targets: color_targets.clone(),
                    depth: variant_depth,
                    topology,
                    cull: variant_cull,
                    front_face,
                    sample_count,
                    label: String::new(),
                },
            ));
        }
        pipeline_irs.push((pipeline_ir, stencil_ref));
    }
    let pipeline_ir = pipeline_irs[0].0;
    let mut vertex_bindings = slot_ir
        .iter()
        .zip(&slot_bytes)
        .zip(&slot_base)
        .map(|((&buffer, &bytes), &offset)| range::VertexBinding {
            buffer,
            bytes,
            offset: u64::from(offset),
        })
        .collect::<Vec<_>>();
    vertex_bindings.extend(client_slots.iter().map(|slot| range::VertexBinding {
        buffer: slot.ir,
        bytes: slot.bytes,
        offset: 0,
    }));
    range::VertexDraw {
        pipeline: pipeline_ir,
        layouts: &vbs,
        bindings: &vertex_bindings,
        indexed: d.indexed,
        first_vertex: d.first.max(0) as u32,
        vertex_count: d.count.max(0) as u32,
        first_instance: d.first_instance,
        instance_count: d.instance_count,
    }
    .trace();

    // ---- uniform buffer + bind group ----
    // The std140 bytes for the shader's binding-0 UBO. Two sources, mutually exclusive per draw:
    //   * a uniform BLOCK fed by the app's `glBindBufferBase`d buffer (GskGpu/GTK4) — `d.ubo_bytes`, already
    //     std140 as the app wrote it, so bound VERBATIM (this carries the real per-draw mvp/clip/scale); OR
    //   * the default-block `glUniform*` uniforms (ES2 `gl_multitex`/`gl_geometry`) — `d.ubuf_bytes`, the
    //     PER-DRAW snapshot of `Program::ubuf` (so two draws of one program with different `glUniform*`
    //     values between them each keep their own bytes, not the last-set ones).
    // A UBO-block draw prefers its snapshotted block bytes, then the per-draw `glUniform*` snapshot, and
    // only falls back to the live `Program::ubuf` for a draw recorded before this snapshotting existed.
    let mut ubuf: Vec<u8> = if !d.ubo_bytes.is_empty() {
        d.ubo_bytes.clone()
    } else if !d.ubuf_bytes.is_empty() {
        d.ubuf_bytes.clone()
    } else {
        prog.ubuf[..prog.ubuf_size.max(0) as usize].to_vec()
    };
    // GskGpu (GTK 4.14+ GL renderer) drives ALL geometry through a std140 push-constant UBO at binding 0:
    //   layout(std140, binding = 0) uniform PushConstants { mat4 mvp; mat3x4 clip; vec2 scale; } push;
    // every vertex is `gl_Position = push.mvp * vec4(in_rect_scaled, 0, 1)` and clipped by `push.clip`. GTK
    // 4.22's GskGL allocates this globals buffer (`glBufferData(GL_UNIFORM_BUFFER, 16384, NULL)`) and binds it
    // via `glBindBufferBase(GL_UNIFORM_BUFFER, 0, buf)`, but its per-pass CONTENTS never arrive over any GL
    // upload this deferred driver observes: there is NO `glBufferSubData` / `glMapBufferRange` / `glUniform*`
    // to the bound globals buffer (only the instance/vertex VBO is filled via `glBufferSubData`) — so the block
    // `resolve_block_ubo_bytes` reads back is all-zero and `push.mvp == 0` collapses every primitive onto the
    // origin (the presented frame keeps only its clear → a uniform blank). Because a GskGpu render pass's mvp
    // IS just the orthographic projection of that pass's render target (top-left origin flipped into GL clip
    // space), its scale is the device pixel scale (1 on our compositor), and its clip is the full target rect,
    // we reconstruct those globals here from the pass's target extent when the bound block is empty-of-data.
    // This is exactly what GskGpu would have written, so `gl_VertexID`-pulled vertices land at the correct
    // screen positions and GTK's real widget geometry becomes visible. Gated on the GskGpu shader signature
    // (`GSK_GLOBAL_MVP`) + an all-zero 128-byte block, so a non-GskGpu app that legitimately binds its own UBO
    // (any non-zero bytes) or uses a different layout is never touched.
    if ubuf.len() == 128
        && tw > 0
        && th > 0
        && ubuf.iter().all(|&b| b == 0)
        && prog.vs_src.contains("GSK_GLOBAL_MVP")
    {
        ubuf = Frame::gsk_globals_std140(tw as f32, th as f32);
    }
    let mut uniform_ir = 0u32;
    if has_u {
        uniform_ir = ctx.alloc_buffer_ir().ok()?;
        cmds.push(Cmd::CreateBuffer(
            uniform_ir,
            BufferDesc {
                size: ubuf.len() as u64,
                usage: buffer_usage::UNIFORM,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: uniform_ir,
            offset: 0,
            data: ubuf.clone(),
        });
    }
    let mut bind_group_ir = 0u32;
    let mut app_bind_entries = Vec::new();
    if has_bg {
        bind_group_ir = ctx.alloc_bind_group_ir().ok()?;
        // Binding scheme (single wgpu bind-group namespace, matching `adapter::glsl`'s emitted
        // `layout(binding=)` — naga derives the pipeline's bind-group layout from that GLSL, so these
        // MUST agree): the uniform block owns binding 0; sampler `k` (declaration index) owns TEXTURE
        // binding `1 + 2k` and SAMPLER binding `2 + 2k`. Every resource lands on a DISTINCT binding, so a
        // program with a UBO AND 2+ samplers no longer aliases the UBO onto a sampler (the old bug: UBO at
        // 1 collided with the 2nd sampler, also at 1).
        let mut entries = Vec::new();
        if has_u {
            entries.push(BindEntry {
                binding: 0,
                resource: BindResource::Buffer {
                    id: uniform_ir,
                    offset: 0,
                    size: ubuf.len() as u64,
                },
            });
        }
        for tb in texbinds.iter() {
            let tex_binding = 1 + 2 * tb.slot as u32;
            let smp_binding = 2 + 2 * tb.slot as u32;
            entries.push(BindEntry {
                binding: tex_binding,
                resource: BindResource::Texture { id: tb.tex_ir },
            });
            entries.push(BindEntry {
                binding: smp_binding,
                resource: BindResource::Sampler { id: tb.samp_ir },
            });
        }
        app_bind_entries = entries.clone();
        cmds.push(Cmd::CreateBindGroup(
            bind_group_ir,
            BindGroupDesc { set: 0, entries },
        ));
    }

    // ---- staging copies (hoisted before BeginRenderPass) + the in-pass draw ops ----
    let mut copies: Vec<Enc> = Vec::new();
    for tb in &texbinds {
        // A cross-pass FBO sample (stage_ir == 0) was rendered by a prior pass — no upload/copy to hoist.
        if tb.stage_ir == 0 {
            continue;
        }
        if tb.layers == 1 {
            copies.push(Enc::CopyBufferToTexture {
                src: tb.stage_ir,
                src_offset: 0,
                bytes_per_row: tb.w * tb.bytes_per_texel,
                dst: tb.tex_ir,
                mip: 0,
                width: tb.w,
                height: tb.h,
            });
        } else {
            // GL cube faces are currently represented by one canonical CPU shadow. Initialize every face
            // from that plane so the cube view is complete and deterministic; leaving five layers
            // uninitialized is both invalid sampling state and observably nondeterministic.
            for (src, layer) in std::iter::once((tb.stage_ir, 0)).chain(tb.layer_stages.iter().copied()) {
                copies.push(Enc::CopyBufferToTextureRegion {
                    src,
                    src_offset: 0,
                    bytes_per_row: tb.w * tb.bytes_per_texel,
                    rows_per_image: tb.h,
                    dst: tb.tex_ir,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: 0, y: 0, z: layer },
                    extent: Extent3d { width: tb.w, height: tb.h, depth: 1 },
                });
            }
        }
        // The mip levels above the base, each into its own level of the same host texture.
        for &(src, mip, width, height, layer) in &tb.mip_stages {
            copies.push(Enc::CopyBufferToTextureRegion {
                src,
                src_offset: 0,
                bytes_per_row: width * tb.bytes_per_texel,
                rows_per_image: height,
                dst: tb.tex_ir,
                dst_sub: TextureSubresource {
                    mip,
                    layer: 0,
                    aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
                },
                dst_origin: Origin3d { x: 0, y: 0, z: layer },
                extent: Extent3d { width, height, depth: 1 },
            });
        }
    }
    let capture_ops = lower_transform_feedback(
        ctx,
        &d,
        &prog,
        CaptureInputs {
            layouts: &vbs,
            slot_ir: &slot_ir,
            slot_base: &slot_base,
            client_slots: &client_slots,
            index_ir,
            expanded_indices: expanded_indices.is_some(),
            app_bind_entries: &app_bind_entries,
            color_targets: &color_targets,
            depth_format: depth_fmt,
            sample_count,
        },
        cmds,
    );
    let mut ops: Vec<Enc> = capture_ops.unwrap_or_default();
    if d.discards_every_primitive() {
        return Some(DrawCommands { copies, ops });
    }
    ops.push(emit_viewport(&d, tw, th, bottom_up));
    ops.push(emit_scissor(&d, tw, th, bottom_up));
    // Dynamic stencil reference — the value the pipeline's stencil compare tests against and a `GL_REPLACE`
    // op writes. Emitted per-draw inside the pass like viewport/scissor (the compare/ops/masks live
    // statically on the pipeline's `DepthState`), so two draws with different `glStencilFunc` references
    // lower correctly. Masked to the 8-bit stencil buffer, and only when this draw stencil-tests.
    if let Some(color) = blend_constant(&d) {
        ops.push(Enc::SetBlendConstant { color });
    }
    if has_bg {
        ops.push(Enc::SetBindGroup {
            index: 0,
            group: bind_group_ir,
        });
    }
    for (sl, &ir) in slot_ir.iter().enumerate() {
        // The per-instance region base (GskGpu's baked `first_instance * stride`) rides the bind offset;
        // it is 0 for an ordinary draw, so this stays byte-identical to the old `offset: 0`.
        ops.push(Enc::SetVertexBuffer {
            slot: sl as u32,
            buffer: ir,
            offset: slot_base[sl] as u64,
        });
    }
    // Client-side transient buffers bind to the slots appended after the VBO slots.
    for (i, cs) in client_slots.iter().enumerate() {
        ops.push(Enc::SetVertexBuffer {
            slot: (nslot + i) as u32,
            buffer: cs.ir,
            offset: 0,
        });
    }
    if expanded_indices.as_ref().is_some_and(Vec::is_empty) {
        // Incomplete loops/fans contain no primitive.
    } else if index_ir != 0 {
        let ifmt = if expanded_indices.is_some() || d.index_type == GL_UNSIGNED_INT {
            hl_gpu::protocol::model::enums::IndexFormat::U32
        } else {
            hl_gpu::protocol::model::enums::IndexFormat::U16
        };
        // A bound element buffer indexes at `index_offset`; a captured client index array is transient
        // (its own buffer from byte 0), so it binds at offset 0.
        let ioff = if expanded_indices.is_none() && d.elem_buf != 0 {
            d.index_offset as u64
        } else {
            0
        };
        ops.push(Enc::SetIndexBuffer {
            buffer: index_ir,
            offset: ioff,
            format: ifmt,
        });
        let index_count = expanded_indices
            .as_ref()
            .map_or(d.count as u32, |indices| indices.len() as u32);
        let base_vertex = if expanded_indices.is_some() && !d.indexed {
            0
        } else {
            d.base_vertex
        };
        for &(pipeline, stencil_ref) in &pipeline_irs {
            ops.push(Enc::SetPipeline(pipeline));
            if d.stencil {
                ops.push(Enc::SetStencilReference {
                    reference: stencil_ref.clamp(0, 0xff) as u32,
                });
            }
            ops.push(Enc::DrawIndexed {
                index_count,
                instance_count: d.instance_count,
                first_index: 0,
                base_vertex,
                first_instance: d.first_instance,
            });
        }
    } else if d.indexed {
        hl_log::hl_warn!(
            hl_log::tag::GL,
            "dropping indexed draw whose index source could not be lowered buffer={} offset={} count={} \
             type={:#x}",
            d.elem_buf,
            d.index_offset,
            d.count,
            d.index_type
        );
        return None;
    } else {
        for &(pipeline, stencil_ref) in &pipeline_irs {
            ops.push(Enc::SetPipeline(pipeline));
            if d.stencil {
                ops.push(Enc::SetStencilReference {
                    reference: stencil_ref.clamp(0, 0xff) as u32,
                });
            }
            ops.push(Enc::Draw {
                vertex_count: d.count as u32,
                instance_count: d.instance_count,
                first_vertex: d.first as u32,
                first_instance: d.first_instance,
            });
        }
    }

    Some(DrawCommands { copies, ops })
}

// Fixed pipeline state encoding continues in `pipeline`.

/// RGB scale folded into the fragment output when GL's RGB source and destination factors require the
/// two distinct constant spellings that WebGPU collapses into one. Keeping the destination factor in
/// fixed-function blending preserves destination reads; replacing the source factor by `ONE` and applying
/// its scalar/vector here is algebraically exact for add, subtract, and reverse-subtract equations.
fn mixed_constant_blend_source_scale(d: &DrawCall) -> Option<[f32; 3]> {
    const CONSTANT_COLOR: u32 = 0x8001;
    const ONE_MINUS_CONSTANT_COLOR: u32 = 0x8002;
    const CONSTANT_ALPHA: u32 = 0x8003;
    const ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;
    if !d.blend {
        return None;
    }
    let colour = |factor| matches!(factor, CONSTANT_COLOR | ONE_MINUS_CONSTANT_COLOR);
    let alpha = |factor| matches!(factor, CONSTANT_ALPHA | ONE_MINUS_CONSTANT_ALPHA);
    if !(colour(d.blend_src_rgb) && alpha(d.blend_dst_rgb)
        || alpha(d.blend_src_rgb) && colour(d.blend_dst_rgb))
    {
        return None;
    }
    let [r, g, b, a] = d.blend_color;
    Some(match d.blend_src_rgb {
        CONSTANT_COLOR => [r, g, b],
        ONE_MINUS_CONSTANT_COLOR => [1.0 - r, 1.0 - g, 1.0 - b],
        CONSTANT_ALPHA => [a; 3],
        ONE_MINUS_CONSTANT_ALPHA => [1.0 - a; 3],
        _ => return None,
    })
}

/// The blend constant a draw's `Enc::SetBlendConstant` must carry, or `None` when no factor references it.
///
/// The IR (like WebGPU) has one `CONSTANT` factor, which multiplies each channel by the SAME-channel
/// component of the blend constant. GL has two: `GL_CONSTANT_COLOR` means exactly that, while
/// `GL_CONSTANT_ALPHA` broadcasts the constant's ALPHA to all four channels. The alpha form is expressed by
/// broadcasting the alpha into the constant this draw sets — the constant is per-draw dynamic state, so
/// this costs nothing and needs no new factor. Without it `GL_CONSTANT_ALPHA` silently behaved as
/// `GL_CONSTANT_COLOR` and a `(0, 0, 0, 200)` constant scaled RGB by zero.
///
/// Alpha blending only consumes the constant's alpha component, where GL's colour and alpha spellings are
/// identical. Therefore only the RGB factors decide whether broadcasting is required. A draw whose RGB
/// source and destination themselves mix both forms remains unexpressible by fixed-function WebGPU; the
/// colour form wins in that case because it is the IR factor's native meaning.
fn blend_constant(d: &DrawCall) -> Option<[f32; 4]> {
    const CONSTANT_COLOR: u32 = 0x8001;
    const ONE_MINUS_CONSTANT_COLOR: u32 = 0x8002;
    const CONSTANT_ALPHA: u32 = 0x8003;
    const ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;
    let effective_src_rgb = if mixed_constant_blend_source_scale(d).is_some() {
        GL_ONE
    } else {
        d.blend_src_rgb
    };
    let factors = [
        effective_src_rgb,
        d.blend_dst_rgb,
        d.blend_src_alpha,
        d.blend_dst_alpha,
    ];
    if !factors
        .iter()
        .any(|factor| matches!(*factor, CONSTANT_COLOR..=ONE_MINUS_CONSTANT_ALPHA))
    {
        return None;
    }
    let rgb_factors = [effective_src_rgb, d.blend_dst_rgb];
    let colour_form = rgb_factors
        .iter()
        .any(|factor| matches!(*factor, CONSTANT_COLOR | ONE_MINUS_CONSTANT_COLOR));
    let alpha_form = rgb_factors
        .iter()
        .any(|factor| matches!(*factor, CONSTANT_ALPHA | ONE_MINUS_CONSTANT_ALPHA));
    if alpha_form && !colour_form {
        return Some([d.blend_color[3]; 4]);
    }
    Some(d.blend_color)
}

#[cfg(test)]
mod blend_constant_tests {
    use super::*;

    #[test]
    fn alpha_equation_constant_colour_does_not_hide_rgb_constant_alpha() {
        let mut draw = DrawCall::default();
        draw.blend = true;
        draw.blend_color = [0.1, 0.2, 0.3, 0.75];
        draw.blend_src_rgb = 0x8003; // GL_CONSTANT_ALPHA
        draw.blend_dst_rgb = GL_ONE;
        draw.blend_src_alpha = 0x8001; // GL_CONSTANT_COLOR; alpha component is still 0.75.

        assert_eq!(blend_constant(&draw), Some([0.75; 4]));
    }

    #[test]
    fn alpha_equation_constant_alpha_does_not_broadcast_rgb_constant_colour() {
        let mut draw = DrawCall::default();
        draw.blend_color = [0.1, 0.2, 0.3, 0.75];
        draw.blend_src_rgb = 0x8001; // GL_CONSTANT_COLOR
        draw.blend_dst_rgb = GL_ONE;
        draw.blend_src_alpha = 0x8003; // GL_CONSTANT_ALPHA

        assert_eq!(blend_constant(&draw), Some(draw.blend_color));
    }

    #[test]
    fn mixed_rgb_constants_fold_only_the_source_factor() {
        let mut draw = DrawCall::default();
        draw.blend = true;
        draw.blend_color = [0.1, 0.2, 0.3, 0.75];
        draw.blend_src_rgb = 0x8004; // GL_ONE_MINUS_CONSTANT_ALPHA
        draw.blend_dst_rgb = 0x8001; // GL_CONSTANT_COLOR

        assert_eq!(mixed_constant_blend_source_scale(&draw), Some([0.25; 3]));
        assert_eq!(blend_constant(&draw), Some(draw.blend_color));

        draw.blend_src_rgb = 0x8002; // GL_ONE_MINUS_CONSTANT_COLOR
        draw.blend_dst_rgb = 0x8003; // GL_CONSTANT_ALPHA
        assert_eq!(
            mixed_constant_blend_source_scale(&draw),
            Some([0.9, 0.8, 0.7])
        );
        assert_eq!(blend_constant(&draw), Some([0.75; 4]));

        draw.blend = false;
        assert_eq!(mixed_constant_blend_source_scale(&draw), None);
    }

    #[test]
    fn mixed_constant_source_scale_is_injected_before_fragment_return() {
        let source = "#version 440\nlayout(location = 0) out vec4 color;\nvoid main() { color = vec4(0.5); }\n";
        let rewritten = crate::adapter::glsl::Source::new(source)
            .scale_fragment_outputs([0.25, 0.5, 0.75]);
        assert!(rewritten.contains(
            "color.rgb = clamp(color.rgb, vec3(0.0), vec3(1.0)) * vec3(0.250000000, 0.500000000, 0.750000000);"
        ));
        assert!(
            rewritten.find("color = vec4(0.5)").unwrap()
                < rewritten.find("color.rgb = clamp").unwrap()
        );
    }
}
