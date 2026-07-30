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
    let prog_name = if d.prog != 0 {
        d.prog
    } else {
        ctx.local.cur_prog
    };
    let Some(prog) = ctx.programs.program(prog_name).cloned() else {
        return None;
    };
    let Some(vs_ir) = prog.vs_ir.clone() else {
        return None;
    };
    let Some(fs_ir) = prog.fs_ir.clone() else {
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
    let (vs_id, fs_id, shaders_new) = ctx
        .program_shader_ir(prog_name, sample_variant, prog.link_gen)
        .ok()?;
    if shaders_new {
        let vs_ir = if bottom_up {
            use hl_gpu::protocol::model::kernel::GlslDescriptor;
            let mut descriptor = GlslDescriptor::from_words(&vs_ir)
                .and_then(|result| result.ok())
                .expect("linked GL vertex shader is a GLSL descriptor");
            descriptor.source =
                crate::adapter::glsl::Source::new(&descriptor.source).present_coordinates();
            descriptor.to_words()
        } else {
            vs_ir
        };
        cmds.push(Cmd::CreateShader {
            id: vs_id,
            kind: ShaderPayloadKind::Glsl,
            spirv: vs_ir,
        });
        let fs_ir = if sample_transforms.is_empty() && !correct_frag_coord {
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
            attrs.push(VertexAttr {
                location: l as u32,
                format: fmt,
                offset: off,
            });
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
            attrs: vec![VertexAttr {
                location: cs.location,
                format: cs.format,
                offset: 0,
            }],
        });
    }
    // Fixed-function state → the pipeline's blend / depth / cull descriptor (the values a real app set via
    // glBlendFunc / glDepthFunc / glCullFace / glFrontFace, mapped to their opaque WebGPU wire enums).
    let blend = if d.blend {
        Some(BlendState {
            src_color: Pipeline::blend_factor(d.blend_src_rgb),
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
    let color_targets: Vec<ColorTargetState> = (0..n_color_targets.max(1))
        .map(|_| ColorTargetState {
            format: target_fmt,
            blend: blend.clone(),
            write_mask: d.color_mask & 0xf,
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
    // Program-keyed pipeline residency: the render pipeline depends on the program's shaders PLUS this draw's
    // fixed-function + vertex-layout state, so the cache key folds a signature of that state in. A program
    // re-drawn with the SAME state reuses its resident pipeline (no `CreateRenderPipeline`); a genuinely new
    // state variant creates one more. Reused across frames + invalidated on relink (see `program_pipeline_ir`).
    let state_key = pipeline_state_key(
        &vbs,
        &color_targets,
        &depth,
        topology,
        cull,
        front_face,
        sample_count,
    ) ^ sample_variant.rotate_left(17);
    let (pipeline_ir, pipe_new) = ctx
        .program_pipeline_ir(prog_name, state_key, prog.link_gen)
        .ok()?;
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
                vertex_buffers: vbs,
                color_targets,
                depth,
                topology,
                cull,
                front_face,
                sample_count,
                label: String::new(),
            },
        ));
    }

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
        copies.push(Enc::CopyBufferToTexture {
            src: tb.stage_ir,
            src_offset: 0,
            bytes_per_row: tb.w * 4,
            dst: tb.tex_ir,
            mip: 0,
            width: tb.w,
            height: tb.h,
        });
    }
    let mut ops: Vec<Enc> = Vec::new();
    ops.push(Enc::SetPipeline(pipeline_ir));
    ops.push(emit_viewport(&d, tw, th, bottom_up));
    ops.push(emit_scissor(&d, tw, th, bottom_up));
    // Dynamic stencil reference — the value the pipeline's stencil compare tests against and a `GL_REPLACE`
    // op writes. Emitted per-draw inside the pass like viewport/scissor (the compare/ops/masks live
    // statically on the pipeline's `DepthState`), so two draws with different `glStencilFunc` references
    // lower correctly. Masked to the 8-bit stencil buffer, and only when this draw stencil-tests.
    if d.stencil {
        ops.push(Enc::SetStencilReference {
            reference: (d.stencil_ref.max(0) as u32) & 0xff,
        });
    }
    if [
        d.blend_src_rgb,
        d.blend_dst_rgb,
        d.blend_src_alpha,
        d.blend_dst_alpha,
    ]
    .iter()
    .any(|factor| matches!(*factor, 0x8001..=0x8004))
    {
        ops.push(Enc::SetBlendConstant {
            color: d.blend_color,
        });
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
        ops.push(Enc::DrawIndexed {
            index_count,
            instance_count: d.instance_count,
            first_index: 0,
            base_vertex,
            first_instance: d.first_instance,
        });
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
        ops.push(Enc::Draw {
            vertex_count: d.count as u32,
            instance_count: d.instance_count,
            first_vertex: d.first as u32,
            first_instance: d.first_instance,
        });
    }

    Some(DrawCommands { copies, ops })
}

// Fixed pipeline state encoding continues in `pipeline`.
