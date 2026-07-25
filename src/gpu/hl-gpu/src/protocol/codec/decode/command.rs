impl Cmd {
    /// Decode one command (tag + body) from `d`.
    pub fn decode(d: &mut Decoder) -> Result<Cmd> {
        Ok(match d.u8()? {
            tag::CREATE_BUFFER => Cmd::CreateBuffer(d.u32()?, d.buffer_desc()?),
            tag::DESTROY_BUFFER => Cmd::DestroyBuffer(d.u32()?),
            tag::WRITE_BUFFER => Cmd::WriteBuffer {
                id: d.u32()?,
                offset: d.u64()?,
                data: d.bytes()?.to_vec(),
            },
            tag::CREATE_TEXTURE => Cmd::CreateTexture(d.u32()?, d.texture_desc()?),
            tag::DESTROY_TEXTURE => Cmd::DestroyTexture(d.u32()?),
            tag::CREATE_SAMPLER => Cmd::CreateSampler(d.u32()?, d.sampler_desc()?),
            tag::DESTROY_SAMPLER => Cmd::DestroySampler(d.u32()?),
            tag::CREATE_SHADER => {
                // WIRE COMPAT (see `encode`): the pinned guest engine speaks the CreateShader layout with
                // no ShaderPayloadKind byte, so we must NOT consume one here. Instead we recover the kind
                // losslessly by inspecting the payload's leading word — each real kind is self-identifying,
                // classified against the NEUTRAL magics in `model::kernel` (never a CUDA/PTX constant):
                //   * SPIRV_MAGIC (0x07230203)  → SpirV     (translated Vulkan modules)
                //   * KERNEL_MAGIC (0xDD6B0001) → PtxKernel (neutral kernel descriptor)
                //   * GLSL_MAGIC  (0xDD670001)  → Glsl      (forwarded GLSL descriptor, WIRE_VERSION 6)
                //   * anything else             → Msl (already-translated MSL words)
                // MSL text words never collide with these magics (both decode to non-ASCII byte runs), so
                // the inference is unambiguous for every payload that actually crosses the ring.
                let id = d.u32()?;
                let spirv = d.words()?;
                let kind = match spirv.first().copied() {
                    Some(SPIRV_MAGIC) => ShaderPayloadKind::SpirV,
                    Some(w) if w == KERNEL_MAGIC => ShaderPayloadKind::PtxKernel,
                    Some(w) if w == GLSL_MAGIC => ShaderPayloadKind::Glsl,
                    _ => ShaderPayloadKind::Msl,
                };
                Cmd::CreateShader { id, kind, spirv }
            }
            tag::DESTROY_SHADER => Cmd::DestroyShader(d.u32()?),
            tag::CREATE_RENDER_PIPELINE => {
                Cmd::CreateRenderPipeline(d.u32()?, d.render_pipeline()?)
            }
            tag::CREATE_COMPUTE_PIPELINE => {
                let id = d.u32()?;
                let compute = d.shader_ref()?;
                let label = d.str()?;
                Cmd::CreateComputePipeline(id, ComputePipelineDesc { compute, label })
            }
            tag::DESTROY_PIPELINE => Cmd::DestroyPipeline(d.u32()?),
            tag::CREATE_BIND_GROUP => Cmd::CreateBindGroup(d.u32()?, d.bind_group()?),
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
            tag::SUBMIT => Cmd::Submit(d.command_buffer()?),
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

impl<'a> Decoder<'a> {
    /// Decode a whole command stream until the input is exhausted.
    pub fn stream(bytes: &'a [u8]) -> Result<Vec<Cmd>> {
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
                    hl_log::hl_warn!(
                        hl_log::tag::WIRE,
                        "decode err cmd={} byte={}/{} tag={:?}",
                        idx,
                        pos,
                        d.len(),
                        tag
                    );
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
}

// ---------------------------------------------------------------------------------------------------
// capability handshake
// ---------------------------------------------------------------------------------------------------
use super::*;
