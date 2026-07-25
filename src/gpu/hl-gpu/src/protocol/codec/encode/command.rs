impl Cmd {
    /// Encode this command (tag + body) into `e`. No length prefix — see [`Cmd::frame`] for that.
    pub fn encode(&self, e: &mut Encoder) {
        match self {
            Cmd::CreateBuffer(id, d) => {
                e.u8(tag::CREATE_BUFFER);
                e.u32(*id);
                e.buffer_desc(d);
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
                e.texture_desc(d);
            }
            Cmd::DestroyTexture(id) => {
                e.u8(tag::DESTROY_TEXTURE);
                e.u32(*id);
            }
            Cmd::CreateSampler(id, d) => {
                e.u8(tag::CREATE_SAMPLER);
                e.u32(*id);
                e.sampler_desc(d);
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
                e.render_pipeline(d);
            }
            Cmd::CreateComputePipeline(id, d) => {
                e.u8(tag::CREATE_COMPUTE_PIPELINE);
                e.u32(*id);
                e.shader_ref(&d.compute);
                e.str(&d.label);
            }
            Cmd::DestroyPipeline(id) => {
                e.u8(tag::DESTROY_PIPELINE);
                e.u32(*id);
            }
            Cmd::CreateBindGroup(id, d) => {
                e.u8(tag::CREATE_BIND_GROUP);
                e.u32(*id);
                e.bind_group(d);
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
                e.command_buffer(cb);
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
use super::*;
