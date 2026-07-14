//! The encode side: descriptor/encoder/command encoders + [`encode_stream`], the capability handshake
//! encoder, and the kernel-descriptor `to_words`. Ported byte-identically from `hl-gpu::ir` / `backend`.
//!
//! The encode logic is attached to the model types as inherent methods defined here, so `Cmd::encode`
//! reads naturally while [`crate::protocol::model`] stays free of serialization code.

use super::wire::Encoder;
use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::{etag, tag, Cmd, CommandBuffer, Enc};
use crate::protocol::model::descriptor::*;
use crate::protocol::model::kernel::{KernelDescriptor, KERNEL_MAGIC};

// ---------------------------------------------------------------------------------------------------
// descriptors
// ---------------------------------------------------------------------------------------------------

fn enc_buffer_desc(e: &mut Encoder, d: &BufferDesc) {
    e.u64(d.size);
    e.u32(d.usage);
    e.str(&d.label);
}

fn enc_texture_desc(e: &mut Encoder, t: &TextureDesc) {
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

fn enc_sampler_desc(e: &mut Encoder, s: &SamplerDesc) {
    e.u32(s.min_filter.to_u32());
    e.u32(s.mag_filter.to_u32());
    e.u32(s.mip_filter.to_u32());
    e.u32(s.address_u.to_u32());
    e.u32(s.address_v.to_u32());
    e.u32(s.address_w.to_u32());
}

fn enc_shader_ref(e: &mut Encoder, s: &ShaderRef) {
    e.u32(s.module);
    e.str(&s.entry);
}

fn enc_vertex_layout(e: &mut Encoder, v: &VertexLayout) {
    e.u32(v.stride);
    e.u32(v.step_mode);
    e.u32(v.attrs.len() as u32);
    for a in &v.attrs {
        e.u32(a.location);
        e.u32(a.format);
        e.u32(a.offset);
    }
}

fn enc_color_target(e: &mut Encoder, c: &ColorTargetState) {
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

fn enc_render_pipeline(e: &mut Encoder, p: &RenderPipelineDesc) {
    enc_shader_ref(e, &p.vertex);
    match &p.fragment {
        None => e.bool(false),
        Some(f) => {
            e.bool(true);
            enc_shader_ref(e, f);
        }
    }
    e.u32(p.vertex_buffers.len() as u32);
    for vb in &p.vertex_buffers {
        enc_vertex_layout(e, vb);
    }
    e.u32(p.color_targets.len() as u32);
    for c in &p.color_targets {
        enc_color_target(e, c);
    }
    match &p.depth {
        None => e.bool(false),
        Some(dp) => {
            e.bool(true);
            e.u32(dp.format.to_u32());
            e.bool(dp.depth_write);
            e.u32(dp.depth_compare);
        }
    }
    e.u32(p.topology.to_u32());
    e.u32(p.cull);
    e.u32(p.front_face);
    e.str(&p.label);
}

fn enc_bind_group(e: &mut Encoder, b: &BindGroupDesc) {
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
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// encoder ops
// ---------------------------------------------------------------------------------------------------

fn enc_subresource(e: &mut Encoder, s: &TextureSubresource) {
    e.u32(s.mip);
    e.u32(s.layer);
    e.u32(s.aspect.to_u32());
}

fn enc_origin(e: &mut Encoder, o: &Origin3d) {
    e.u32(o.x);
    e.u32(o.y);
    e.u32(o.z);
}

fn enc_extent(e: &mut Encoder, x: &Extent3d) {
    e.u32(x.width);
    e.u32(x.height);
    e.u32(x.depth);
}

fn enc_enc(e: &mut Encoder, op: &Enc) {
    match op {
        Enc::BeginRenderPass { color, depth } => {
            e.u8(etag::BEGIN_RENDER_PASS);
            e.u32(color.len() as u32);
            for c in color {
                e.u32(c.texture);
                e.u32(c.load.to_u32());
                for v in c.clear {
                    e.f32(v);
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
        Enc::SetVertexBuffer { slot, buffer, offset } => {
            e.u8(etag::SET_VERTEX_BUFFER);
            e.u32(*slot);
            e.u32(*buffer);
            e.u64(*offset);
        }
        Enc::SetIndexBuffer { buffer, offset, format } => {
            e.u8(etag::SET_INDEX_BUFFER);
            e.u32(*buffer);
            e.u64(*offset);
            e.u32(format.to_u32());
        }
        Enc::SetViewport { x, y, w, h, min_depth, max_depth } => {
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
        Enc::ClearRect { texture, x, y, w, h, color } => {
            e.u8(etag::CLEAR_RECT);
            e.u32(*texture);
            e.u32(*x);
            e.u32(*y);
            e.u32(*w);
            e.u32(*h);
            for v in color {
                e.f32(*v);
            }
        }
        Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
            e.u8(etag::DRAW);
            e.u32(*vertex_count);
            e.u32(*instance_count);
            e.u32(*first_vertex);
            e.u32(*first_instance);
        }
        Enc::DrawIndexed { index_count, instance_count, first_index, base_vertex, first_instance } => {
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
        Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
            e.u8(etag::COPY_B2B);
            e.u32(*src);
            e.u64(*src_offset);
            e.u32(*dst);
            e.u64(*dst_offset);
            e.u64(*size);
        }
        Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
            e.u8(etag::COPY_B2T);
            e.u32(*src);
            e.u64(*src_offset);
            e.u32(*bytes_per_row);
            e.u32(*dst);
            e.u32(*mip);
            e.u32(*width);
            e.u32(*height);
        }
        Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
            e.u8(etag::COPY_T2B);
            e.u32(*src);
            e.u32(*mip);
            e.u32(*width);
            e.u32(*height);
            e.u32(*dst);
            e.u64(*dst_offset);
            e.u32(*bytes_per_row);
        }
        Enc::CopyTextureToTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
            e.u8(etag::COPY_T2T);
            e.u32(*src);
            enc_subresource(e, src_sub);
            enc_origin(e, src_origin);
            e.u32(*dst);
            enc_subresource(e, dst_sub);
            enc_origin(e, dst_origin);
            enc_extent(e, extent);
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
        } => {
            e.u8(etag::BLIT_TEXTURE);
            e.u32(*src);
            enc_subresource(e, src_sub);
            enc_origin(e, src_origin);
            enc_extent(e, src_extent);
            e.u32(*dst);
            enc_subresource(e, dst_sub);
            enc_origin(e, dst_origin);
            enc_extent(e, dst_extent);
            e.u32(filter.to_u32());
        }
        Enc::ResolveTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
            e.u8(etag::RESOLVE_TEXTURE);
            e.u32(*src);
            enc_subresource(e, src_sub);
            enc_origin(e, src_origin);
            e.u32(*dst);
            enc_subresource(e, dst_sub);
            enc_origin(e, dst_origin);
            enc_extent(e, extent);
        }
        Enc::FillBuffer { buffer, offset, size, value } => {
            e.u8(etag::FILL_BUFFER);
            e.u32(*buffer);
            e.u64(*offset);
            e.u64(*size);
            e.u32(*value);
        }
    }
}

fn enc_command_buffer(e: &mut Encoder, cb: &CommandBuffer) {
    e.u32(cb.encoder.len() as u32);
    for op in &cb.encoder {
        enc_enc(e, op);
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

// ---------------------------------------------------------------------------------------------------
// top-level Cmd
// ---------------------------------------------------------------------------------------------------

impl Cmd {
    /// Encode this command (tag + body) into `e`. No length prefix — see [`Cmd::frame`] for that.
    pub fn encode(&self, e: &mut Encoder) {
        match self {
            Cmd::CreateBuffer(id, d) => {
                e.u8(tag::CREATE_BUFFER);
                e.u32(*id);
                enc_buffer_desc(e, d);
            }
            Cmd::DestroyBuffer(id) => {
                e.u8(tag::DESTROY_BUFFER);
                e.u32(*id);
            }
            Cmd::WriteBuffer { id, offset, data } => {
                e.u8(tag::WRITE_BUFFER);
                e.u32(*id);
                e.u64(*offset);
                e.bytes(data);
            }
            Cmd::CreateTexture(id, d) => {
                e.u8(tag::CREATE_TEXTURE);
                e.u32(*id);
                enc_texture_desc(e, d);
            }
            Cmd::DestroyTexture(id) => {
                e.u8(tag::DESTROY_TEXTURE);
                e.u32(*id);
            }
            Cmd::CreateSampler(id, d) => {
                e.u8(tag::CREATE_SAMPLER);
                e.u32(*id);
                enc_sampler_desc(e, d);
            }
            Cmd::DestroySampler(id) => {
                e.u8(tag::DESTROY_SAMPLER);
                e.u32(*id);
            }
            Cmd::CreateShader { id, kind: _, spirv } => {
                e.u8(tag::CREATE_SHADER);
                e.u32(*id);
                // WIRE COMPAT: the shipped guest engine emits the CreateShader layout as `id` followed
                // directly by the shader word payload, with NO ShaderPayloadKind byte. Writing a kind
                // byte here would desync the pinned guest's decoder against ours (it would read the
                // payload's first word-count byte AS the kind and reject real shaders as `BadTag`). Keep
                // the payload byte-identical; the kind is re-derived on decode by the neutral magic.
                e.words(spirv);
            }
            Cmd::DestroyShader(id) => {
                e.u8(tag::DESTROY_SHADER);
                e.u32(*id);
            }
            Cmd::CreateRenderPipeline(id, d) => {
                e.u8(tag::CREATE_RENDER_PIPELINE);
                e.u32(*id);
                enc_render_pipeline(e, d);
            }
            Cmd::CreateComputePipeline(id, d) => {
                e.u8(tag::CREATE_COMPUTE_PIPELINE);
                e.u32(*id);
                enc_shader_ref(e, &d.compute);
                e.str(&d.label);
            }
            Cmd::DestroyPipeline(id) => {
                e.u8(tag::DESTROY_PIPELINE);
                e.u32(*id);
            }
            Cmd::CreateBindGroup(id, d) => {
                e.u8(tag::CREATE_BIND_GROUP);
                e.u32(*id);
                enc_bind_group(e, d);
            }
            Cmd::DestroyBindGroup(id) => {
                e.u8(tag::DESTROY_BIND_GROUP);
                e.u32(*id);
            }
            Cmd::CreateSurface(id, d) => {
                e.u8(tag::CREATE_SURFACE);
                e.u32(*id);
                e.u32(d.width);
                e.u32(d.height);
                e.u32(d.format.to_u32());
                e.u32(d.hlp_surface);
            }
            Cmd::DestroySurface(id) => {
                e.u8(tag::DESTROY_SURFACE);
                e.u32(*id);
            }
            Cmd::CreateFence(id) => {
                e.u8(tag::CREATE_FENCE);
                e.u32(*id);
            }
            Cmd::DestroyFence(id) => {
                e.u8(tag::DESTROY_FENCE);
                e.u32(*id);
            }
            Cmd::Submit(cb) => {
                e.u8(tag::SUBMIT);
                enc_command_buffer(e, cb);
            }
            Cmd::WaitFence { id, value } => {
                e.u8(tag::WAIT_FENCE);
                e.u32(*id);
                e.u64(*value);
            }
            Cmd::Present { surface, texture } => {
                e.u8(tag::PRESENT);
                e.u32(*surface);
                e.u32(*texture);
            }
        }
    }

    /// Encode as a self-delimiting frame (u32 length + body) for the command ring.
    pub fn frame(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.frame(|inner| self.encode(inner));
        e.into_vec()
    }
}

/// Encode a whole command stream (tag+body, back to back, no per-message length prefix).
pub fn encode_stream(cmds: &[Cmd]) -> Vec<u8> {
    let mut e = Encoder::new();
    for c in cmds {
        c.encode(&mut e);
    }
    e.into_vec()
}

// ---------------------------------------------------------------------------------------------------
// capability handshake
// ---------------------------------------------------------------------------------------------------

impl Capabilities {
    /// Serialize this descriptor into the connection handshake byte-stream (the guest decodes it with
    /// [`Capabilities::decode`] and negotiates before advertising any API feature).
    pub fn encode(&self, e: &mut Encoder) {
        e.u32(self.wire_version);
        e.str(&self.name);
        e.bool(self.unified_memory);
        e.bool(self.supports_compute);
        e.bool(self.supports_graphics);
        e.bool(self.supports_timeline_fences);
        e.u32(self.max_texture_2d);
        e.u32(self.max_bind_groups);
        e.u64(self.max_frame_bytes);
        e.u64(self.max_buffer_bytes);
        e.u64(self.command_bits);
        e.u32(self.shader_payloads);
        e.u32(self.texture_formats);
        e.u32(self.present_bits());
    }

    /// Serialize to a standalone handshake frame (u32 length + body).
    pub fn to_handshake(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.frame(|inner| self.encode(inner));
        e.into_vec()
    }
}

// ---------------------------------------------------------------------------------------------------
// kernel descriptor → CreateShader words
// ---------------------------------------------------------------------------------------------------

impl KernelDescriptor {
    /// Serialize into `CreateShader` shader words: `[MAGIC, byte_len, ...packed bytes...]`.
    pub fn to_words(&self) -> Vec<u32> {
        let mut e = Encoder::new();
        e.str(&self.ptx);
        e.str(&self.entry);
        for v in self.block {
            e.u32(v);
        }
        let bytes = e.into_vec();
        let mut words = Vec::with_capacity(2 + bytes.len() / 4 + 1);
        words.push(KERNEL_MAGIC);
        words.push(bytes.len() as u32);
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(b));
        }
        words
    }
}
