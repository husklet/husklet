//! Negotiated replay validation and transactional executor residency accounting.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::backend::{shader_payload, Capabilities};
use crate::ir::{BindResource, Cmd, ShaderPayloadKind, TextureDesc};
use crate::{GpuError, Result};

#[derive(Clone, Debug)]
pub struct ReplayLimits {
    pub caps: Capabilities,
    pub max_connection_bytes: u64,
    pub max_connection_objects: u64,
}

impl ReplayLimits {
    pub fn from_capabilities(caps: Capabilities) -> Self {
        Self { caps, max_connection_bytes: 512 << 20, max_connection_objects: 65_536 }
    }

    pub fn validate(&self, frame_bytes: usize, cmds: &[Cmd]) -> Result<()> {
        if frame_bytes as u64 > self.caps.max_frame_bytes {
            return Err(GpuError::ResourceLimit("frame bytes"));
        }
        for cmd in cmds {
            match cmd {
                Cmd::CreateBuffer(_, d) if d.size > self.caps.max_buffer_bytes => {
                    return Err(GpuError::ResourceLimit("buffer bytes"));
                }
                Cmd::CreateTexture(_, d) => {
                    if d.width > self.caps.max_texture_2d || d.height > self.caps.max_texture_2d {
                        return Err(GpuError::ResourceLimit("texture dimensions"));
                    }
                    if texture_bytes(d)? > self.caps.max_buffer_bytes {
                        return Err(GpuError::ResourceLimit("texture bytes"));
                    }
                    let bit = 1u32.checked_shl(d.format.to_u32()).unwrap_or(0);
                    if self.caps.texture_formats & bit == 0 {
                        return Err(GpuError::ResourceLimit("texture format"));
                    }
                }
                Cmd::CreateShader { kind, .. } => {
                    let bit = match kind {
                        ShaderPayloadKind::SpirV => shader_payload::SPIRV,
                        ShaderPayloadKind::LegacyMsl => shader_payload::MSL,
                        ShaderPayloadKind::PtxKernel => shader_payload::PTX,
                        ShaderPayloadKind::DemoBuiltin => 0,
                    };
                    if bit != 0 && self.caps.shader_payloads & bit == 0 {
                        return Err(GpuError::ResourceLimit("shader payload"));
                    }
                }
                Cmd::CreateBindGroup(_, d) if d.set >= self.caps.max_bind_groups => {
                    return Err(GpuError::ResourceLimit("bind groups"));
                }
                Cmd::Submit(cb) => {
                    for op in &cb.encoder {
                        let tag = op.wire_tag();
                        if tag >= 64 || self.caps.command_bits & (1u64 << tag) == 0 {
                            return Err(GpuError::ResourceLimit("encoder command"));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn texture_bytes(d: &TextureDesc) -> Result<u64> {
    let texel = d
        .format
        .bytes_per_texel()
        .ok_or(GpuError::ResourceLimit("texture format footprint"))? as u64;
    let mut total = 0u64;
    let mut w = d.width as u64;
    let mut h = d.height as u64;
    let mut depth = d.depth as u64;
    for _ in 0..d.mip_levels {
        let level = w
            .max(1)
            .checked_mul(h.max(1))
            .and_then(|v| v.checked_mul(depth.max(1)))
            .and_then(|v| v.checked_mul(d.sample_count as u64))
            .and_then(|v| v.checked_mul(texel))
            .ok_or(GpuError::ResourceLimit("texture footprint overflow"))?;
        total = total.checked_add(level).ok_or(GpuError::ResourceLimit("texture footprint overflow"))?;
        w >>= 1;
        h >>= 1;
        depth >>= 1;
    }
    Ok(total)
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Totals {
    bytes: u64,
    objects: u64,
}

#[derive(Clone)]
pub struct GlobalBudget {
    inner: Arc<Mutex<Totals>>,
    max_bytes: u64,
    max_objects: u64,
}

impl GlobalBudget {
    pub fn new(max_bytes: u64, max_objects: u64) -> Self {
        Self { inner: Arc::new(Mutex::new(Totals::default())), max_bytes, max_objects }
    }
}

pub struct ExecutorBudget {
    limits: ReplayLimits,
    global: GlobalBudget,
    live: HashMap<(u8, u32), u64>,
    totals: Totals,
}

impl ExecutorBudget {
    pub fn new(limits: ReplayLimits, global: GlobalBudget) -> Self {
        Self { limits, global, live: HashMap::new(), totals: Totals::default() }
    }

    pub fn max_frame_bytes(&self) -> u64 {
        self.limits.caps.max_frame_bytes
    }

    /// Validate and charge a complete frame transactionally. Limit failure leaves both the connection
    /// and process-wide accounts unchanged.
    pub fn preflight(&mut self, frame_bytes: usize, cmds: &[Cmd]) -> Result<()> {
        self.limits.validate(frame_bytes, cmds)?;
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        for cmd in cmds {
            if let Some((kind, id, bytes)) = create_charge(cmd)? {
                if next_live.insert((kind, id), bytes).is_some() {
                    return Err(GpuError::Invalid("duplicate budget object id"));
                }
                next.bytes = next.bytes.checked_add(bytes).ok_or(GpuError::ResourceLimit("residency overflow"))?;
                next.objects = next.objects.checked_add(1).ok_or(GpuError::ResourceLimit("object count overflow"))?;
            } else if let Some((kind, id)) = destroy_key(cmd) {
                if let Some(bytes) = next_live.remove(&(kind, id)) {
                    next.bytes -= bytes;
                    next.objects -= 1;
                }
            }
        }
        if next.bytes > self.limits.max_connection_bytes || next.objects > self.limits.max_connection_objects {
            return Err(GpuError::ResourceLimit("connection residency"));
        }
        let mut global = self.global.inner.lock().unwrap_or_else(|e| e.into_inner());
        let without_self = Totals {
            bytes: global.bytes.saturating_sub(self.totals.bytes),
            objects: global.objects.saturating_sub(self.totals.objects),
        };
        let proposed = Totals {
            bytes: without_self.bytes.checked_add(next.bytes).ok_or(GpuError::ResourceLimit("global residency overflow"))?,
            objects: without_self.objects.checked_add(next.objects).ok_or(GpuError::ResourceLimit("global object overflow"))?,
        };
        if proposed.bytes > self.global.max_bytes || proposed.objects > self.global.max_objects {
            return Err(GpuError::ResourceLimit("global residency"));
        }
        *global = proposed;
        self.live = next_live;
        self.totals = next;
        Ok(())
    }
}

impl Drop for ExecutorBudget {
    fn drop(&mut self) {
        let mut global = self.global.inner.lock().unwrap_or_else(|e| e.into_inner());
        global.bytes = global.bytes.saturating_sub(self.totals.bytes);
        global.objects = global.objects.saturating_sub(self.totals.objects);
    }
}

fn create_charge(cmd: &Cmd) -> Result<Option<(u8, u32, u64)>> {
    Ok(match cmd {
        Cmd::CreateBuffer(id, d) => Some((1, *id, d.size)),
        Cmd::CreateTexture(id, d) => Some((2, *id, texture_bytes(d)?)),
        Cmd::CreateSampler(id, _) => Some((3, *id, 64)),
        Cmd::CreateShader { id, spirv, .. } => Some((4, *id, (spirv.len() as u64).saturating_mul(4))),
        Cmd::CreateRenderPipeline(id, _) | Cmd::CreateComputePipeline(id, _) => Some((5, *id, 4096)),
        Cmd::CreateBindGroup(id, d) => {
            let referenced = d.entries.iter().map(|e| match e.resource {
                BindResource::Buffer { size, .. } => size,
                _ => 64,
            }).sum::<u64>();
            Some((6, *id, 256u64.saturating_add(referenced.min(4096))))
        }
        _ => None,
    })
}

fn destroy_key(cmd: &Cmd) -> Option<(u8, u32)> {
    match cmd {
        Cmd::DestroyBuffer(id) => Some((1, *id)),
        Cmd::DestroyTexture(id) => Some((2, *id)),
        Cmd::DestroySampler(id) => Some((3, *id)),
        Cmd::DestroyShader(id) => Some((4, *id)),
        Cmd::DestroyPipeline(id) => Some((5, *id)),
        Cmd::DestroyBindGroup(id) => Some((6, *id)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::GpuBackend;
    use crate::ir::{buffer_usage, BufferDesc};

    fn limits(connection_bytes: u64, connection_objects: u64) -> ReplayLimits {
        let mut caps = crate::software::SoftwareBackend::new().capabilities();
        caps.max_frame_bytes = 1024;
        caps.max_buffer_bytes = 16;
        ReplayLimits { caps, max_connection_bytes: connection_bytes, max_connection_objects: connection_objects }
    }

    #[test]
    fn exact_buffer_limit_is_accepted_and_limit_plus_one_is_atomic() {
        let global = GlobalBudget::new(1024, 32);
        let mut budget = ExecutorBudget::new(limits(16, 1), global);
        let exact = Cmd::CreateBuffer(
            1,
            BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() },
        );
        budget.preflight(32, &[exact]).expect("exact boundary");
        let before = budget.totals;
        let over = Cmd::CreateBuffer(
            2,
            BufferDesc { size: 17, usage: buffer_usage::COPY_DST, label: String::new() },
        );
        assert_eq!(budget.preflight(32, &[over]), Err(GpuError::ResourceLimit("buffer bytes")));
        assert_eq!(budget.totals, before, "failed charge did not mutate accounting");
    }

    #[test]
    fn destroy_refunds_and_recreate_can_reuse_the_full_connection_budget() {
        let global = GlobalBudget::new(1024, 32);
        let mut budget = ExecutorBudget::new(limits(16, 1), global);
        let desc = BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() };
        budget.preflight(20, &[Cmd::CreateBuffer(1, desc.clone())]).unwrap();
        budget.preflight(8, &[Cmd::DestroyBuffer(1)]).unwrap();
        budget.preflight(20, &[Cmd::CreateBuffer(2, desc)]).expect("refund is exact");
        assert_eq!(budget.totals, Totals { bytes: 16, objects: 1 });
    }

    #[test]
    fn global_budget_isolates_connections_and_drop_refunds_owner() {
        let global = GlobalBudget::new(16, 1);
        let desc = BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() };
        let mut first = ExecutorBudget::new(limits(16, 1), global.clone());
        first.preflight(20, &[Cmd::CreateBuffer(1, desc.clone())]).unwrap();
        let mut abusive = ExecutorBudget::new(limits(16, 1), global.clone());
        assert_eq!(
            abusive.preflight(20, &[Cmd::CreateBuffer(2, desc.clone())]),
            Err(GpuError::ResourceLimit("global residency"))
        );
        assert_eq!(first.totals.bytes, 16, "the accepted connection remains charged and usable");
        drop(first);
        abusive.preflight(20, &[Cmd::CreateBuffer(2, desc)]).expect("disconnect refunded global owner");
    }

    #[test]
    fn texture_footprint_overflow_and_frame_limit_are_rejected_before_charge() {
        let mut l = limits(u64::MAX, 8);
        l.caps.max_texture_2d = u32::MAX;
        l.caps.max_buffer_bytes = u64::MAX;
        let global = GlobalBudget::new(u64::MAX, 8);
        let mut budget = ExecutorBudget::new(l, global);
        let mut desc = crate::ir::TextureDesc {
            width: u32::MAX,
            height: u32::MAX,
            depth: u32::MAX,
            mip_levels: 1,
            sample_count: u32::MAX,
            dim: crate::ir::TextureDim::D3,
            format: crate::ir::TextureFormat::Rgba8Unorm,
            usage: 0,
            label: String::new(),
        };
        assert_eq!(
            budget.preflight(32, &[Cmd::CreateTexture(1, desc.clone())]),
            Err(GpuError::ResourceLimit("texture footprint overflow"))
        );
        desc.width = 1;
        desc.height = 1;
        desc.depth = 1;
        desc.sample_count = 1;
        assert_eq!(budget.preflight(1025, &[Cmd::CreateTexture(1, desc)]), Err(GpuError::ResourceLimit("frame bytes")));
        assert_eq!(budget.totals, Totals::default());
    }

    #[test]
    fn rejected_limited_frame_never_reaches_backend() {
        let global = GlobalBudget::new(1024, 8);
        let mut budget = ExecutorBudget::new(limits(16, 1), global);
        let cmds = vec![Cmd::CreateBuffer(
            1,
            BufferDesc { size: 17, usage: buffer_usage::COPY_DST, label: String::new() },
        )];
        let wire = crate::ir::encode_stream(&cmds);
        let mut backend = crate::mock::RecordingBackend::new();
        assert_eq!(
            crate::replay::replay_stream_limited(&mut backend, &wire, &mut budget),
            Err(GpuError::ResourceLimit("buffer bytes"))
        );
        assert!(backend.log.is_empty(), "limit rejection happened before backend mutation");
        assert_eq!(budget.totals, Totals::default());
    }
}
