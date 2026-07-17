//! `validate` — the per-object shape/limit checks a decoded batch must pass BEFORE anything mutates.
//!
//! First stage of the runtime pipeline (decode → **validate** → account → dispatch). Every check here is
//! read-only over the batch: a rejection anywhere leaves accounting and executor state untouched, so a
//! malformed command late in a frame can never leave earlier commands' effects behind (failure
//! atomicity). Ported from `hl-gpu/src/limits.rs` (`ReplayLimits::validate` + `check_copy_alignment`),
//! reusing [`texture_bytes`](crate::runtime::model::resources::texture_bytes) for the footprint check.

use crate::protocol::model::capability::shader_payload;
use crate::protocol::model::command::{Cmd, Enc, ShaderPayloadKind};
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::texture_bytes;
use crate::runtime::model::session::Limits;

/// The negotiated copy alignment must divide every buffer/image copy offset, size, and row stride.
fn check_copy_alignment(alignment: u64, op: &Enc) -> Result<()> {
    if alignment <= 1 {
        return Ok(());
    }
    let a = alignment;
    let misaligned = match op {
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
        } => src_offset % a != 0 || (*bytes_per_row as u64) % a != 0,
        Enc::CopyTextureToBuffer {
            dst_offset,
            bytes_per_row,
            ..
        } => dst_offset % a != 0 || (*bytes_per_row as u64) % a != 0,
        _ => false,
    };
    if misaligned {
        return Err(GpuError::ResourceLimit("copy alignment"));
    }
    Ok(())
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
                if texture_bytes(d)? > caps.max_buffer_bytes {
                    return Err(GpuError::ResourceLimit("texture bytes"));
                }
                let bit = 1u32.checked_shl(d.format.to_u32()).unwrap_or(0);
                if caps.texture_formats & bit == 0 {
                    return Err(GpuError::ResourceLimit("texture format"));
                }
            }
            Cmd::CreateShader { kind, .. } => {
                let bit = match kind {
                    ShaderPayloadKind::SpirV => shader_payload::SPIRV,
                    ShaderPayloadKind::Glsl => shader_payload::GLSL,
                    ShaderPayloadKind::LegacyMsl => shader_payload::MSL,
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
            Cmd::Submit(cb) => {
                for op in &cb.encoder {
                    let tag = op.wire_tag();
                    if tag >= 64 || caps.command_bits & (1u64 << tag) == 0 {
                        return Err(GpuError::ResourceLimit("encoder command"));
                    }
                    check_copy_alignment(limits.copy_alignment, op)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
