use super::*;

/// `SetViewport` for a `tw`×`th` target.
///
/// The recorded rect is GL window space (origin bottom-left). A target storing rows top-down converts the
/// row (`th - y - h`); a `bottom_up` target — an imported external image, see
/// [`RenderPasses::stores_bottom_up_rows`] — keeps the recorded row, exactly as its scissor does.
pub(super) fn emit_viewport(d: &DrawCall, tw: i32, th: i32, bottom_up: bool) -> Enc {
    let (mut x, mut y, mut w, mut h) = (0.0f32, 0.0f32, tw as f32, th as f32);
    if d.viewport[2] > 0 && d.viewport[3] > 0 {
        x = d.viewport[0] as f32;
        w = d.viewport[2] as f32;
        h = d.viewport[3] as f32;
        y = if bottom_up {
            d.viewport[1] as f32
        } else {
            (th - d.viewport[1] - d.viewport[3]) as f32
        };
    }
    Enc::SetViewport {
        x,
        y,
        w,
        h,
        // `glDepthRangef`. This was hard-coded to `[0, 1]`, so a fragment landed at its default-range
        // depth whatever range the application had set — invisible to any test using the default, and
        // wrong for every one that does not. `n > f` is legal in GL and passes through unchanged; the
        // host validates each component's range, not their order.
        min_depth: d.depth_range[0],
        max_depth: d.depth_range[1],
    }
}

/// `SetScissor` clamped against a `tw`×`th` target.
///
/// Rows of a top-down target are converted from GL's bottom-left origin; a `bottom_up` target keeps its
/// recorded row (see [`RenderPasses::stores_bottom_up_rows`]).
pub(super) fn emit_scissor(d: &DrawCall, tw: i32, th: i32, bottom_up: bool) -> Enc {
    let (x, y, w, h) = if d.scissor_enabled {
        let left = d.scissor[0];
        let top = if bottom_up {
            d.scissor[1]
        } else {
            th.saturating_sub(d.scissor[1].saturating_add(d.scissor[3]))
        };
        let right = left.saturating_add(d.scissor[2]);
        let bottom = top.saturating_add(d.scissor[3]);
        let x0 = left.clamp(0, tw);
        let y0 = top.clamp(0, th);
        let x1 = right.clamp(0, tw);
        let y1 = bottom.clamp(0, th);
        (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    } else {
        (0, 0, tw, th)
    };
    Enc::SetScissor {
        x: x as u32,
        y: y as u32,
        w: w.max(0) as u32,
        h: h.max(0) as u32,
    }
}

/// Lower a `glBlitFramebuffer` color sub-rect, flipping the GL bottom-left window rects into the render
/// targets' top-left texel origin.
///
/// A MIRRORED blit — `x1 < x0` (or `y1 < y0`) on one side but not the other — is legal GL and reverses
/// the destination's row or column order. The min/max below already computes the comparison that decides
/// it; that result used to be discarded, so a mirrored blit produced an unmirrored image. The NET flip
/// (source inversion exclusive-or destination inversion) now rides along on `Enc::BlitTexture`. Because
/// the exact copy cannot mirror, a net-flipped blit takes the resampling path even at equal extents and
/// matching formats.
///
/// A SCALING blit (source extent != destination extent) or one that CHANGES FORMAT lowers to
/// `Enc::BlitTexture` carrying `filter`, which resamples and converts. Only an equal-size, same-format
/// blit lowers to the exact `Enc::CopyTextureToTexture`, which moves bytes.
///
/// The format half was missing, and the two format parameters were plumbed in and then ignored — the
/// signature said `_src_fmt` and `_dst_fmt`. So a non-scaling blit between differently formatted
/// framebuffers became a byte REINTERPRET where `glBlitFramebuffer` is defined to convert: an
/// `Rgba8Unorm` into a `Bgra8Unorm` attachment silently swapped every channel, reachable today through
/// `GL_BGRA8_EXT` with no extension involved. Advertising float colour buffers widened it further, since
/// a half-float source into an eight-bit attachment then differs in texel SIZE as well as meaning.
///
/// Returns `None` only for a degenerate (empty) source or destination rect.
#[allow(clippy::too_many_arguments)]
pub(super) fn blit_copy_enc(
    src: &[i32; 4],
    dst: &[i32; 4],
    src_tex: u32,
    src_th: i32,
    src_fmt: TextureFormat,
    dst_tex: u32,
    dst_th: i32,
    dst_fmt: TextureFormat,
    filter: Filter,
) -> Option<Enc> {
    let (sx0, sx1) = (src[0].min(src[2]), src[0].max(src[2]));
    let (sy0, sy1) = (src[1].min(src[3]), src[1].max(src[3]));
    let (dx0, dx1) = (dst[0].min(dst[2]), dst[0].max(dst[2]));
    let (dy0, dy1) = (dst[1].min(dst[3]), dst[1].max(dst[3]));
    // Both sides get the same bottom-left-to-top-left y reflection below, so it cancels out of the net
    // vertical flip and the comparison is taken on the GL rects as given.
    // `z` is false on both sides by construction, not by default: `glBlitFramebuffer` names two 2D
    // rects and has no depth axis to invert, so a GL blit can never carry a depth flip.
    let mirror = Mirror::net(
        Mirror {
            x: src[2] < src[0],
            y: src[3] < src[1],
            z: false,
        },
        Mirror {
            x: dst[2] < dst[0],
            y: dst[3] < dst[1],
            z: false,
        },
    );
    let (sw, sh) = (sx1 - sx0, sy1 - sy0);
    let (dw, dh) = (dx1 - dx0, dy1 - dy0);
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return None;
    }
    // GL y is bottom-left; a region's TOP row in a top-left texture is `height - y_max`.
    let src_oy = (src_th - sy1).max(0) as u32;
    let dst_oy = (dst_th - dy1).max(0) as u32;
    if sw != dw || sh != dh || src_fmt != dst_fmt || mirror != Mirror::NONE {
        // Scaling OR format-changing blit: resample and convert the source rect into the destination
        // rect. The wgpu executor draws a filtered textured quad for this (its `Enc::BlitTexture`
        // implementation), which is what performs the conversion `glBlitFramebuffer` promises; the exact
        // copy below cannot convert and must not be used when the formats disagree.
        return Some(Enc::BlitTexture {
            src: src_tex,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d {
                x: sx0.max(0) as u32,
                y: src_oy,
                z: 0,
            },
            src_extent: Extent3d {
                width: sw as u32,
                height: sh as u32,
                depth: 1,
            },
            dst: dst_tex,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d {
                x: dx0.max(0) as u32,
                y: dst_oy,
                z: 0,
            },
            dst_extent: Extent3d {
                width: dw as u32,
                height: dh as u32,
                depth: 1,
            },
            filter,
            mirror,
        });
    }
    Some(Enc::CopyTextureToTexture {
        src: src_tex,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d {
            x: sx0.max(0) as u32,
            y: src_oy,
            z: 0,
        },
        dst: dst_tex,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d {
            x: dx0.max(0) as u32,
            y: dst_oy,
            z: 0,
        },
        extent: Extent3d {
            width: sw as u32,
            height: sh as u32,
            depth: 1,
        },
    })
}

/// A stable signature of the pipeline-state a draw contributes on top of its program's (cached) shader
/// modules: the vertex-buffer layouts, the color targets (format + blend), the depth state, the primitive
/// topology, the cull mode + front-face winding, and the MSAA sample count. Two draws of the same program
/// with an equal signature share one render pipeline (see [`GlContext::program_pipeline_ir`]); any
/// difference mints a new one.
///
/// These descriptor types derive `Debug` but not `Hash` (and live in the `hl_gpu` crate this shim does not
/// modify), so a canonical `Debug` rendering is the hash input: structurally-equal state renders identically
/// and hashes equal, while any change (blend, depth, topology, a vertex attribute, cull/winding, the sample
/// count, or the target format) renders differently and hashes apart. None of these fields carry floats, so
/// the rendering is exact.
pub(super) fn pipeline_state_key(
    vbs: &[VertexLayout],
    color_targets: &[ColorTargetState],
    depth: &Option<DepthState>,
    topology: Topology,
    cull: u32,
    front_face: u32,
    sample_count: u32,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!(
        "{:?}",
        (
            vbs,
            color_targets,
            depth,
            topology,
            cull,
            front_face,
            sample_count
        )
    )
    .hash(&mut h);
    h.finish()
}

/// GL primitive mode → the neutral pipeline [`Topology`] the executor rasterizes with. Previously only
/// `GL_TRIANGLE_STRIP` was distinguished and EVERY other mode folded to `TriangleList`, so a
/// `glDrawArrays(GL_LINES)` / `GL_POINTS` / `GL_LINE_STRIP` silently rasterized as triangles. Each GL mode
/// with a neutral equivalent now maps to it — the protocol's `Topology` offers `PointList` / `LineList` /
/// `LineStrip` / `TriangleList` / `TriangleStrip` (see `hl_gpu::protocol::model::enums::Topology`), all of
/// which the wgpu executor honors (`hl_gpu-wgpu` `pipeline::topology`).
///
/// `GL_LINE_LOOP` and `GL_TRIANGLE_FAN` have no neutral variant, so [`expanded_array_indices`] converts
/// non-indexed draws into exact line/triangle lists before submission.
///
/// Any unrecognized mode also falls back to `TriangleList` and never panics.
pub(super) struct PrimitiveAssembly;

impl PrimitiveAssembly {
    pub(super) fn topology(mode: u32) -> Topology {
        // GL_LINE_LOOP (0x0002) and GL_TRIANGLE_FAN (0x0006) have no glconst here; matched by raw value.
        match mode {
        GL_POINTS => Topology::PointList,
        GL_LINES => Topology::LineList,
        0x0002 /* GL_LINE_LOOP */ => Topology::LineList,
        GL_LINE_STRIP => Topology::LineStrip,
        GL_TRIANGLE_STRIP => Topology::TriangleStrip,
        // GL_TRIANGLES, GL_TRIANGLE_FAN (0x0006, no neutral fan), and any unknown mode → safe TriangleList.
        _ => Topology::TriangleList,
    }
    }

    /// Expand a non-indexed GL primitive that has no neutral topology into an exact `u32` index list.
    pub(super) fn expanded_array_indices(mode: u32, first: i32, count: i32) -> Option<Vec<u32>> {
        if first < 0 || count < 0 {
            return None;
        }
        let first = first as u32;
        let count = count as u32;
        let indices = (first..first.checked_add(count)?).collect::<Vec<_>>();
        Some(Self::expand(mode, &indices))
    }

    pub(super) fn expand(mode: u32, source: &[u32]) -> Vec<u32> {
        match mode {
        0x0002 /* GL_LINE_LOOP */ => {
            if source.len() < 2 {
                return Vec::new();
            }
            let mut indices = Vec::with_capacity(source.len() * 2);
            for pair in source.windows(2) {
                indices.extend([pair[0], pair[1]]);
            }
            indices.extend([*source.last().unwrap(), source[0]]);
            indices
        }
        0x0006 /* GL_TRIANGLE_FAN */ => {
            if source.len() < 3 {
                return Vec::new();
            }
            let mut indices = Vec::with_capacity((source.len() - 2) * 3);
            for pair in source[1..].windows(2) {
                indices.extend([source[0], pair[0], pair[1]]);
            }
            indices
        }
        _ => source.to_vec(),
    }
    }

    pub(super) fn decode_indices(
        bytes: &[u8],
        offset: usize,
        kind: u32,
        count: i32,
    ) -> Option<Vec<u32>> {
        let count = usize::try_from(count).ok()?;
        let width = match kind {
            GL_UNSIGNED_BYTE => 1,
            GL_UNSIGNED_SHORT => 2,
            GL_UNSIGNED_INT => 4,
            _ => return None,
        };
        let end = offset.checked_add(count.checked_mul(width)?)?;
        let bytes = bytes.get(offset..end)?;
        Some(
            bytes
                .chunks_exact(width)
                .map(|chunk| match width {
                    1 => chunk[0] as u32,
                    2 => u16::from_le_bytes([chunk[0], chunk[1]]) as u32,
                    _ => u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(layouts: &[VertexLayout]) -> u64 {
        pipeline_state_key(
            layouts,
            &[ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: 0xf,
            }],
            &None,
            Topology::TriangleList,
            0,
            0,
            1,
        )
    }

    #[test]
    fn non_mipmapped_texture_filters_cannot_select_uploaded_mip_levels() {
        for min_filter in [GL_NEAREST, GL_LINEAR] {
            let texture = crate::model::texture::GlTexture {
                min_filter,
                ..Default::default()
            };
            assert_eq!(Pipeline::sampler_desc(&texture, &None).lod_max_clamp, 0.0);
        }
    }

    #[test]
    fn mipmapped_texture_filter_retains_the_level_range() {
        let texture = crate::model::texture::GlTexture {
            min_filter: GL_LINEAR_MIPMAP_LINEAR,
            ..Default::default()
        };
        assert_eq!(Pipeline::sampler_desc(&texture, &None).lod_max_clamp, 32.0);
    }

    #[test]
    fn non_mipmapped_sampler_object_overrides_its_lod_range_with_base_level() {
        let sampler = crate::model::es3::SamplerObj {
            min_filter: GL_LINEAR as i32,
            min_lod: 4.0,
            max_lod: 12.0,
            ..Default::default()
        };
        let descriptor = Pipeline::sampler_desc(
            &crate::model::texture::GlTexture::default(),
            &Some(sampler),
        );
        assert_eq!((descriptor.lod_min_clamp, descriptor.lod_max_clamp), (0.0, 0.0));
    }

    #[test]
    fn pipeline_identity_includes_complete_vertex_layout() {
        let base = VertexLayout {
            stride: 16,
            step_mode: 0,
            attrs: vec![VertexAttr {
                location: 0,
                format: vertex_format_wire(GL_FLOAT, 2, false, false),
                offset: 0,
            }],
        };
        let mut stride = base.clone();
        stride.stride = 32;
        let mut step = base.clone();
        step.step_mode = 1;
        let mut offset = base.clone();
        offset.attrs[0].offset = 8;
        let extra = vec![
            base.clone(),
            VertexLayout {
                stride: 16,
                step_mode: 0,
                attrs: vec![VertexAttr {
                    location: 1,
                    format: vertex_format_wire(GL_FLOAT, 2, false, false),
                    offset: 0,
                }],
            },
        ];

        assert_ne!(key(&[base.clone()]), key(&[stride]));
        assert_ne!(key(&[base.clone()]), key(&[step]));
        assert_ne!(key(&[base.clone()]), key(&[offset]));
        assert_ne!(key(&[base]), key(&extra));
    }

    #[test]
    fn enabled_zero_area_scissor_stays_empty() {
        let draw = DrawCall {
            scissor_enabled: true,
            scissor: [4, 5, 0, 8],
            ..DrawCall::default()
        };
        assert_eq!(
            emit_scissor(&draw, 32, 24, false),
            Enc::SetScissor {
                x: 4,
                y: 11,
                w: 0,
                h: 8,
            }
        );
    }

    #[test]
    fn scissor_clips_both_edges_without_growing() {
        let draw = DrawCall {
            scissor_enabled: true,
            scissor: [-5, -4, 12, 10],
            ..DrawCall::default()
        };
        assert_eq!(
            emit_scissor(&draw, 16, 16, false),
            Enc::SetScissor {
                x: 0,
                y: 10,
                w: 7,
                h: 6,
            }
        );
    }

    #[test]
    fn present_scissor_uses_host_surface_row_origin() {
        let draw = DrawCall {
            scissor_enabled: true,
            scissor: [2, 3, 7, 5],
            ..DrawCall::default()
        };
        assert_eq!(
            emit_scissor(&draw, 16, 16, true),
            Enc::SetScissor {
                x: 2,
                y: 3,
                w: 7,
                h: 5,
            }
        );
    }
}

/// The MSAA sample count the pass's render pipeline must declare so a multisampled attachment resolves (the
/// GL analogue of the Vulkan `sample_count` drop fixed earlier) — the count shared by the bound draw
/// framebuffer's color (and depth) attachments.
///
/// HONEST GAP: this GL model has no multisample-attachment representation yet. There is no
/// `glRenderbufferStorageMultisample` / `glTexStorage2DMultisample` entry point, no per-resource `samples`
/// field (`model::renderbuffer::Renderbuffer`, `model::texture::GlTexture`), and the `GL_SAMPLES` query
/// reads back 0 — so EVERY framebuffer this model can currently represent is single-sampled and this
/// returns 1. That is CORRECT for every representable state; the value is now sourced from ONE documented
/// place instead of a blind hardcode at the pipeline descriptor. When multisample-attachment tracking lands
/// (a `samples` field on the color/depth attachment + a `glRenderbufferStorageMultisample` /
/// `glTexStorage2DMultisample` recorder), read the attachment's sample count here AND raise the matching
/// `TextureDesc.sample_count` on the render-target + depth textures (`push_target_creates` /
/// `depth_attachment_for`) — MSAA then flows end to end with no other change to this lowering. Never panics;
/// a plain or unknown FBO yields 1.
/// Vertex-attribute format packing (`gl_shim.c` `vertex_format_wire`):
/// `comps | (kind<<8) | (normalized<<16) | (integer<<17)`, comps clamped to [1,4].
pub(super) fn vertex_format_wire(
    kind_enum: u32,
    comps: i32,
    normalized: bool,
    integer: bool,
) -> u32 {
    let comps = comps.clamp(1, 4) as u32;
    let kind = match kind_enum {
        GL_UNSIGNED_BYTE => 1,
        GL_BYTE => 2,
        GL_UNSIGNED_SHORT => 3,
        GL_SHORT => 4,
        GL_UNSIGNED_INT => 5,
        GL_INT => 6,
        GL_HALF_FLOAT => 7,
        GL_UNSIGNED_INT_2_10_10_10_REV if normalized && comps == 4 => 8,
        _ => 0, // GL_FLOAT and unknown
    };
    comps | (kind << 8) | ((normalized as u32) << 16) | ((integer as u32) << 17)
}

/// Build the `SamplerDesc` for a sampled texture, honoring a bound ES3 sampler OBJECT when present. A
/// `glBindSampler`d object overrides the texture's own filter/wrap (ES 3.0 §3.8.13); with no object bound
/// (`obj == None`) the texture parameters win — byte-identical to the pre-sampler-object path.
pub(super) struct Pipeline;

impl Pipeline {
    pub(super) fn sampler_desc(
        t: &crate::model::texture::GlTexture,
        obj: &Option<crate::model::es3::SamplerObj>,
    ) -> SamplerDesc {
        match obj {
            Some(o) => SamplerDesc {
                min_filter: o.ir_min_filter(),
                mag_filter: o.ir_mag_filter(),
                mip_filter: o.ir_mip_filter(),
                address_u: o.ir_wrap_s(),
                address_v: o.ir_wrap_t(),
                address_w: o.ir_wrap_r(),
                // This backend currently exposes mip levels from base level zero. Negative GL clamps are
                // therefore observationally identical to zero and are normalized before crossing into
                // WebGPU, whose sampler contract rejects negative clamps.
                lod_min_clamp: if matches!(o.min_filter as u32, GL_NEAREST | GL_LINEAR) {
                    0.0
                } else {
                    o.min_lod.max(0.0)
                },
                lod_max_clamp: if matches!(o.min_filter as u32, GL_NEAREST | GL_LINEAR) {
                    0.0
                } else {
                    o.max_lod.max(0.0)
                },
                compare: o.ir_compare(),
            },
            None => SamplerDesc {
                min_filter: t.ir_min_filter(),
                mag_filter: t.ir_mag_filter(),
                mip_filter: t.ir_mip_filter(),
                address_u: t.ir_wrap_s(),
                address_v: t.ir_wrap_t(),
                address_w: AddressMode::Repeat,
                lod_min_clamp: 0.0,
                // GL_NEAREST and GL_LINEAR are the two non-mipmapped minification modes. WebGPU has no
                // separate non-mip sampler state: with an ordinary max LOD it still selects another
                // level from the texture when derivatives demand minification. Clamp those two modes to
                // level zero; mipmapped GL filters retain the complete level range.
                lod_max_clamp: if matches!(t.min_filter, GL_NEAREST | GL_LINEAR) {
                    0.0
                } else {
                    32.0
                },
                compare: None,
            },
        }
    }

    /// GL blend factor enum → opaque WebGPU blend-factor wire value (`gl_shim.c` `blend_factor_wire`).
    pub(super) fn blend_factor(f: u32) -> u32 {
        use hl_gpu::protocol::model::enums::blend_factor as wire;
        match f {
            GL_ZERO => wire::ZERO,
            GL_ONE => wire::ONE,
            GL_SRC_COLOR => wire::SRC_COLOR,
            GL_ONE_MINUS_SRC_COLOR => wire::ONE_MINUS_SRC_COLOR,
            GL_SRC_ALPHA => wire::SRC_ALPHA,
            GL_ONE_MINUS_SRC_ALPHA => wire::ONE_MINUS_SRC_ALPHA,
            GL_DST_COLOR => wire::DST_COLOR,
            GL_ONE_MINUS_DST_COLOR => wire::ONE_MINUS_DST_COLOR,
            GL_DST_ALPHA => wire::DST_ALPHA,
            GL_ONE_MINUS_DST_ALPHA => wire::ONE_MINUS_DST_ALPHA,
            GL_SRC_ALPHA_SATURATE => wire::SRC_ALPHA_SATURATE,
            0x8001 | 0x8003 => wire::CONSTANT, // GL_CONSTANT_COLOR / GL_CONSTANT_ALPHA
            0x8002 | 0x8004 => wire::ONE_MINUS_CONSTANT, // GL_ONE_MINUS_CONSTANT_COLOR / _ALPHA
            GL_SRC1_COLOR => wire::SRC1_COLOR,
            GL_ONE_MINUS_SRC1_COLOR => wire::ONE_MINUS_SRC1_COLOR,
            GL_SRC1_ALPHA => wire::SRC1_ALPHA,
            GL_ONE_MINUS_SRC1_ALPHA => wire::ONE_MINUS_SRC1_ALPHA,
            _ => wire::ONE, // GL_ONE default for an unmodeled factor.
        }
    }

    /// GL blend equation enum → opaque WebGPU blend-op wire value (`gl_shim.c` `blend_op_wire`).
    pub(super) fn blend_op(e: u32) -> u32 {
        match e {
            GL_FUNC_SUBTRACT => 1,
            GL_FUNC_REVERSE_SUBTRACT => 2,
            GL_MIN => 3,
            GL_MAX => 4,
            _ => 0, // GL_FUNC_ADD and unknown.
        }
    }

    /// GL depth-compare enum → the neutral protocol compare code the executor decodes (`hl_gpu`'s
    /// `enums::compare`, Vulkan `VkCompareOp` ordering: NEVER=0 … ALWAYS=7). This MUST match those constants,
    /// NOT WebGPU's 1-based `CompareFunction`: the wgpu executor maps the wire value through `compare::*`
    /// (`pipeline::compare_function`), so an off-by-one here silently turns `GL_LESS` into `EQUAL` and rejects
    /// every depth-tested fragment.
    pub(super) fn compare(func: u32) -> u32 {
        use hl_gpu::protocol::model::enums::compare;
        match func {
            GL_NEVER => compare::NEVER,
            GL_LESS => compare::LESS,
            GL_EQUAL => compare::EQUAL,
            GL_LEQUAL => compare::LESS_EQUAL,
            GL_GREATER => compare::GREATER,
            GL_NOTEQUAL => compare::NOT_EQUAL,
            GL_GEQUAL => compare::GREATER_EQUAL,
            GL_ALWAYS => compare::ALWAYS,
            _ => compare::LESS, // GL_LESS default.
        }
    }

    /// Build the pipeline `DepthState` (depth compare + write, and the front/back stencil test/ops + masks)
    /// for a depth- or stencil-tested draw, at the pass's depth-attachment `format`. When the draw only
    /// stencil-tests (no `GL_DEPTH_TEST`), depth is neutral (`ALWAYS` compare, writes off) so the stencil test
    /// alone governs; the stencil faces are `DISABLED` when the draw does not stencil-test, reproducing the
    /// pure-depth behavior on a `Depth24PlusStencil8` pass.
    pub(super) fn depth_state(format: TextureFormat, d: &DrawCall) -> DepthState {
        Self::depth_state_for_face(format, d, false)
    }

    pub(super) fn depth_state_for_face(
        format: TextureFormat,
        d: &DrawCall,
        back_face_masks: bool,
    ) -> DepthState {
        let (stencil_front, stencil_back, read_mask, write_mask) = if d.stencil {
            (
                StencilFaceState {
                    compare: Pipeline::compare(d.stencil_func_front),
                    fail_op: Pipeline::stencil_op(d.stencil_fail_front),
                    depth_fail_op: Pipeline::stencil_op(d.stencil_zfail_front),
                    pass_op: Pipeline::stencil_op(d.stencil_zpass_front),
                },
                StencilFaceState {
                    compare: Pipeline::compare(d.stencil_func_back),
                    fail_op: Pipeline::stencil_op(d.stencil_fail_back),
                    depth_fail_op: Pipeline::stencil_op(d.stencil_zfail_back),
                    pass_op: Pipeline::stencil_op(d.stencil_zpass_back),
                },
                if back_face_masks {
                    d.stencil_read_mask_back
                } else {
                    d.stencil_read_mask_front
                } & 0xff,
                if back_face_masks {
                    d.stencil_write_mask_back
                } else {
                    d.stencil_write_mask_front
                } & 0xff,
            )
        } else {
            (
                StencilFaceState::DISABLED,
                StencilFaceState::DISABLED,
                0xff,
                0xff,
            )
        };
        DepthState {
            format,
            // A stencil-only draw (no GL_DEPTH_TEST) leaves depth neutral: never writes depth, always passes.
            depth_write: d.depth && d.depth_write,
            depth_compare: if d.depth {
                Pipeline::compare(d.depth_func)
            } else {
                hl_gpu::protocol::model::enums::compare::ALWAYS
            },
            stencil_front,
            stencil_back,
            stencil_read_mask: read_mask,
            stencil_write_mask: write_mask,
            bias_constant: 0,
            bias_slope_scale: 0.0,
            bias_clamp: 0.0,
        }
    }

    /// GL stencil-operation enum (`glStencilOp*`) → the neutral protocol stencil-op wire code the executor
    /// decodes (`hl_gpu`'s `enums::stencil_op`, Vulkan `VkStencilOp` ordering). An unmodeled op maps to `KEEP`.
    pub(super) fn stencil_op(op: u32) -> u32 {
        use hl_gpu::protocol::model::enums::stencil_op as so;
        match op {
            GL_KEEP => so::KEEP,
            GL_ZERO => so::ZERO,
            GL_REPLACE => so::REPLACE,
            GL_INCR => so::INCREMENT_CLAMP,
            GL_DECR => so::DECREMENT_CLAMP,
            GL_INVERT => so::INVERT,
            GL_INCR_WRAP => so::INCREMENT_WRAP,
            GL_DECR_WRAP => so::DECREMENT_WRAP,
            _ => so::KEEP,
        }
    }

    /// GL cull-face enum → pipeline cull mode (`0` none, `1` front, `2` back). `GL_FRONT_AND_BACK` has no
    /// single-face WebGPU equivalent; a draw that requests it discards every triangle before it is lowered
    /// at all (see [`DrawCall::discards_every_primitive`]), so it never reaches this mapping.
    pub(super) fn cull(face: u32) -> u32 {
        match face {
            GL_FRONT => 1,
            _ => 2, // GL_BACK / GL_FRONT_AND_BACK.
        }
    }

    /// GL front-face winding enum → pipeline front-face (`0` CCW, `1` CW).
    pub(super) fn front_face(mode: u32) -> u32 {
        if mode == GL_CW {
            1
        } else {
            0
        }
    }

    /// Vertex-attribute format from a GLSL declaration type string (`gl_shim.c` `decl_format_wire`).
    pub(super) fn decl_format(t: &str) -> u32 {
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
}
