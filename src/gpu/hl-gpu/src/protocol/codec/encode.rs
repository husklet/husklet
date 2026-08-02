//! The encode side: descriptor/encoder/command encoders + [`encode_stream`], the capability handshake
//! encoder, and the kernel-descriptor `to_words`. Ported byte-identically from `hl-gpu::ir` / `backend`.
//!
//! The encode logic is attached to the model types as inherent methods defined here, so `Cmd::encode`
//! reads naturally while [`crate::protocol::model`] stays free of serialization code.

use super::wire::Encoder;
use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::{etag, tag, Cmd, CommandBuffer, Enc};
use crate::protocol::model::descriptor::*;
use crate::protocol::model::kernel::{GlslDescriptor, KernelDescriptor, GLSL_MAGIC, KERNEL_MAGIC};

// ---------------------------------------------------------------------------------------------------
// descriptors
// ---------------------------------------------------------------------------------------------------

impl Encoder {
    fn buffer_desc(&mut self, d: &BufferDesc) {
        let e = self;
        e.u64(d.size);
        e.u32(d.usage);
        e.str(&d.label);
    }

    fn texture_desc(&mut self, t: &TextureDesc) {
        let e = self;
        e.u32(t.width);
        e.u32(t.height);
        e.u32(t.depth);
        e.u32(t.mip_levels);
        e.u32(t.sample_count);
        e.u32(t.dim.to_u32());
        e.u32(t.format.to_u32());
        e.u32(t.usage);
        e.str(&t.label);
    }

    fn sampler_desc(&mut self, s: &SamplerDesc) {
        let e = self;
        e.u32(s.min_filter.to_u32());
        e.u32(s.mag_filter.to_u32());
        e.u32(s.mip_filter.to_u32());
        e.u32(s.address_u.to_u32());
        e.u32(s.address_v.to_u32());
        e.u32(s.address_w.to_u32());
        e.f32(s.lod_min_clamp);
        e.f32(s.lod_max_clamp);
        match s.compare {
            Some(compare) => {
                e.bool(true);
                e.u32(compare);
            }
            None => e.bool(false),
        }
    }

    fn shader_ref(&mut self, s: &ShaderRef) {
        let e = self;
        e.u32(s.module);
        e.str(&s.entry);
    }

    fn vertex_layout(&mut self, v: &VertexLayout) {
        let e = self;
        e.u32(v.stride);
        e.u32(v.step_mode);
        e.u32(v.attrs.len() as u32);
        for a in &v.attrs {
            e.u32(a.location);
            e.u32(a.format);
            e.u32(a.offset);
        }
    }

    fn color_target(&mut self, c: &ColorTargetState) {
        let e = self;
        e.u32(c.format.to_u32());
        match &c.blend {
            None => e.bool(false),
            Some(b) => {
                e.bool(true);
                e.u32(b.src_color);
                e.u32(b.dst_color);
                e.u32(b.op_color);
                e.u32(b.src_alpha);
                e.u32(b.dst_alpha);
                e.u32(b.op_alpha);
            }
        }
        e.u32(c.write_mask);
    }

    fn stencil_face(&mut self, f: &StencilFaceState) {
        let e = self;
        e.u32(f.compare);
        e.u32(f.fail_op);
        e.u32(f.depth_fail_op);
        e.u32(f.pass_op);
    }

    fn render_pipeline(&mut self, p: &RenderPipelineDesc) {
        let e = self;
        e.shader_ref(&p.vertex);
        match &p.fragment {
            None => e.bool(false),
            Some(f) => {
                e.bool(true);
                e.shader_ref(f);
            }
        }
        e.u32(p.vertex_buffers.len() as u32);
        for vb in &p.vertex_buffers {
            e.vertex_layout(vb);
        }
        e.u32(p.color_targets.len() as u32);
        for c in &p.color_targets {
            e.color_target(c);
        }
        match &p.depth {
            None => e.bool(false),
            Some(dp) => {
                e.bool(true);
                e.u32(dp.format.to_u32());
                e.bool(dp.depth_write);
                e.u32(dp.depth_compare);
                // v7: stencil front/back faces + read/write masks, appended after the depth fields.
                e.stencil_face(&dp.stencil_front);
                e.stencil_face(&dp.stencil_back);
                e.u32(dp.stencil_read_mask);
                e.u32(dp.stencil_write_mask);
                e.i32(dp.bias_constant);
                e.f32(dp.bias_slope_scale);
                e.f32(dp.bias_clamp);
            }
        }
        e.u32(p.topology.to_u32());
        e.u32(p.cull);
        e.u32(p.front_face);
        // v8: MSAA sample count, appended after front_face. Neutral default 1 = single-sampled, so a stream
        // that never rasterizes multisampled is byte-identical in meaning to the pre-v8 layout.
        e.u32(p.sample_count);
        e.str(&p.label);
    }

    fn bind_group(&mut self, b: &BindGroupDesc) {
        let e = self;
        e.u32(b.set);
        e.u32(b.entries.len() as u32);
        for en in &b.entries {
            e.u32(en.binding);
            match &en.resource {
                BindResource::Buffer { id, offset, size } => {
                    e.u8(0);
                    e.u32(*id);
                    e.u64(*offset);
                    e.u64(*size);
                }
                BindResource::Texture { id } => {
                    e.u8(1);
                    e.u32(*id);
                }
                BindResource::Sampler { id } => {
                    e.u8(2);
                    e.u32(*id);
                }
                BindResource::BufferArray { elements } => {
                    e.u8(3);
                    e.u32(elements.len() as u32);
                    for element in elements {
                        e.u32(element.id);
                        e.u64(element.offset);
                        e.u64(element.size);
                    }
                }
                BindResource::TextureArray { ids } => {
                    e.u8(4);
                    e.u32(ids.len() as u32);
                    for id in ids {
                        e.u32(*id);
                    }
                }
                BindResource::SamplerArray { ids } => {
                    e.u8(5);
                    e.u32(ids.len() as u32);
                    for id in ids {
                        e.u32(*id);
                    }
                }
                BindResource::TexelBuffer {
                    id,
                    offset,
                    size,
                    format,
                    writable,
                } => {
                    e.u8(6);
                    e.u32(*id);
                    e.u64(*offset);
                    e.u64(*size);
                    e.u32(format.to_u32());
                    e.bool(*writable);
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // encoder ops
    // ---------------------------------------------------------------------------------------------------

    fn subresource(&mut self, s: &TextureSubresource) {
        let e = self;
        e.u32(s.mip);
        e.u32(s.layer);
        e.u32(s.aspect.to_u32());
    }

    fn origin(&mut self, o: &Origin3d) {
        let e = self;
        e.u32(o.x);
        e.u32(o.y);
        e.u32(o.z);
    }

    fn extent(&mut self, x: &Extent3d) {
        let e = self;
        e.u32(x.width);
        e.u32(x.height);
        e.u32(x.depth);
    }

    fn enc(&mut self, op: &Enc) {
        let e = self;
        match op {
            Enc::BeginRenderPass { color, depth } => {
                e.u8(etag::BEGIN_RENDER_PASS);
                e.u32(color.len() as u32);
                for c in color {
                    e.u32(c.texture);
                    e.u32(c.load.to_u32());
                    for v in c.clear {
                        e.f64(v);
                    }
                    e.bool(c.store);
                }
                match depth {
                    None => e.bool(false),
                    Some(dp) => {
                        e.bool(true);
                        e.u32(dp.texture);
                        e.u32(dp.load.to_u32());
                        e.f32(dp.clear_depth);
                        e.u32(dp.clear_stencil); // v7
                    }
                }
            }
            Enc::EndRenderPass => e.u8(etag::END_RENDER_PASS),
            Enc::SetPipeline(p) => {
                e.u8(etag::SET_PIPELINE);
                e.u32(*p);
            }
            Enc::SetBindGroup { index, group } => {
                e.u8(etag::SET_BIND_GROUP);
                e.u32(*index);
                e.u32(*group);
            }
            Enc::SetVertexBuffer {
                slot,
                buffer,
                offset,
            } => {
                e.u8(etag::SET_VERTEX_BUFFER);
                e.u32(*slot);
                e.u32(*buffer);
                e.u64(*offset);
            }
            Enc::SetIndexBuffer {
                buffer,
                offset,
                format,
            } => {
                e.u8(etag::SET_INDEX_BUFFER);
                e.u32(*buffer);
                e.u64(*offset);
                e.u32(format.to_u32());
            }
            Enc::SetViewport {
                x,
                y,
                w,
                h,
                min_depth,
                max_depth,
            } => {
                e.u8(etag::SET_VIEWPORT);
                for v in [*x, *y, *w, *h, *min_depth, *max_depth] {
                    e.f32(v);
                }
            }
            Enc::SetScissor { x, y, w, h } => {
                e.u8(etag::SET_SCISSOR);
                e.u32(*x);
                e.u32(*y);
                e.u32(*w);
                e.u32(*h);
            }
            Enc::ClearRect {
                texture,
                x,
                y,
                w,
                h,
                color,
                base_array_layer,
                layer_count,
                mip_level,
            } => {
                e.u8(etag::CLEAR_RECT);
                e.u32(*texture);
                e.u32(*x);
                e.u32(*y);
                e.u32(*w);
                e.u32(*h);
                for v in color {
                    e.f64(*v);
                }
                // Appended after the existing payload, so the prefix every previous field occupied is
                // byte-identical and the tag itself never moved.
                e.u32(*base_array_layer);
                e.u32(*layer_count);
                e.u32(*mip_level);
            }
            Enc::Draw {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                e.u8(etag::DRAW);
                e.u32(*vertex_count);
                e.u32(*instance_count);
                e.u32(*first_vertex);
                e.u32(*first_instance);
            }
            Enc::DrawIndexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                e.u8(etag::DRAW_INDEXED);
                e.u32(*index_count);
                e.u32(*instance_count);
                e.u32(*first_index);
                e.i32(*base_vertex);
                e.u32(*first_instance);
            }
            Enc::BeginComputePass => e.u8(etag::BEGIN_COMPUTE_PASS),
            Enc::EndComputePass => e.u8(etag::END_COMPUTE_PASS),
            Enc::Dispatch { x, y, z } => {
                e.u8(etag::DISPATCH);
                e.u32(*x);
                e.u32(*y);
                e.u32(*z);
            }
            Enc::CopyBufferToBuffer {
                src,
                src_offset,
                dst,
                dst_offset,
                size,
            } => {
                e.u8(etag::COPY_B2B);
                e.u32(*src);
                e.u64(*src_offset);
                e.u32(*dst);
                e.u64(*dst_offset);
                e.u64(*size);
            }
            Enc::CopyBufferToTexture {
                src,
                src_offset,
                bytes_per_row,
                dst,
                mip,
                width,
                height,
            } => {
                e.u8(etag::COPY_B2T);
                e.u32(*src);
                e.u64(*src_offset);
                e.u32(*bytes_per_row);
                e.u32(*dst);
                e.u32(*mip);
                e.u32(*width);
                e.u32(*height);
            }
            Enc::CopyTextureToBuffer {
                src,
                mip,
                width,
                height,
                dst,
                dst_offset,
                bytes_per_row,
            } => {
                e.u8(etag::COPY_T2B);
                e.u32(*src);
                e.u32(*mip);
                e.u32(*width);
                e.u32(*height);
                e.u32(*dst);
                e.u64(*dst_offset);
                e.u32(*bytes_per_row);
            }
            Enc::CopyBufferToTextureRegion {
                src,
                src_offset,
                bytes_per_row,
                rows_per_image,
                dst,
                dst_sub,
                dst_origin,
                extent,
            } => {
                e.u8(etag::COPY_B2T_REGION);
                e.u32(*src);
                e.u64(*src_offset);
                e.u32(*bytes_per_row);
                e.u32(*rows_per_image);
                e.u32(*dst);
                e.subresource(dst_sub);
                e.origin(dst_origin);
                e.extent(extent);
            }
            Enc::CopyTextureToBufferRegion {
                src,
                src_sub,
                src_origin,
                extent,
                dst,
                dst_offset,
                bytes_per_row,
                rows_per_image,
            } => {
                e.u8(etag::COPY_T2B_REGION);
                e.u32(*src);
                e.subresource(src_sub);
                e.origin(src_origin);
                e.extent(extent);
                e.u32(*dst);
                e.u64(*dst_offset);
                e.u32(*bytes_per_row);
                e.u32(*rows_per_image);
            }
            Enc::CopyTextureToTexture {
                src,
                src_sub,
                src_origin,
                dst,
                dst_sub,
                dst_origin,
                extent,
            } => {
                e.u8(etag::COPY_T2T);
                e.u32(*src);
                e.subresource(src_sub);
                e.origin(src_origin);
                e.u32(*dst);
                e.subresource(dst_sub);
                e.origin(dst_origin);
                e.extent(extent);
            }
            Enc::BlitTexture {
                src,
                src_sub,
                src_origin,
                src_extent,
                dst,
                dst_sub,
                dst_origin,
                dst_extent,
                filter,
                mirror,
            } => {
                e.u8(etag::BLIT_TEXTURE);
                e.u32(*src);
                e.subresource(src_sub);
                e.origin(src_origin);
                e.extent(src_extent);
                e.u32(*dst);
                e.subresource(dst_sub);
                e.origin(dst_origin);
                e.extent(dst_extent);
                e.u32(filter.to_u32());
                e.u32(mirror.to_u32());
            }
            Enc::ResolveTexture {
                src,
                src_sub,
                src_origin,
                dst,
                dst_sub,
                dst_origin,
                extent,
            } => {
                e.u8(etag::RESOLVE_TEXTURE);
                e.u32(*src);
                e.subresource(src_sub);
                e.origin(src_origin);
                e.u32(*dst);
                e.subresource(dst_sub);
                e.origin(dst_origin);
                e.extent(extent);
            }
            Enc::FillBuffer {
                buffer,
                offset,
                size,
                value,
            } => {
                e.u8(etag::FILL_BUFFER);
                e.u32(*buffer);
                e.u64(*offset);
                e.u64(*size);
                e.u32(*value);
            }
            Enc::SetStencilReference { reference } => {
                e.u8(etag::SET_STENCIL_REFERENCE);
                e.u32(*reference);
            }
            Enc::SetBlendConstant { color } => {
                e.u8(etag::SET_BLEND_CONSTANT);
                for value in color {
                    e.f32(*value);
                }
            }
        }
    }

    fn command_buffer(&mut self, cb: &CommandBuffer) {
        let e = self;
        e.u32(cb.encoder.len() as u32);
        for op in &cb.encoder {
            e.enc(op);
        }
        match cb.signal {
            None => e.bool(false),
            Some((f, v)) => {
                e.bool(true);
                e.u32(f);
                e.u64(v);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// top-level Cmd
// ---------------------------------------------------------------------------------------------------

impl Encoder {
    /// Encode a whole command stream (tag+body, back to back, no per-message length prefix).
    pub fn stream(commands: &[Cmd]) -> Vec<u8> {
        let mut encoder = Self::new();
        for command in commands {
            command.encode(&mut encoder);
        }
        encoder.into_vec()
    }
}

// ---------------------------------------------------------------------------------------------------
// capability handshake
// ---------------------------------------------------------------------------------------------------
mod command;
mod descriptor;
