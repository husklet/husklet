//! The decode side: descriptor/encoder/command decoders + [`decode_stream`], the capability handshake
//! decoder, and the kernel-descriptor `from_words`. Ported byte-identically from `hl-gpu::ir` / `backend`.
//!
//! Shader payloads are classified by the **neutral** magics in [`crate::protocol::model::kernel`]
//! ([`SPIRV_MAGIC`] / [`KERNEL_MAGIC`]) — the decoder never reaches into a CUDA/PTX constant. That is the
//! seam that breaks the old ptx leak.

use super::wire::Decoder;
use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::{
    etag, tag, Cmd, CommandBuffer, Enc, ShaderPayloadKind,
};
use crate::protocol::model::descriptor::*;
use crate::protocol::model::enums::*;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::kernel::{
    GlslDescriptor, KernelDescriptor, GLSL_MAGIC, KERNEL_MAGIC, SPIRV_MAGIC,
};

// ---------------------------------------------------------------------------------------------------
// descriptors
// ---------------------------------------------------------------------------------------------------

fn dec_buffer_desc(d: &mut Decoder) -> Result<BufferDesc> {
    Ok(BufferDesc {
        size: d.u64()?,
        usage: d.u32()?,
        label: d.str()?,
    })
}

fn dec_texture_desc(d: &mut Decoder) -> Result<TextureDesc> {
    Ok(TextureDesc {
        width: d.u32()?,
        height: d.u32()?,
        depth: d.u32()?,
        mip_levels: d.u32()?,
        sample_count: d.u32()?,
        dim: TextureDim::from_u32(d.u32()?)?,
        format: TextureFormat::from_u32(d.u32()?)?,
        usage: d.u32()?,
        label: d.str()?,
    })
}

fn dec_sampler_desc(d: &mut Decoder) -> Result<SamplerDesc> {
    Ok(SamplerDesc {
        min_filter: Filter::from_u32(d.u32()?)?,
        mag_filter: Filter::from_u32(d.u32()?)?,
        mip_filter: Filter::from_u32(d.u32()?)?,
        address_u: AddressMode::from_u32(d.u32()?)?,
        address_v: AddressMode::from_u32(d.u32()?)?,
        address_w: AddressMode::from_u32(d.u32()?)?,
    })
}

fn dec_shader_ref(d: &mut Decoder) -> Result<ShaderRef> {
    Ok(ShaderRef {
        module: d.u32()?,
        entry: d.str()?,
    })
}

fn dec_vertex_layout(d: &mut Decoder) -> Result<VertexLayout> {
    let stride = d.u32()?;
    let step_mode = d.u32()?;
    let n = d.u32()? as usize;
    // each VertexAttr = 3×u32 = 12 bytes
    let mut attrs = Vec::with_capacity(d.cap_count(n, 12));
    for _ in 0..n {
        attrs.push(VertexAttr {
            location: d.u32()?,
            format: d.u32()?,
            offset: d.u32()?,
        });
    }
    Ok(VertexLayout { stride, step_mode, attrs })
}

fn dec_color_target(d: &mut Decoder) -> Result<ColorTargetState> {
    let format = TextureFormat::from_u32(d.u32()?)?;
    let blend = if d.bool()? {
        Some(BlendState {
            src_color: d.u32()?,
            dst_color: d.u32()?,
            op_color: d.u32()?,
            src_alpha: d.u32()?,
            dst_alpha: d.u32()?,
            op_alpha: d.u32()?,
        })
    } else {
        None
    };
    Ok(ColorTargetState {
        format,
        blend,
        write_mask: d.u32()?,
    })
}

fn dec_stencil_face(d: &mut Decoder) -> Result<StencilFaceState> {
    Ok(StencilFaceState {
        compare: d.u32()?,
        fail_op: d.u32()?,
        depth_fail_op: d.u32()?,
        pass_op: d.u32()?,
    })
}

fn dec_render_pipeline(d: &mut Decoder) -> Result<RenderPipelineDesc> {
    let vertex = dec_shader_ref(d)?;
    let fragment = if d.bool()? { Some(dec_shader_ref(d)?) } else { None };
    let nvb = d.u32()? as usize;
    // each VertexLayout is at least stride+step_mode+count = 12 bytes
    let mut vertex_buffers = Vec::with_capacity(d.cap_count(nvb, 12));
    for _ in 0..nvb {
        vertex_buffers.push(dec_vertex_layout(d)?);
    }
    let nct = d.u32()? as usize;
    // each ColorTargetState is at least format+blend_flag+write_mask = 9 bytes
    let mut color_targets = Vec::with_capacity(d.cap_count(nct, 9));
    for _ in 0..nct {
        color_targets.push(dec_color_target(d)?);
    }
    let depth = if d.bool()? {
        Some(DepthState {
            format: TextureFormat::from_u32(d.u32()?)?,
            depth_write: d.bool()?,
            depth_compare: d.u32()?,
            // v7: stencil front/back faces + read/write masks, appended after the depth fields.
            stencil_front: dec_stencil_face(d)?,
            stencil_back: dec_stencil_face(d)?,
            stencil_read_mask: d.u32()?,
            stencil_write_mask: d.u32()?,
        })
    } else {
        None
    };
    Ok(RenderPipelineDesc {
        vertex,
        fragment,
        vertex_buffers,
        color_targets,
        depth,
        topology: Topology::from_u32(d.u32()?)?,
        cull: d.u32()?,
        front_face: d.u32()?,
        // v8: MSAA sample count, appended after front_face (see `enc_render_pipeline`).
        sample_count: d.u32()?,
        label: d.str()?,
    })
}

fn dec_bind_group(d: &mut Decoder) -> Result<BindGroupDesc> {
    let set = d.u32()?;
    let n = d.u32()? as usize;
    // each BindEntry is at least binding+tag+id = 9 bytes
    let mut entries = Vec::with_capacity(d.cap_count(n, 9));
    for _ in 0..n {
        let binding = d.u32()?;
        let resource = match d.u8()? {
            0 => BindResource::Buffer {
                id: d.u32()?,
                offset: d.u64()?,
                size: d.u64()?,
            },
            1 => BindResource::Texture { id: d.u32()? },
            2 => BindResource::Sampler { id: d.u32()? },
            t => return Err(GpuError::BadEnum { what: "BindResource", val: t as u32 }),
        };
        entries.push(BindEntry { binding, resource });
    }
    Ok(BindGroupDesc { set, entries })
}

// ---------------------------------------------------------------------------------------------------
// encoder ops
// ---------------------------------------------------------------------------------------------------

fn dec_subresource(d: &mut Decoder) -> Result<TextureSubresource> {
    Ok(TextureSubresource {
        mip: d.u32()?,
        layer: d.u32()?,
        aspect: TextureAspect::from_u32(d.u32()?)?,
    })
}

fn dec_origin(d: &mut Decoder) -> Result<Origin3d> {
    Ok(Origin3d { x: d.u32()?, y: d.u32()?, z: d.u32()? })
}

fn dec_extent(d: &mut Decoder) -> Result<Extent3d> {
    Ok(Extent3d { width: d.u32()?, height: d.u32()?, depth: d.u32()? })
}

fn dec_enc(d: &mut Decoder) -> Result<Enc> {
    Ok(match d.u8()? {
        etag::BEGIN_RENDER_PASS => {
            let n = d.u32()? as usize;
            // each ColorAttachment = texture+load+clear[4]+store = 25 bytes
            let mut color = Vec::with_capacity(d.cap_count(n, 25));
            for _ in 0..n {
                let texture = d.u32()?;
                let load = LoadOp::from_u32(d.u32()?)?;
                let clear = [
                    d.f32_finite("color attachment clear r")?,
                    d.f32_finite("color attachment clear g")?,
                    d.f32_finite("color attachment clear b")?,
                    d.f32_finite("color attachment clear a")?,
                ];
                let store = d.bool()?;
                color.push(ColorAttachment { texture, load, clear, store });
            }
            let depth = if d.bool()? {
                Some(DepthAttachment {
                    texture: d.u32()?,
                    load: LoadOp::from_u32(d.u32()?)?,
                    clear_depth: d.f32_finite("depth attachment clear")?,
                    clear_stencil: d.u32()?, // v7
                })
            } else {
                None
            };
            Enc::BeginRenderPass { color, depth }
        }
        etag::END_RENDER_PASS => Enc::EndRenderPass,
        etag::SET_PIPELINE => Enc::SetPipeline(d.u32()?),
        etag::SET_BIND_GROUP => Enc::SetBindGroup { index: d.u32()?, group: d.u32()? },
        etag::SET_VERTEX_BUFFER => Enc::SetVertexBuffer {
            slot: d.u32()?,
            buffer: d.u32()?,
            offset: d.u64()?,
        },
        etag::SET_INDEX_BUFFER => Enc::SetIndexBuffer {
            buffer: d.u32()?,
            offset: d.u64()?,
            format: IndexFormat::from_u32(d.u32()?)?,
        },
        etag::SET_VIEWPORT => Enc::SetViewport {
            x: d.f32_finite("viewport x")?,
            y: d.f32_finite("viewport y")?,
            w: d.f32_finite("viewport w")?,
            h: d.f32_finite("viewport h")?,
            min_depth: d.f32_finite("viewport min_depth")?,
            max_depth: d.f32_finite("viewport max_depth")?,
        },
        etag::SET_SCISSOR => Enc::SetScissor {
            x: d.u32()?,
            y: d.u32()?,
            w: d.u32()?,
            h: d.u32()?,
        },
        etag::CLEAR_RECT => Enc::ClearRect {
            texture: d.u32()?,
            x: d.u32()?,
            y: d.u32()?,
            w: d.u32()?,
            h: d.u32()?,
            color: [
                d.f32_finite("clear-rect r")?,
                d.f32_finite("clear-rect g")?,
                d.f32_finite("clear-rect b")?,
                d.f32_finite("clear-rect a")?,
            ],
        },
        etag::DRAW => Enc::Draw {
            vertex_count: d.u32()?,
            instance_count: d.u32()?,
            first_vertex: d.u32()?,
            first_instance: d.u32()?,
        },
        etag::DRAW_INDEXED => Enc::DrawIndexed {
            index_count: d.u32()?,
            instance_count: d.u32()?,
            first_index: d.u32()?,
            base_vertex: d.i32()?,
            first_instance: d.u32()?,
        },
        etag::BEGIN_COMPUTE_PASS => Enc::BeginComputePass,
        etag::END_COMPUTE_PASS => Enc::EndComputePass,
        etag::DISPATCH => Enc::Dispatch {
            x: d.u32()?,
            y: d.u32()?,
            z: d.u32()?,
        },
        etag::COPY_B2B => Enc::CopyBufferToBuffer {
            src: d.u32()?,
            src_offset: d.u64()?,
            dst: d.u32()?,
            dst_offset: d.u64()?,
            size: d.u64()?,
        },
        etag::COPY_B2T => Enc::CopyBufferToTexture {
            src: d.u32()?,
            src_offset: d.u64()?,
            bytes_per_row: d.u32()?,
            dst: d.u32()?,
            mip: d.u32()?,
            width: d.u32()?,
            height: d.u32()?,
        },
        etag::COPY_T2B => Enc::CopyTextureToBuffer {
            src: d.u32()?,
            mip: d.u32()?,
            width: d.u32()?,
            height: d.u32()?,
            dst: d.u32()?,
            dst_offset: d.u64()?,
            bytes_per_row: d.u32()?,
        },
        etag::COPY_T2T => Enc::CopyTextureToTexture {
            src: d.u32()?,
            src_sub: dec_subresource(d)?,
            src_origin: dec_origin(d)?,
            dst: d.u32()?,
            dst_sub: dec_subresource(d)?,
            dst_origin: dec_origin(d)?,
            extent: dec_extent(d)?,
        },
        etag::BLIT_TEXTURE => Enc::BlitTexture {
            src: d.u32()?,
            src_sub: dec_subresource(d)?,
            src_origin: dec_origin(d)?,
            src_extent: dec_extent(d)?,
            dst: d.u32()?,
            dst_sub: dec_subresource(d)?,
            dst_origin: dec_origin(d)?,
            dst_extent: dec_extent(d)?,
            filter: Filter::from_u32(d.u32()?)?,
        },
        etag::RESOLVE_TEXTURE => Enc::ResolveTexture {
            src: d.u32()?,
            src_sub: dec_subresource(d)?,
            src_origin: dec_origin(d)?,
            dst: d.u32()?,
            dst_sub: dec_subresource(d)?,
            dst_origin: dec_origin(d)?,
            extent: dec_extent(d)?,
        },
        etag::FILL_BUFFER => Enc::FillBuffer {
            buffer: d.u32()?,
            offset: d.u64()?,
            size: d.u64()?,
            value: d.u32()?,
        },
        etag::SET_STENCIL_REFERENCE => Enc::SetStencilReference { reference: d.u32()? },
        t => return Err(GpuError::BadTag(t as u32)),
    })
}

fn dec_command_buffer(d: &mut Decoder) -> Result<CommandBuffer> {
    let n = d.u32()? as usize;
    // each encoder op is at least a 1-byte tag
    let mut encoder = Vec::with_capacity(d.cap_count(n, 1));
    for i in 0..n {
        let pos = d.pos();
        let tag = d.peek_u8();
        match dec_enc(d) {
            Ok(op) => encoder.push(op),
            Err(e) => {
                return Err(GpuError::Decode(format!(
                    "submit encoder op {i}/{n} at byte {pos}/{} tag {:?} remaining {}: {e}",
                    d.len(),
                    tag,
                    d.remaining()
                )));
            }
        }
    }
    let signal = if d.bool()? { Some((d.u32()?, d.u64()?)) } else { None };
    Ok(CommandBuffer { encoder, signal })
}

// ---------------------------------------------------------------------------------------------------
// top-level Cmd
// ---------------------------------------------------------------------------------------------------

impl Cmd {
    /// Decode one command (tag + body) from `d`.
    pub fn decode(d: &mut Decoder) -> Result<Cmd> {
        Ok(match d.u8()? {
            tag::CREATE_BUFFER => Cmd::CreateBuffer(d.u32()?, dec_buffer_desc(d)?),
            tag::DESTROY_BUFFER => Cmd::DestroyBuffer(d.u32()?),
            tag::WRITE_BUFFER => Cmd::WriteBuffer {
                id: d.u32()?,
                offset: d.u64()?,
                data: d.bytes()?.to_vec(),
            },
            tag::CREATE_TEXTURE => Cmd::CreateTexture(d.u32()?, dec_texture_desc(d)?),
            tag::DESTROY_TEXTURE => Cmd::DestroyTexture(d.u32()?),
            tag::CREATE_SAMPLER => Cmd::CreateSampler(d.u32()?, dec_sampler_desc(d)?),
            tag::DESTROY_SAMPLER => Cmd::DestroySampler(d.u32()?),
            tag::CREATE_SHADER => {
                // WIRE COMPAT (see `encode`): the pinned guest engine speaks the CreateShader layout with
                // no ShaderPayloadKind byte, so we must NOT consume one here. Instead we recover the kind
                // losslessly by inspecting the payload's leading word — each real kind is self-identifying,
                // classified against the NEUTRAL magics in `model::kernel` (never a CUDA/PTX constant):
                //   * SPIRV_MAGIC (0x07230203)  → SpirV     (translated Vulkan modules)
                //   * KERNEL_MAGIC (0xDD6B0001) → PtxKernel (neutral kernel descriptor)
                //   * GLSL_MAGIC  (0xDD670001)  → Glsl      (forwarded GLSL descriptor, WIRE_VERSION 6)
                //   * anything else             → LegacyMsl (already-translated MSL words)
                // MSL text words never collide with these magics (both decode to non-ASCII byte runs), so
                // the inference is unambiguous for every payload that actually crosses the ring.
                let id = d.u32()?;
                let spirv = d.words()?;
                let kind = match spirv.first().copied() {
                    Some(SPIRV_MAGIC) => ShaderPayloadKind::SpirV,
                    Some(w) if w == KERNEL_MAGIC => ShaderPayloadKind::PtxKernel,
                    Some(w) if w == GLSL_MAGIC => ShaderPayloadKind::Glsl,
                    _ => ShaderPayloadKind::LegacyMsl,
                };
                Cmd::CreateShader { id, kind, spirv }
            }
            tag::DESTROY_SHADER => Cmd::DestroyShader(d.u32()?),
            tag::CREATE_RENDER_PIPELINE => Cmd::CreateRenderPipeline(d.u32()?, dec_render_pipeline(d)?),
            tag::CREATE_COMPUTE_PIPELINE => {
                let id = d.u32()?;
                let compute = dec_shader_ref(d)?;
                let label = d.str()?;
                Cmd::CreateComputePipeline(id, ComputePipelineDesc { compute, label })
            }
            tag::DESTROY_PIPELINE => Cmd::DestroyPipeline(d.u32()?),
            tag::CREATE_BIND_GROUP => Cmd::CreateBindGroup(d.u32()?, dec_bind_group(d)?),
            tag::DESTROY_BIND_GROUP => Cmd::DestroyBindGroup(d.u32()?),
            tag::CREATE_SURFACE => {
                let id = d.u32()?;
                Cmd::CreateSurface(
                    id,
                    SurfaceDesc {
                        width: d.u32()?,
                        height: d.u32()?,
                        format: TextureFormat::from_u32(d.u32()?)?,
                        hlp_surface: d.u32()?,
                    },
                )
            }
            tag::DESTROY_SURFACE => Cmd::DestroySurface(d.u32()?),
            tag::CREATE_FENCE => Cmd::CreateFence(d.u32()?),
            tag::DESTROY_FENCE => Cmd::DestroyFence(d.u32()?),
            tag::SUBMIT => Cmd::Submit(dec_command_buffer(d)?),
            tag::WAIT_FENCE => Cmd::WaitFence {
                id: d.u32()?,
                value: d.u64()?,
            },
            tag::PRESENT => Cmd::Present {
                surface: d.u32()?,
                texture: d.u32()?,
            },
            t => return Err(GpuError::BadTag(t as u32)),
        })
    }

    /// Decode one length-prefixed command frame (as produced by [`Cmd::frame`]). Rejects a frame body
    /// that carries extra bytes after the command — a malformed frame is a decode error, not an accepted
    /// command with a silently-discarded tail.
    pub fn decode_frame(d: &mut Decoder) -> Result<Cmd> {
        d.frame(Cmd::decode)
    }
}

/// Decode a whole command stream produced by [`encode_stream`](super::encode::encode_stream) until the
/// input is exhausted.
pub fn decode_stream(bytes: &[u8]) -> Result<Vec<Cmd>> {
    let _span = hl_log::hl_span!(hl_log::tag::WIRE, "decode");
    let mut d = Decoder::new(bytes);
    let mut out = Vec::new();
    while !d.is_empty() {
        let idx = out.len();
        let pos = d.pos();
        let tag = d.peek_u8();
        match Cmd::decode(&mut d) {
            Ok(cmd) => out.push(cmd),
            Err(e) => {
                hl_log::hl_warn!(hl_log::tag::WIRE, "decode err cmd={} byte={}/{} tag={:?}", idx, pos, d.len(), tag);
                return Err(GpuError::Decode(format!(
                    "command {idx} at byte {pos}/{} tag {:?} remaining {}: {e}",
                    d.len(),
                    tag,
                    d.remaining()
                )));
            }
        }
    }
    hl_log::hl_add!(hl_log::tag::WIRE, "cmds_decoded", out.len() as u64);
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------
// capability handshake
// ---------------------------------------------------------------------------------------------------

impl Capabilities {
    /// Decode a handshake descriptor produced by [`Capabilities::encode`](super::encode).
    pub fn decode(d: &mut Decoder) -> Result<Capabilities> {
        let wire_version = d.u32()?;
        let name = d.str()?;
        let unified_memory = d.bool()?;
        let supports_compute = d.bool()?;
        let supports_graphics = d.bool()?;
        let supports_timeline_fences = d.bool()?;
        let max_texture_2d = d.u32()?;
        let max_bind_groups = d.u32()?;
        let max_frame_bytes = d.u64()?;
        let max_buffer_bytes = d.u64()?;
        let command_bits = d.u64()?;
        let shader_payloads = d.u32()?;
        let texture_formats = d.u32()?;
        let pbits = d.u32()?;
        Ok(Capabilities {
            name,
            unified_memory,
            supports_compute,
            supports_graphics,
            max_texture_2d,
            present_kinds: Capabilities::present_kinds_from_bits(pbits),
            wire_version,
            command_bits,
            shader_payloads,
            texture_formats,
            max_frame_bytes,
            max_buffer_bytes,
            max_bind_groups,
            supports_timeline_fences,
        })
    }

    /// Decode a handshake frame (u32 length + body) written by
    /// [`Capabilities::to_handshake`](super::encode).
    pub fn from_handshake(bytes: &[u8]) -> Result<Capabilities> {
        let mut d = Decoder::new(bytes);
        d.frame(Capabilities::decode)
    }
}

// ---------------------------------------------------------------------------------------------------
// kernel descriptor ← CreateShader words
// ---------------------------------------------------------------------------------------------------

impl KernelDescriptor {
    /// Decode from shader words. Returns `None` if the words are not a kernel descriptor (i.e. SPIR-V).
    pub fn from_words(words: &[u32]) -> Option<Result<Self>> {
        if words.len() < 2 || words[0] != KERNEL_MAGIC {
            return None;
        }
        let byte_len = words[1] as usize;
        let mut bytes = Vec::with_capacity((words.len() - 2) * 4);
        for &w in &words[2..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() < byte_len {
            return Some(Err(GpuError::Kernel("kernel descriptor truncated".into())));
        }
        bytes.truncate(byte_len);
        let mut d = Decoder::new(&bytes);
        Some((|| {
            let ptx = d.str()?;
            let entry = d.str()?;
            let block = [d.u32()?, d.u32()?, d.u32()?];
            Ok(KernelDescriptor { ptx, entry, block })
        })())
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL descriptor ← CreateShader words
// ---------------------------------------------------------------------------------------------------

impl GlslDescriptor {
    /// Decode from shader words. Returns `None` if the words are not a GLSL descriptor (leading word is not
    /// [`GLSL_MAGIC`]) — the mirror of [`KernelDescriptor::from_words`].
    pub fn from_words(words: &[u32]) -> Option<Result<Self>> {
        if words.len() < 2 || words[0] != GLSL_MAGIC {
            return None;
        }
        let byte_len = words[1] as usize;
        let mut bytes = Vec::with_capacity((words.len() - 2) * 4);
        for &w in &words[2..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() < byte_len {
            return Some(Err(GpuError::Kernel("glsl descriptor truncated".into())));
        }
        bytes.truncate(byte_len);
        let mut d = Decoder::new(&bytes);
        Some((|| {
            let stage = d.u32()?;
            let entry = d.str()?;
            let source = d.str()?;
            Ok(GlslDescriptor { stage, entry, source })
        })())
    }
}
