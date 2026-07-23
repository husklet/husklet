use super::*;

// ==================================================================================================
// graphics pipeline
// ==================================================================================================

/// Translate a `VkPipelineVertexInputStateCreateInfo` into the neutral per-binding vertex layouts (the
/// host rasterizer fetches slot-0 positions/colors from these).
struct VertexLayouts;
impl VertexLayouts {
    fn parse(vi: *const VkPipelineVertexInputStateCreateInfo) -> Vec<VertexLayout> {
        let Some(vi) = (unsafe { vi.as_ref() }) else {
            return Vec::new();
        };
        let bindings: &[VkVertexInputBindingDescription] =
            if vi.p_vertex_binding_descriptions.is_null()
                || vi.vertex_binding_description_count == 0
            {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        vi.p_vertex_binding_descriptions,
                        vi.vertex_binding_description_count as usize,
                    )
                }
            };
        let attrs: &[VkVertexInputAttributeDescription] =
            if vi.p_vertex_attribute_descriptions.is_null()
                || vi.vertex_attribute_description_count == 0
            {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        vi.p_vertex_attribute_descriptions,
                        vi.vertex_attribute_description_count as usize,
                    )
                }
            };
        bindings
            .iter()
            .map(|b| VertexLayout {
                stride: b.stride,
                step_mode: b.input_rate as u32,
                attrs: attrs
                    .iter()
                    .filter(|a| a.binding == b.binding)
                    .map(|a| VertexAttr {
                        location: a.location,
                        format: a.format as u32,
                        offset: a.offset,
                    })
                    .collect(),
            })
            .collect()
    }
}

/// Walk a pNext chain for `VkPipelineRenderingCreateInfo` and read its `pColorAttachmentFormats` into the
/// neutral color-target formats (a dynamic-rendering pipeline's color targets). Empty when absent / no
/// color formats (a valid depth-only or no-color pipeline).
struct RenderingInfo;
impl RenderingInfo {
    fn color_formats(p_next: *const c_void) -> Vec<TextureFormat> {
        let mut node = p_next as *const VkBaseInStructure;
        while let Some(n) = unsafe { node.as_ref() } {
            if n.s_type == VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO {
                let pr = unsafe { &*(node as *const VkPipelineRenderingCreateInfo) };
                if pr.p_color_attachment_formats.is_null() || pr.color_attachment_count == 0 {
                    return Vec::new();
                }
                let fmts = unsafe {
                    std::slice::from_raw_parts(
                        pr.p_color_attachment_formats,
                        pr.color_attachment_count as usize,
                    )
                };
                return fmts.iter().map(|&f| Format(f as u32).wire()).collect();
            }
            node = n.p_next;
        }
        Vec::new()
    }

    /// Walk a pNext chain for `VkPipelineRenderingCreateInfo` and read its `depthAttachmentFormat` — the depth
    /// format a dynamic-rendering (null `renderPass`) pipeline targets. `None` when the struct is absent or the
    /// format is `VK_FORMAT_UNDEFINED` (0), i.e. a color-only pipeline.
    fn depth_format(p_next: *const c_void) -> Option<TextureFormat> {
        let mut node = p_next as *const VkBaseInStructure;
        while let Some(n) = unsafe { node.as_ref() } {
            if n.s_type == VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO {
                let pr = unsafe { &*(node as *const VkPipelineRenderingCreateInfo) };
                // VK_FORMAT_UNDEFINED (0) => no depth attachment.
                return (pr.depth_attachment_format != 0)
                    .then(|| Format(pr.depth_attachment_format as u32).wire());
            }
            node = n.p_next;
        }
        None
    }
}

/// Translate one `VkStencilOpState` face onto the neutral [`StencilFaceState`]. `VkStencilOp`
/// (KEEP=0 … DECREMENT_AND_WRAP=7) and `VkCompareOp` (NEVER=0 … ALWAYS=7) share the neutral
/// `stencil_op::*` / `compare::*` numbering verbatim, so every field maps 1:1.
struct Stencil;
impl Stencil {
    fn face(s: &VkStencilOpState) -> StencilFaceState {
        StencilFaceState {
            compare: s.compare_op as u32,
            fail_op: s.fail_op as u32,
            depth_fail_op: s.depth_fail_op as u32,
            pass_op: s.pass_op as u32,
        }
    }
}

/// Translate a `VkPipelineDepthStencilStateCreateInfo` into the neutral [`DepthState`] when the depth OR
/// stencil test is enabled. `depth_format` is the pass's depth attachment format (from the render pass /
/// dynamic-rendering pNext); when unresolved it defaults to `Depth24PlusStencil8` for a stencil-enabled
/// pipeline (the stencil plane must exist) else `Depth32Float`. Returns `None` for a null state pointer or a
/// pipeline with BOTH tests disabled — exactly the pipelines that must NOT carry a depth attachment.
///
/// A disabled depth test with an enabled stencil test yields an `ALWAYS` depth compare + no depth write (a
/// stencil-only pass — UI masking / portals), so the stencil op runs without occluding. The per-face
/// `VkStencilOpState` (fail/pass/depthFail ops + compareOp) threads to `stencil_front`/`stencil_back`, and
/// the front face's `compareMask`/`writeMask` thread to the neutral single read/write masks (WebGPU/wgpu
/// carry ONE mask pair for both faces). Without this the stencil state was DROPPED (`DepthState::depth_only`
/// forced the inert `DISABLED` faces) and every stencil-gated draw ran with the stencil test off.
struct DepthStencil;
impl DepthStencil {
    fn parse(
        p_depth_stencil_state: *const c_void,
        depth_format: Option<TextureFormat>,
    ) -> Option<DepthState> {
        let ds = unsafe {
            (p_depth_stencil_state as *const VkPipelineDepthStencilStateCreateInfo).as_ref()
        }?;
        let depth_enabled = ds.depth_test_enable != 0;
        let stencil_enabled = ds.stencil_test_enable != 0;
        if !depth_enabled && !stencil_enabled {
            return None;
        }
        let format = depth_format.unwrap_or(if stencil_enabled {
            TextureFormat::Depth24PlusStencil8
        } else {
            TextureFormat::Depth32Float
        });
        let (stencil_front, stencil_back, read_mask, write_mask) = if stencil_enabled {
            (
                Stencil::face(&ds.front),
                Stencil::face(&ds.back),
                ds.front.compare_mask,
                ds.front.write_mask,
            )
        } else {
            (
                StencilFaceState::DISABLED,
                StencilFaceState::DISABLED,
                0xffff_ffff,
                0xffff_ffff,
            )
        };
        Some(DepthState {
            format,
            depth_write: depth_enabled && ds.depth_write_enable != 0,
            depth_compare: if depth_enabled {
                ds.depth_compare_op as u32
            } else {
                compare::ALWAYS
            },
            stencil_front,
            stencil_back,
            stencil_read_mask: read_mask,
            stencil_write_mask: write_mask,
        })
    }
}

/// Translate a `VkBlendFactor` onto the neutral `hl_gpu` blend-factor wire numbering the GL driver emits
/// (0=ZERO 1=ONE 2=SRC_COLOR 3=1-SRC_COLOR 4=SRC_ALPHA 5=1-SRC_ALPHA 6=DST_COLOR 7=1-DST_COLOR 8=DST_ALPHA
/// 9=1-DST_ALPHA 10=SRC_ALPHA_SATURATE) that `hl-gpu-wgpu`'s `blend_factor` decodes. VkBlendFactor
/// interleaves color/alpha differently (SRC_ALPHA=6, DST_COLOR=4) so the mapping is NOT identity. An
/// unmodeled factor defaults to ONE, matching the executor's own fallback rather than dropping the blend.
struct Blend;
impl Blend {
    fn factor(f: i32) -> u32 {
        match f {
            0 => 0,   // VK_BLEND_FACTOR_ZERO
            1 => 1,   // VK_BLEND_FACTOR_ONE
            2 => 2,   // VK_BLEND_FACTOR_SRC_COLOR
            3 => 3,   // VK_BLEND_FACTOR_ONE_MINUS_SRC_COLOR
            4 => 6,   // VK_BLEND_FACTOR_DST_COLOR
            5 => 7,   // VK_BLEND_FACTOR_ONE_MINUS_DST_COLOR
            6 => 4,   // VK_BLEND_FACTOR_SRC_ALPHA
            7 => 5,   // VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA
            8 => 8,   // VK_BLEND_FACTOR_DST_ALPHA
            9 => 9,   // VK_BLEND_FACTOR_ONE_MINUS_DST_ALPHA
            14 => 10, // VK_BLEND_FACTOR_SRC_ALPHA_SATURATE
            _ => 1,
        }
    }

    /// Translate a `VkBlendOp` onto the neutral blend-op wire numbering (0=ADD 1=SUBTRACT 2=REVERSE_SUBTRACT
    /// 3=MIN 4=MAX). `VkBlendOp` (ADD=0 … MAX=4) already matches this ordering 1:1; an unmodeled op defaults
    /// to ADD.
    fn operation(o: i32) -> u32 {
        match o {
            1 => 1, // VK_BLEND_OP_SUBTRACT
            2 => 2, // VK_BLEND_OP_REVERSE_SUBTRACT
            3 => 3, // VK_BLEND_OP_MIN
            4 => 4, // VK_BLEND_OP_MAX
            _ => 0, // VK_BLEND_OP_ADD (and unmodeled)
        }
    }

    /// Translate a `VkPipelineColorBlendStateCreateInfo`'s FIRST attachment into the neutral [`BlendState`]
    /// when that attachment's `blendEnable` is set. The software rasterizer models one blend for all color
    /// targets, so only attachment 0 is read. Returns `None` for a null state pointer, an empty attachment
    /// list, or `blendEnable = VK_FALSE` — exactly the pipelines that must OVERWRITE (opaque replace) rather
    /// than composite. Without this the color-blend state was dropped (`blend: None` hardcoded) and a
    /// translucent draw overwrote the destination instead of alpha-compositing over it.
    fn parse(p_color_blend_state: *const c_void) -> Option<BlendState> {
        let cb = unsafe {
            (p_color_blend_state as *const VkPipelineColorBlendStateCreateInfo).as_ref()
        }?;
        if cb.attachment_count == 0 || cb.p_attachments.is_null() {
            return None;
        }
        let att = unsafe { &*cb.p_attachments };
        if att.blend_enable == 0 {
            return None;
        }
        Some(BlendState {
            src_color: Blend::factor(att.src_color_blend_factor),
            dst_color: Blend::factor(att.dst_color_blend_factor),
            op_color: Blend::operation(att.color_blend_op),
            src_alpha: Blend::factor(att.src_alpha_blend_factor),
            dst_alpha: Blend::factor(att.dst_alpha_blend_factor),
            op_alpha: Blend::operation(att.alpha_blend_op),
        })
    }
}

/// Read a `VkPipelineMultisampleStateCreateInfo`'s `rasterizationSamples` as the pipeline's multisample
/// count. `rasterizationSamples` is a `VkSampleCountFlagBits` whose bit VALUE is the count itself
/// (`_1_BIT`=1, `_2_BIT`=2, `_4_BIT`=4, `_8_BIT`=8, …), so the value is returned verbatim. A null pointer
/// (a pipeline with no multisample state — spec-legal for a rasterization-discard pipeline) or a `0`/`_1_BIT`
/// count folds to `1` (single-sample), keeping an existing non-MSAA pipeline byte-identical. Without this
/// the sample count was dropped (`sample_count: 1` hardcoded) and an MSAA pipeline rasterized single-sampled.
struct Multisample;
impl Multisample {
    fn samples(p_multisample_state: *const c_void) -> u32 {
        let Some(ms) = (unsafe {
            (p_multisample_state as *const VkPipelineMultisampleStateCreateInfo).as_ref()
        }) else {
            return 1;
        };
        (ms.rasterization_samples as u32).max(1)
    }
}

/// Read a `VkPipelineInputAssemblyStateCreateInfo`'s `topology` as the pipeline's primitive-assembly mode.
/// `VkPrimitiveTopology` shares WebGPU's numbering for every mode our IR carries (0 POINT_LIST, 1 LINE_LIST,
/// 2 LINE_STRIP, 3 TRIANGLE_LIST, 4 TRIANGLE_STRIP), so those map straight onto the wire [`Topology`]. A null
/// pInputAssemblyState or a topology our IR/executor cannot express (TRIANGLE_FAN, the *_ADJACENCY modes,
/// PATCH_LIST) folds to `TriangleList`. Without this the topology was DROPPED (`Topology::TriangleList`
/// hardcoded in create.rs) and a pipeline drawing 4-vertex TRIANGLE_STRIP quads (GPUI's entire UI: the
/// window/panel/glyph quads) rasterized only the FIRST triangle of each quad — every rectangle collapsed to
/// a half-rectangle triangle.
struct InputAssembly;
impl InputAssembly {
    fn topology(p_input_assembly_state: *const c_void) -> Topology {
        let Some(ia) = (unsafe {
            (p_input_assembly_state as *const VkPipelineInputAssemblyStateCreateInfo).as_ref()
        }) else {
            return Topology::TriangleList;
        };
        match ia.topology {
            0 => Topology::PointList,
            1 => Topology::LineList,
            2 => Topology::LineStrip,
            3 => Topology::TriangleList,
            4 => Topology::TriangleStrip,
            _ => Topology::TriangleList,
        }
    }
}

/// Read a `VkPipelineRasterizationStateCreateInfo`'s `cullMode` + `frontFace` as the pipeline's neutral
/// `(cull, front_face)`. `VkCullModeFlags` (NONE=0, FRONT_BIT=1, BACK_BIT=2, FRONT_AND_BACK=3) maps to the
/// neutral `cull` (0 none / 1 front / 2 back); `FRONT_AND_BACK` (cull-all) is not expressible in the neutral
/// pipeline and folds to `0` (none) — content is drawn rather than silently vanishing. `VkFrontFace`
/// (COUNTER_CLOCKWISE=0, CLOCKWISE=1) matches the neutral `front_face` (0 CCW / 1 CW) verbatim. A null
/// pRasterizationState folds to `(0, 0)`. Without this the cull + winding were DROPPED (`cull: 0`,
/// `front_face: 0` hardcoded in create.rs) and a back-face-culled solid mesh drew its interior/back
/// triangles bleeding through the front.
struct Rasterization;
impl Rasterization {
    fn parse(p_rasterization_state: *const c_void) -> (u32, u32) {
        let Some(rs) = (unsafe {
            (p_rasterization_state as *const VkPipelineRasterizationStateCreateInfo).as_ref()
        }) else {
            return (0, 0);
        };
        let cull = match rs.cull_mode {
            1 => 1,
            2 => 2,
            _ => 0,
        };
        let front_face = if rs.front_face == 1 { 1 } else { 0 };
        (cull, front_face)
    }
}

/// Read a `VkPipelineColorBlendStateCreateInfo`'s FIRST attachment's `colorWriteMask` as the pipeline's
/// neutral RGBA write mask (the software rasterizer applies one write mask to all targets, mirroring the
/// one-blend model). `VkColorComponentFlags` (R=0x1 G=0x2 B=0x4 A=0x8) matches the neutral `write_mask` low
/// 4 bits verbatim. A null state / empty attachment list folds to `0xf` (write all channels — the prior
/// default). Without this the write mask was DROPPED (`write_mask: 0xf` hardcoded in create.rs) and a
/// channel-masked draw (e.g. a depth-prepass `colorWriteMask = 0`, or preserving destination alpha) wrote
/// color it must have left untouched.
struct ColorMask;
impl ColorMask {
    fn parse(p_color_blend_state: *const c_void) -> u32 {
        let Some(cb) = (unsafe {
            (p_color_blend_state as *const VkPipelineColorBlendStateCreateInfo).as_ref()
        }) else {
            return 0xf;
        };
        if cb.attachment_count == 0 || cb.p_attachments.is_null() {
            return 0xf;
        }
        (unsafe { &*cb.p_attachments }.color_write_mask) & 0xf
    }
}

#[no_mangle]
pub extern "C" fn vkCreateGraphicsPipelines(
    _device: *mut c_void,
    _pipeline_cache: u64,
    create_info_count: u32,
    p_create_infos: *const c_void,
    _p_allocator: *const c_void,
    p_pipelines: *mut u64,
) -> VkResult {
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(
            p_create_infos as *const VkGraphicsPipelineCreateInfo,
            create_info_count as usize,
        )
    };
    let out = unsafe { std::slice::from_raw_parts_mut(p_pipelines, create_info_count as usize) };
    let mut result = VK_SUCCESS;
    for (i, ci) in infos.iter().enumerate() {
        out[i] = 0;
        // Resolve the vertex (+ optional fragment) stage module + entry from pStages.
        let stages: &[VkPipelineShaderStageCreateInfo] = if ci.p_stages.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ci.p_stages, ci.stage_count as usize) }
        };
        let mut vertex: Option<(u64, String)> = None;
        let mut fragment: Option<(u64, String)> = None;
        for st in stages {
            let entry = unsafe { EntryPoint::read(st.p_name) }.to_string();
            if st.stage & VK_SHADER_STAGE_VERTEX_BIT != 0 {
                vertex = Some((st.module, entry));
            } else if st.stage & VK_SHADER_STAGE_FRAGMENT_BIT != 0 {
                fragment = Some((st.module, entry));
            }
        }
        let Some((vmod, ventry)) = vertex else {
            result = VK_ERROR_UNKNOWN;
            continue;
        };
        let layouts = VertexLayouts::parse(ci.p_vertex_input_state);
        // The color-target formats: from the bound VkRenderPass's attachment in the classic path, or —
        // for a VK_KHR_dynamic_rendering pipeline (null renderPass) — from the
        // VkPipelineRenderingCreateInfo::pColorAttachmentFormats carried in the pNext chain.
        let color_formats: Vec<TextureFormat> = if ci.render_pass == 0 {
            RenderingInfo::color_formats(ci.p_next)
        } else {
            let fmt = StateStore::with(|s| {
                s.render_passes
                    .get(&ci.render_pass)
                    .map(|r| Format(r.color_format_vk).wire())
            });
            vec![fmt.unwrap_or(TextureFormat::Rgba8Unorm)]
        };

        // Depth-test state: the depth attachment format comes from the dynamic-rendering pNext
        // (VkPipelineRenderingCreateInfo::depthAttachmentFormat) for a null-renderPass pipeline, or — for a
        // classic pipeline bound to a VkRenderPass — from that pass's declared depth attachment. Falls back to
        // Depth32Float when a depth-tested pipeline resolves no explicit format. A null pDepthStencilState /
        // disabled test => no depth.
        let depth_format = if ci.render_pass == 0 {
            RenderingInfo::depth_format(ci.p_next)
        } else {
            StateStore::with(|s| {
                s.render_passes
                    .get(&ci.render_pass)
                    .and_then(|r| r.depth)
                    .map(|d| Format(d.format_vk).wire())
            })
        };
        let depth = DepthStencil::parse(ci.p_depth_stencil_state, depth_format);

        // Color-blend state: the first attachment's blendEnable + factors/ops, mapped onto the neutral
        // blend wire numbering. A null pColorBlendState / blendEnable = VK_FALSE => None (opaque overwrite),
        // preserving the pre-blend behavior.
        let blend = Blend::parse(ci.p_color_blend_state);

        // Multisample state: rasterizationSamples → the pipeline's MSAA count. Null / _1_BIT => single-sample.
        let sample_count = Multisample::samples(ci.p_multisample_state);

        // Input-assembly topology: the real VkPrimitiveTopology (GPUI's quads are 4-vertex TRIANGLE_STRIP).
        let topology = InputAssembly::topology(ci.p_input_assembly_state);

        // Rasterization cull state: cullMode + frontFace (a back-face-culled solid mesh must not show its
        // interior). Null pRasterizationState => (none, CCW).
        let (cull, front_face) = Rasterization::parse(ci.p_rasterization_state);

        // Per-attachment colorWriteMask (attachment 0, applied to every color target — the one-mask model).
        let color_write_mask = ColorMask::parse(ci.p_color_blend_state);

        let r = ShimState::with_sink(|dev, sink| {
            let frag = fragment.as_ref().map(|(m, e)| (*m, e.as_str()));
            create::create_graphics_pipeline(
                dev,
                sink,
                (vmod, ventry.as_str()),
                frag,
                layouts,
                color_formats,
                depth,
                blend,
                sample_count,
                topology,
                cull,
                front_face,
                color_write_mask,
            )
        })
        .unwrap_or(Err(hl_gpu::GpuError::Invalid(
            "vkCreateGraphicsPipelines: no device",
        )));
        match r {
            Ok(h) => out[i] = h,
            Err(e) => result = Status::from_error(&e),
        }
    }
    result
}
