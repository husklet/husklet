//! The decode side: descriptor/encoder/command decoders + [`decode_stream`], the capability handshake
//! decoder, and the kernel-descriptor `from_words`. Ported byte-identically from `hl-gpu::ir` / `backend`.
//!
//! Shader payloads are classified by the **neutral** magics in [`crate::protocol::model::kernel`]
//! ([`SPIRV_MAGIC`] / [`KERNEL_MAGIC`]) — the decoder never reaches into a CUDA/PTX constant. That is the
//! seam that breaks the old ptx leak.

use super::wire::Decoder;
use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::{etag, tag, Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use crate::protocol::model::descriptor::*;
use crate::protocol::model::enums::*;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::kernel::{
    GlslDescriptor, KernelDescriptor, GLSL_MAGIC, KERNEL_MAGIC, SPIRV_MAGIC,
};

// ---------------------------------------------------------------------------------------------------
// descriptors
// ---------------------------------------------------------------------------------------------------

impl<'a> Decoder<'a> {
    fn buffer_desc(&mut self) -> Result<BufferDesc> {
        let d = self;
        Ok(BufferDesc {
            size: d.u64()?,
            usage: d.u32()?,
            label: d.str()?,
        })
    }

    fn texture_desc(&mut self) -> Result<TextureDesc> {
        let d = self;
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

    fn sampler_desc(&mut self) -> Result<SamplerDesc> {
        let d = self;
        Ok(SamplerDesc {
            min_filter: Filter::from_u32(d.u32()?)?,
            mag_filter: Filter::from_u32(d.u32()?)?,
            mip_filter: Filter::from_u32(d.u32()?)?,
            address_u: AddressMode::from_u32(d.u32()?)?,
            address_v: AddressMode::from_u32(d.u32()?)?,
            address_w: AddressMode::from_u32(d.u32()?)?,
            border_color: BorderColor::from_u32(d.u32()?)?,
            lod_min_clamp: d.f32_finite("sampler lod_min_clamp")?,
            lod_max_clamp: d.f32_finite("sampler lod_max_clamp")?,
            compare: if d.bool()? { Some(d.u32()?) } else { None },
        })
    }

    fn shader_ref(&mut self) -> Result<ShaderRef> {
        let d = self;
        Ok(ShaderRef {
            module: d.u32()?,
            entry: d.str()?,
        })
    }

    fn vertex_layout(&mut self) -> Result<VertexLayout> {
        let d = self;
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
        Ok(VertexLayout {
            stride,
            step_mode,
            attrs,
        })
    }

    fn color_target(&mut self) -> Result<ColorTargetState> {
        let d = self;
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

    fn stencil_face(&mut self) -> Result<StencilFaceState> {
        let d = self;
        Ok(StencilFaceState {
            compare: d.u32()?,
            fail_op: d.u32()?,
            depth_fail_op: d.u32()?,
            pass_op: d.u32()?,
        })
    }

    fn render_pipeline(&mut self) -> Result<RenderPipelineDesc> {
        let d = self;
        let vertex = d.shader_ref()?;
        let fragment = if d.bool()? {
            Some(d.shader_ref()?)
        } else {
            None
        };
        let nvb = d.u32()? as usize;
        // each VertexLayout is at least stride+step_mode+count = 12 bytes
        let mut vertex_buffers = Vec::with_capacity(d.cap_count(nvb, 12));
        for _ in 0..nvb {
            vertex_buffers.push(d.vertex_layout()?);
        }
        let nct = d.u32()? as usize;
        // each ColorTargetState is at least format+blend_flag+write_mask = 9 bytes
        let mut color_targets = Vec::with_capacity(d.cap_count(nct, 9));
        for _ in 0..nct {
            color_targets.push(d.color_target()?);
        }
        let depth = if d.bool()? {
            Some(DepthState {
                format: TextureFormat::from_u32(d.u32()?)?,
                depth_write: d.bool()?,
                depth_compare: d.u32()?,
                // v7: stencil front/back faces + read/write masks, appended after the depth fields.
                stencil_front: d.stencil_face()?,
                stencil_back: d.stencil_face()?,
                stencil_read_mask: d.u32()?,
                stencil_write_mask: d.u32()?,
                bias_constant: d.i32()?,
                bias_slope_scale: d.f32_finite("depth bias slope scale")?,
                bias_clamp: d.f32_finite("depth bias clamp")?,
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

    fn bind_group(&mut self) -> Result<BindGroupDesc> {
        let d = self;
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
                3 => {
                    let count = d.u32()? as usize;
                    if count == 0 {
                        return Err(GpuError::Invalid("empty buffer binding array"));
                    }
                    let mut elements = Vec::with_capacity(d.cap_count(count, 20));
                    for _ in 0..count {
                        elements.push(BufferBinding {
                            id: d.u32()?,
                            offset: d.u64()?,
                            size: d.u64()?,
                        });
                    }
                    BindResource::BufferArray { elements }
                }
                4 => {
                    let count = d.u32()? as usize;
                    if count == 0 {
                        return Err(GpuError::Invalid("empty binding array"));
                    }
                    let mut ids = Vec::with_capacity(d.cap_count(count, 4));
                    for _ in 0..count {
                        ids.push(d.u32()?);
                    }
                    BindResource::TextureArray { ids }
                }
                5 => {
                    let count = d.u32()? as usize;
                    if count == 0 {
                        return Err(GpuError::Invalid("empty binding array"));
                    }
                    let mut ids = Vec::with_capacity(d.cap_count(count, 4));
                    for _ in 0..count {
                        ids.push(d.u32()?);
                    }
                    BindResource::SamplerArray { ids }
                }
                6 => BindResource::TexelBuffer {
                    id: d.u32()?,
                    offset: d.u64()?,
                    size: d.u64()?,
                    format: TextureFormat::from_u32(d.u32()?)?,
                    writable: d.bool()?,
                },
                t => {
                    return Err(GpuError::BadEnum {
                        what: "BindResource",
                        val: t as u32,
                    });
                }
            };
            entries.push(BindEntry { binding, resource });
        }
        Ok(BindGroupDesc { set, entries })
    }

    // ---------------------------------------------------------------------------------------------------
    // encoder ops
    // ---------------------------------------------------------------------------------------------------

    fn subresource(&mut self) -> Result<TextureSubresource> {
        let d = self;
        Ok(TextureSubresource {
            mip: d.u32()?,
            layer: d.u32()?,
            aspect: TextureAspect::from_u32(d.u32()?)?,
        })
    }

    fn origin(&mut self) -> Result<Origin3d> {
        let d = self;
        Ok(Origin3d {
            x: d.u32()?,
            y: d.u32()?,
            z: d.u32()?,
        })
    }

    fn extent(&mut self) -> Result<Extent3d> {
        let d = self;
        Ok(Extent3d {
            width: d.u32()?,
            height: d.u32()?,
            depth: d.u32()?,
        })
    }

    fn enc(&mut self) -> Result<Enc> {
        let d = self;
        Ok(match d.u8()? {
            etag::BEGIN_RENDER_PASS => {
                let n = d.u32()? as usize;
                // each ColorAttachment = texture+load+clear[4]+store = 41 bytes
                let mut color = Vec::with_capacity(d.cap_count(n, 41));
                for _ in 0..n {
                    let texture = d.u32()?;
                    let load = LoadOp::from_u32(d.u32()?)?;
                    let clear = [
                        d.f64_finite("color attachment clear r")?,
                        d.f64_finite("color attachment clear g")?,
                        d.f64_finite("color attachment clear b")?,
                        d.f64_finite("color attachment clear a")?,
                    ];
                    let store = d.bool()?;
                    color.push(ColorAttachment {
                        texture,
                        load,
                        clear,
                        store,
                    });
                }
                let depth = if d.bool()? {
                    Some(DepthAttachment {
                        texture: d.u32()?,
                        depth_load: LoadOp::from_u32(d.u32()?)?,
                        stencil_load: LoadOp::from_u32(d.u32()?)?,
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
            etag::SET_BIND_GROUP => Enc::SetBindGroup {
                index: d.u32()?,
                group: d.u32()?,
            },
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
                    d.f64_finite("clear-rect r")?,
                    d.f64_finite("clear-rect g")?,
                    d.f64_finite("clear-rect b")?,
                    d.f64_finite("clear-rect a")?,
                ],
                base_array_layer: d.u32()?,
                layer_count: d.u32()?,
                mip_level: d.u32()?,
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
            etag::COPY_B2T_REGION => Enc::CopyBufferToTextureRegion {
                src: d.u32()?,
                src_offset: d.u64()?,
                bytes_per_row: d.u32()?,
                rows_per_image: d.u32()?,
                dst: d.u32()?,
                dst_sub: d.subresource()?,
                dst_origin: d.origin()?,
                extent: d.extent()?,
            },
            etag::COPY_T2B_REGION => Enc::CopyTextureToBufferRegion {
                src: d.u32()?,
                src_sub: d.subresource()?,
                src_origin: d.origin()?,
                extent: d.extent()?,
                dst: d.u32()?,
                dst_offset: d.u64()?,
                bytes_per_row: d.u32()?,
                rows_per_image: d.u32()?,
            },
            etag::COPY_T2T => Enc::CopyTextureToTexture {
                src: d.u32()?,
                src_sub: d.subresource()?,
                src_origin: d.origin()?,
                dst: d.u32()?,
                dst_sub: d.subresource()?,
                dst_origin: d.origin()?,
                extent: d.extent()?,
            },
            etag::BLIT_TEXTURE => Enc::BlitTexture {
                src: d.u32()?,
                src_sub: d.subresource()?,
                src_origin: d.origin()?,
                src_extent: d.extent()?,
                dst: d.u32()?,
                dst_sub: d.subresource()?,
                dst_origin: d.origin()?,
                dst_extent: d.extent()?,
                filter: Filter::from_u32(d.u32()?)?,
                mirror: {
                    let v = d.u32()?;
                    Mirror::from_u32(v).ok_or(GpuError::BadEnum {
                        what: "blit mirror",
                        val: v,
                    })?
                },
            },
            etag::RESOLVE_TEXTURE => Enc::ResolveTexture {
                src: d.u32()?,
                src_sub: d.subresource()?,
                src_origin: d.origin()?,
                dst: d.u32()?,
                dst_sub: d.subresource()?,
                dst_origin: d.origin()?,
                extent: d.extent()?,
            },
            etag::FILL_BUFFER => Enc::FillBuffer {
                buffer: d.u32()?,
                offset: d.u64()?,
                size: d.u64()?,
                value: d.u32()?,
            },
            etag::SET_STENCIL_REFERENCE => Enc::SetStencilReference {
                reference: d.u32()?,
            },
            etag::SET_BLEND_CONSTANT => Enc::SetBlendConstant {
                color: [
                    d.f32_finite("blend constant r")?,
                    d.f32_finite("blend constant g")?,
                    d.f32_finite("blend constant b")?,
                    d.f32_finite("blend constant a")?,
                ],
            },
            t => return Err(GpuError::BadTag(t as u32)),
        })
    }

    fn command_buffer(&mut self) -> Result<CommandBuffer> {
        let d = self;
        let n = d.u32()? as usize;
        // each encoder op is at least a 1-byte tag
        let mut encoder = Vec::with_capacity(d.cap_count(n, 1));
        for i in 0..n {
            let pos = d.pos();
            let tag = d.peek_u8();
            match d.enc() {
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
        let signal = if d.bool()? {
            Some((d.u32()?, d.u64()?))
        } else {
            None
        };
        Ok(CommandBuffer { encoder, signal })
    }
}

// ---------------------------------------------------------------------------------------------------
// top-level Cmd
// ---------------------------------------------------------------------------------------------------
mod command;
mod descriptor;
