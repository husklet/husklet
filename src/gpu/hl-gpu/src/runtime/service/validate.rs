//! `validate` — the per-object shape/limit checks a decoded batch must pass BEFORE anything mutates.
//!
//! First stage of the runtime pipeline (decode → **validate** → account → dispatch). Every check here is
//! read-only over the batch: a rejection anywhere leaves accounting and executor state untouched, so a
//! malformed command late in a frame can never leave earlier commands' effects behind (failure
//! atomicity). Ported from `hl-gpu/src/limits.rs` (`ReplayLimits::validate` + `check_copy_alignment`),
//! reusing [`texture_bytes`](crate::runtime::model::resources::texture_bytes) for the footprint check.

use crate::protocol::model::capability::shader_payload;
use crate::protocol::model::command::{Cmd, Enc, ShaderPayloadKind};
use crate::protocol::model::enums::{compare, stencil_op};
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::session::Limits;

/// The negotiated copy alignment must divide every buffer/image copy offset, size, and row stride.
impl Enc {
    fn validate_copy_alignment(&self, alignment: u64) -> Result<()> {
        if alignment <= 1 {
            return Ok(());
        }
        let a = alignment;
        let misaligned = match self {
            Enc::CopyBufferToBuffer {
                src_offset,
                dst_offset,
                size,
                ..
            } => src_offset % a != 0 || dst_offset % a != 0 || size % a != 0,
            Enc::CopyBufferToTexture {
                src_offset,
                bytes_per_row,
                ..
            }
            | Enc::CopyBufferToTextureRegion {
                src_offset,
                bytes_per_row,
                ..
            } => src_offset % a != 0 || !(*bytes_per_row as u64).is_multiple_of(a),
            Enc::CopyTextureToBuffer {
                dst_offset,
                bytes_per_row,
                ..
            }
            | Enc::CopyTextureToBufferRegion {
                dst_offset,
                bytes_per_row,
                ..
            } => dst_offset % a != 0 || !(*bytes_per_row as u64).is_multiple_of(a),
            _ => false,
        };
        if misaligned {
            return Err(GpuError::ResourceLimit("copy alignment"));
        }
        Ok(())
    }
}

/// Validate a whole decoded frame against the negotiated [`Limits`]. Checked before any charge or
/// executor mutation: frame size, per-object ceilings (buffer/texture bytes, texture dims + format,
/// shader payload, bind-group index), and, for each submitted encoder op, that its command tag is in the
/// negotiated set and its copies meet the negotiated alignment.
pub fn validate(limits: &Limits, frame_bytes: usize, cmds: &[Cmd]) -> Result<()> {
    let caps = &limits.caps;
    if frame_bytes as u64 > caps.max_frame_bytes {
        return Err(GpuError::ResourceLimit("frame bytes"));
    }
    for cmd in cmds {
        match cmd {
            Cmd::CreateBuffer(_, d) if d.size > caps.max_buffer_bytes => {
                return Err(GpuError::ResourceLimit("buffer bytes"));
            }
            Cmd::CreateTexture(_, d) => {
                if d.width > caps.max_texture_2d || d.height > caps.max_texture_2d {
                    return Err(GpuError::ResourceLimit("texture dimensions"));
                }
                if d.residency_bytes()? > caps.max_buffer_bytes {
                    return Err(GpuError::ResourceLimit("texture bytes"));
                }
                let bit = 1u64.checked_shl(d.format.to_u32()).unwrap_or(0);
                if caps.texture_formats & bit == 0 {
                    return Err(GpuError::ResourceLimit("texture format"));
                }
            }
            Cmd::CreateSampler(_, descriptor) => {
                if !descriptor.lod_min_clamp.is_finite()
                    || !descriptor.lod_max_clamp.is_finite()
                    || descriptor.lod_min_clamp < 0.0
                    || descriptor.lod_max_clamp < descriptor.lod_min_clamp
                {
                    return Err(GpuError::Invalid("sampler LOD clamp"));
                }
                if descriptor
                    .compare
                    .is_some_and(|function| function > compare::ALWAYS)
                {
                    return Err(GpuError::Invalid("sampler comparison"));
                }
            }
            Cmd::CreateShader { kind, .. } => {
                let bit = match kind {
                    ShaderPayloadKind::SpirV => shader_payload::SPIRV,
                    ShaderPayloadKind::Glsl => shader_payload::GLSL,
                    ShaderPayloadKind::Msl => shader_payload::MSL,
                    ShaderPayloadKind::PtxKernel => shader_payload::KERNEL,
                    ShaderPayloadKind::DemoBuiltin => 0,
                };
                if bit != 0 && caps.shader_payloads & bit == 0 {
                    return Err(GpuError::ResourceLimit("shader payload"));
                }
            }
            Cmd::CreateBindGroup(_, d) if d.set >= caps.max_bind_groups => {
                return Err(GpuError::ResourceLimit("bind groups"));
            }
            Cmd::CreateRenderPipeline(_, descriptor) => {
                // Depth/stencil comparisons and stencil ops are opaque `u32` codes, and BOTH executors
                // absorbed an unrecognised one into a permissive answer: the oracle's `compare::passes`
                // returned true and the wgpu path mapped it to `Always`, so a depth test the guest asked
                // for was silently not performed and the draw reported success. The sampler's compare has
                // been range-checked here all along — same constants, twenty lines up — and nothing
                // distinguished the two but oversight.
                if let Some(depth) = &descriptor.depth {
                    if depth.depth_compare > compare::ALWAYS {
                        return Err(GpuError::Invalid("depth compare"));
                    }
                    for face in [&depth.stencil_front, &depth.stencil_back] {
                        if face.compare > compare::ALWAYS {
                            return Err(GpuError::Invalid("stencil compare"));
                        }
                        for op in [face.fail_op, face.depth_fail_op, face.pass_op] {
                            if op > stencil_op::DECREMENT_WRAP {
                                return Err(GpuError::Invalid("stencil operation"));
                            }
                        }
                    }
                }
            }
            Cmd::CreateRenderPipelineLayout(_, _, layout, _)
            | Cmd::CreateComputePipelineLayout(_, _, layout) => {
                for binding in &layout.bindings {
                    if binding.group >= caps.max_bind_groups {
                        return Err(GpuError::ResourceLimit("bind groups"));
                    }
                    if binding.count == 0 || binding.count > 1_048_576 {
                        return Err(GpuError::ResourceLimit("descriptor count"));
                    }
                    if binding.count > 1 {
                        use crate::protocol::model::capability::binding_array;
                        use crate::protocol::model::descriptor::PipelineBindingKind;
                        let required = match binding.kind {
                            PipelineBindingKind::UniformBuffer => binding_array::UNIFORM_BUFFER,
                            PipelineBindingKind::StorageBuffer => binding_array::STORAGE_BUFFER,
                            PipelineBindingKind::UniformTexelBuffer
                            | PipelineBindingKind::StorageTexelBuffer => {
                                binding_array::STORAGE_BUFFER
                            }
                            PipelineBindingKind::SampledTexture => binding_array::SAMPLED_TEXTURE,
                            PipelineBindingKind::StorageTexture => binding_array::STORAGE_TEXTURE,
                            PipelineBindingKind::Sampler => binding_array::SAMPLER,
                            PipelineBindingKind::CombinedImageSampler => {
                                binding_array::SAMPLED_TEXTURE | binding_array::SAMPLER
                            }
                        };
                        if caps.binding_arrays & required != required {
                            return Err(GpuError::Unsupported("binding array kind"));
                        }
                    }
                }
            }
            Cmd::Submit(cb) => {
                for op in &cb.encoder {
                    let tag = op.wire_tag();
                    if tag >= 64 || caps.command_bits & (1u64 << tag) == 0 {
                        return Err(GpuError::ResourceLimit("encoder command"));
                    }
                    op.validate_copy_alignment(limits.copy_alignment)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
