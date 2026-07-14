//! Negotiated replay validation and transactional executor residency accounting.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::backend::{shader_payload, Capabilities};
use crate::ir::{BindResource, Cmd, Enc, ShaderPayloadKind, SurfaceDesc, TextureDesc};
use crate::{GpuError, Result};

#[derive(Clone, Debug)]
pub struct ReplayLimits {
    pub caps: Capabilities,
    pub max_connection_bytes: u64,
    pub max_connection_objects: u64,
    /// Negotiated backend copy alignment (bytes). Buffer-copy offsets/sizes and image-copy
    /// `bytes_per_row`/offsets must be a multiple of this before the executor decodes the transfer or
    /// allocates any staging — a real Metal/wgpu backend rejects a misaligned copy at encode, so the
    /// executor enforces the negotiated alignment up front instead of surfacing a driver error mid-frame.
    /// `<= 1` disables the check (byte-addressable). Derived from the connection's negotiated capabilities.
    pub copy_alignment: u64,
    /// Negotiated per-connection compiled-pipeline (PSO/AIR) cache ceiling in bytes. Each created render
    /// or compute pipeline charges its compiled-cache footprint against this budget; a connection that
    /// would blow the compiled-cache ceiling is rejected before the executor compiles/allocates the
    /// pipeline, bounding the warm host-side shader cache independently of raw resource residency.
    pub max_compiled_cache_bytes: u64,
}

impl ReplayLimits {
    pub fn from_capabilities(caps: Capabilities) -> Self {
        Self {
            caps,
            max_connection_bytes: 512 << 20,
            max_connection_objects: 65_536,
            copy_alignment: 4,
            max_compiled_cache_bytes: 64 << 20,
        }
    }

    /// The negotiated copy alignment must divide every buffer/image copy offset, size, and row stride.
    /// Checked over a decoded frame before any charge or backend mutation (validate-before-execute).
    fn check_copy_alignment(&self, op: &Enc) -> Result<()> {
        let a = self.copy_alignment;
        if a <= 1 {
            return Ok(());
        }
        let misaligned = match op {
            Enc::CopyBufferToBuffer { src_offset, dst_offset, size, .. } => {
                src_offset % a != 0 || dst_offset % a != 0 || size % a != 0
            }
            Enc::CopyBufferToTexture { src_offset, bytes_per_row, .. } => {
                src_offset % a != 0 || (*bytes_per_row as u64) % a != 0
            }
            Enc::CopyTextureToBuffer { dst_offset, bytes_per_row, .. } => {
                dst_offset % a != 0 || (*bytes_per_row as u64) % a != 0
            }
            _ => false,
        };
        if misaligned {
            return Err(GpuError::ResourceLimit("copy alignment"));
        }
        Ok(())
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
                        self.check_copy_alignment(op)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn texture_bytes(d: &TextureDesc) -> Result<u64> {
    use crate::ir::TextureDim;

    if d.width == 0 || d.height == 0 || d.depth == 0 {
        return Err(GpuError::ResourceLimit("zero texture dimension"));
    }
    if d.mip_levels == 0 {
        return Err(GpuError::ResourceLimit("zero texture mip levels"));
    }
    if d.sample_count == 0 {
        return Err(GpuError::ResourceLimit("zero texture sample count"));
    }
    match d.dim {
        TextureDim::D1 if d.height != 1 || d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid 1D texture shape"));
        }
        TextureDim::D2 => {}
        TextureDim::D3 if d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid 3D texture sample count"));
        }
        TextureDim::Cube if d.width != d.height || d.depth % 6 != 0 || d.sample_count != 1 => {
            return Err(GpuError::ResourceLimit("invalid cube texture shape"));
        }
        _ => {}
    }
    let max_mip_dimension = match d.dim {
        TextureDim::D1 => d.width,
        TextureDim::D2 | TextureDim::Cube => d.width.max(d.height),
        TextureDim::D3 => d.width.max(d.height).max(d.depth),
    };
    let max_mips = u32::BITS - max_mip_dimension.leading_zeros();
    if d.mip_levels > max_mips {
        return Err(GpuError::ResourceLimit("texture mip levels exceed dimensions"));
    }
    // Depth/stencil formats report `bytes_per_texel() == None` (the software backend can't clear-fill
    // them), but for memory-footprint accounting they occupy a real 4-byte-per-texel target. Charge
    // that (matching the depth-tolerant accounting elsewhere in this module) instead of rejecting the
    // allocation — a depth attachment is a valid render target (vkcube binds one).
    let texel = d.format.bytes_per_texel().unwrap_or(4) as u64;
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
        if d.dim == TextureDim::D3 {
            depth >>= 1;
        }
    }
    Ok(total)
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Totals {
    bytes: u64,
    objects: u64,
    /// Subset of `bytes` attributable to compiled-pipeline (PSO/AIR) cache entries, tracked separately so
    /// the warm shader cache can be bounded by its own negotiated ceiling. Process-global totals ignore
    /// this field (it is a per-connection accounting).
    compiled_bytes: u64,
}

/// Backing residency (bytes) an executor must keep resident for a presentable surface: one full-frame
/// render target at the surface's format footprint.
fn surface_bytes(d: &SurfaceDesc) -> u64 {
    let texel = d.format.bytes_per_texel().unwrap_or(4) as u64;
    (d.width as u64).saturating_mul(d.height as u64).saturating_mul(texel)
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
    /// and process-wide accounts unchanged. Charges core ids, surfaces, fences, and the compiled-pipeline
    /// cache; external allocations and ownership transfers are charged out-of-band via the dedicated
    /// methods below.
    pub fn preflight(&mut self, frame_bytes: usize, cmds: &[Cmd]) -> Result<()> {
        self.limits.validate(frame_bytes, cmds)?;
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        for cmd in cmds {
            if let Some((kind, id, bytes)) = create_charge(cmd)? {
                // Guests legitimately RE-CREATE an object under a stable id every frame without an explicit
                // Destroy (create-or-replace: the shim re-emits CreateShader/CreateBuffer/… for the same id,
                // and the backend replaces it — see the L3 id-hash cache). Treat a create over a still-live
                // id as a residency SWAP (drop the old charge, add the new) rather than a fatal "duplicate":
                // erroring here rejected the whole frame and left real apps (Chrome) unable to render, while
                // the residency ceiling is still enforced because only the delta is charged.
                if let Some(old) = next_live.insert((kind, id), bytes) {
                    next.bytes = next.bytes.saturating_sub(old);
                    if kind == KIND_PIPELINE {
                        next.compiled_bytes = next.compiled_bytes.saturating_sub(old);
                    }
                    next.objects = next.objects.saturating_sub(1);
                }
                next.bytes = next.bytes.checked_add(bytes).ok_or(GpuError::ResourceLimit("residency overflow"))?;
                next.objects = next.objects.checked_add(1).ok_or(GpuError::ResourceLimit("object count overflow"))?;
                if kind == KIND_PIPELINE {
                    next.compiled_bytes =
                        next.compiled_bytes.checked_add(bytes).ok_or(GpuError::ResourceLimit("compiled cache overflow"))?;
                }
            } else if let Some((kind, id)) = destroy_key(cmd) {
                if let Some(bytes) = next_live.remove(&(kind, id)) {
                    next.bytes -= bytes;
                    next.objects -= 1;
                    if kind == KIND_PIPELINE {
                        next.compiled_bytes -= bytes;
                    }
                }
            }
        }
        self.commit(next_live, next)
    }

    /// Charge an external allocation (a render-node dma-buf / IOSurface imported from the guest, not
    /// produced by the IR command stream) against this connection's cumulative residency. Transactional:
    /// a limit rejection leaves the connection and process-global accounts unchanged.
    pub fn charge_external_allocation(&mut self, id: u32, bytes: u64) -> Result<()> {
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        if next_live.insert((KIND_EXTERNAL, id), bytes).is_some() {
            return Err(GpuError::Invalid("duplicate external allocation id"));
        }
        next.bytes = next.bytes.checked_add(bytes).ok_or(GpuError::ResourceLimit("residency overflow"))?;
        next.objects = next.objects.checked_add(1).ok_or(GpuError::ResourceLimit("object count overflow"))?;
        self.commit(next_live, next)
    }

    /// Release a previously-charged external allocation. Errors if the id was never charged.
    pub fn release_external_allocation(&mut self, id: u32) -> Result<()> {
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        let bytes = next_live
            .remove(&(KIND_EXTERNAL, id))
            .ok_or(GpuError::UnknownId { kind: "external allocation", id })?;
        next.bytes -= bytes;
        next.objects -= 1;
        self.commit(next_live, next)
    }

    /// Accept ownership of an object transferred INTO this connection (e.g. a surface buffer handed back
    /// from the compositor). Charges it to this connection's residency under `(kind, id)`.
    pub fn accept_ownership_transfer(&mut self, kind: u8, id: u32, bytes: u64) -> Result<()> {
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        if next_live.insert((kind, id), bytes).is_some() {
            return Err(GpuError::Invalid("ownership transfer over a live id"));
        }
        next.bytes = next.bytes.checked_add(bytes).ok_or(GpuError::ResourceLimit("residency overflow"))?;
        next.objects = next.objects.checked_add(1).ok_or(GpuError::ResourceLimit("object count overflow"))?;
        self.commit(next_live, next)
    }

    /// Transfer ownership of a live object OUT of this connection (e.g. a surface buffer handed to the
    /// compositor). Removes its residency charge from this connection and returns the bytes released so
    /// the receiving accountant can charge them. Errors if the object is not live here.
    pub fn release_ownership_transfer(&mut self, kind: u8, id: u32) -> Result<u64> {
        let mut next_live = self.live.clone();
        let mut next = self.totals;
        let bytes = next_live
            .remove(&(kind, id))
            .ok_or(GpuError::UnknownId { kind: "owned object", id })?;
        next.bytes -= bytes;
        next.objects -= 1;
        if kind == KIND_PIPELINE {
            next.compiled_bytes -= bytes;
        }
        self.commit(next_live, next)?;
        Ok(bytes)
    }

    /// Cumulative bytes resident on this connection (buffers, textures, samplers, shaders, pipelines,
    /// bind groups, surfaces, fences, external allocations, and transferred-in objects).
    pub fn residency_bytes(&self) -> u64 {
        self.totals.bytes
    }

    /// Cumulative live object count charged to this connection.
    pub fn object_count(&self) -> u64 {
        self.totals.objects
    }

    /// Bytes of this connection's residency attributable to the compiled-pipeline cache.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.totals.compiled_bytes
    }

    /// Validate the proposed connection totals against per-connection + compiled-cache + process-global
    /// ceilings and commit them atomically. On any rejection neither `self` nor the global account moves.
    fn commit(&mut self, next_live: HashMap<(u8, u32), u64>, next: Totals) -> Result<()> {
        if next.bytes > self.limits.max_connection_bytes || next.objects > self.limits.max_connection_objects {
            return Err(GpuError::ResourceLimit("connection residency"));
        }
        if next.compiled_bytes > self.limits.max_compiled_cache_bytes {
            return Err(GpuError::ResourceLimit("compiled cache"));
        }
        let mut global = self.global.inner.lock().unwrap_or_else(|e| e.into_inner());
        let without_self = Totals {
            bytes: global.bytes.saturating_sub(self.totals.bytes),
            objects: global.objects.saturating_sub(self.totals.objects),
            compiled_bytes: 0,
        };
        let proposed = Totals {
            bytes: without_self.bytes.checked_add(next.bytes).ok_or(GpuError::ResourceLimit("global residency overflow"))?,
            objects: without_self.objects.checked_add(next.objects).ok_or(GpuError::ResourceLimit("global object overflow"))?,
            compiled_bytes: 0,
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

// Residency accounting kinds (the first tuple element of the `(kind, id)` live-object key). Distinct
// per resource class so a buffer id and a surface id sharing a numeric value are charged separately.
const KIND_BUFFER: u8 = 1;
const KIND_TEXTURE: u8 = 2;
const KIND_SAMPLER: u8 = 3;
const KIND_SHADER: u8 = 4;
const KIND_PIPELINE: u8 = 5;
const KIND_BIND_GROUP: u8 = 6;
const KIND_SURFACE: u8 = 7;
const KIND_FENCE: u8 = 8;
const KIND_EXTERNAL: u8 = 9;

/// Fixed residency charged for a timeline fence's host-side signal/wait state.
const FENCE_BYTES: u64 = 128;

fn create_charge(cmd: &Cmd) -> Result<Option<(u8, u32, u64)>> {
    Ok(match cmd {
        Cmd::CreateBuffer(id, d) => Some((KIND_BUFFER, *id, d.size)),
        Cmd::CreateTexture(id, d) => Some((KIND_TEXTURE, *id, texture_bytes(d)?)),
        Cmd::CreateSampler(id, _) => Some((KIND_SAMPLER, *id, 64)),
        Cmd::CreateShader { id, spirv, .. } => Some((KIND_SHADER, *id, (spirv.len() as u64).saturating_mul(4))),
        // A created pipeline's charge is its compiled-cache (PSO/AIR) footprint; `preflight` also meters
        // it against the negotiated per-connection compiled-cache ceiling.
        Cmd::CreateRenderPipeline(id, _) | Cmd::CreateComputePipeline(id, _) => Some((KIND_PIPELINE, *id, 4096)),
        Cmd::CreateBindGroup(id, d) => {
            let referenced = d.entries.iter().map(|e| match e.resource {
                BindResource::Buffer { size, .. } => size,
                _ => 64,
            }).sum::<u64>();
            Some((KIND_BIND_GROUP, *id, 256u64.saturating_add(referenced.min(4096))))
        }
        // A presentable surface pins one full-frame render target resident on the executor.
        Cmd::CreateSurface(id, d) => Some((KIND_SURFACE, *id, surface_bytes(d))),
        // A timeline fence pins a small amount of host-side signal state.
        Cmd::CreateFence(id) => Some((KIND_FENCE, *id, FENCE_BYTES)),
        _ => None,
    })
}

fn destroy_key(cmd: &Cmd) -> Option<(u8, u32)> {
    match cmd {
        Cmd::DestroyBuffer(id) => Some((KIND_BUFFER, *id)),
        Cmd::DestroyTexture(id) => Some((KIND_TEXTURE, *id)),
        Cmd::DestroySampler(id) => Some((KIND_SAMPLER, *id)),
        Cmd::DestroyShader(id) => Some((KIND_SHADER, *id)),
        Cmd::DestroyPipeline(id) => Some((KIND_PIPELINE, *id)),
        Cmd::DestroyBindGroup(id) => Some((KIND_BIND_GROUP, *id)),
        Cmd::DestroySurface(id) => Some((KIND_SURFACE, *id)),
        Cmd::DestroyFence(id) => Some((KIND_FENCE, *id)),
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
        ReplayLimits {
            caps,
            max_connection_bytes: connection_bytes,
            max_connection_objects: connection_objects,
            copy_alignment: 4,
            max_compiled_cache_bytes: u64::MAX,
        }
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
        assert_eq!(budget.totals, Totals { bytes: 16, objects: 1, compiled_bytes: 0 });
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
            sample_count: 1,
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

    fn texture_desc(dim: crate::ir::TextureDim) -> TextureDesc {
        TextureDesc {
            width: 4,
            height: 4,
            depth: 3,
            mip_levels: 3,
            sample_count: 1,
            dim,
            format: crate::ir::TextureFormat::Rgba8Unorm,
            usage: 0,
            label: String::new(),
        }
    }

    #[test]
    fn d2_array_layers_stay_constant_while_d3_depth_halves_per_mip() {
        let d2 = texture_desc(crate::ir::TextureDim::D2);
        assert_eq!(texture_bytes(&d2).unwrap(), (4 * 4 + 2 * 2 + 1) * 3 * 4);

        let mut d3 = texture_desc(crate::ir::TextureDim::D3);
        d3.depth = 4;
        assert_eq!(texture_bytes(&d3).unwrap(), (4 * 4 * 4 + 2 * 2 * 2 + 1) * 4);

        let mut msaa = texture_desc(crate::ir::TextureDim::D2);
        msaa.depth = 1;
        msaa.sample_count = 4;
        assert_eq!(texture_bytes(&msaa).unwrap(), (4 * 4 + 2 * 2 + 1) * 4 * 4);
    }

    #[test]
    fn zero_and_invalid_texture_shapes_are_rejected_before_charge() {
        let base = texture_desc(crate::ir::TextureDim::D2);
        for (mut desc, expected) in [
            ({ let mut d = base.clone(); d.width = 0; d }, "zero texture dimension"),
            ({ let mut d = base.clone(); d.height = 0; d }, "zero texture dimension"),
            ({ let mut d = base.clone(); d.depth = 0; d }, "zero texture dimension"),
            ({ let mut d = base.clone(); d.mip_levels = 0; d }, "zero texture mip levels"),
            ({ let mut d = base.clone(); d.sample_count = 0; d }, "zero texture sample count"),
            ({ let mut d = base.clone(); d.mip_levels = 4; d }, "texture mip levels exceed dimensions"),
            ({ let mut d = base.clone(); d.dim = crate::ir::TextureDim::D1; d }, "invalid 1D texture shape"),
            ({ let mut d = base.clone(); d.dim = crate::ir::TextureDim::Cube; d.depth = 5; d }, "invalid cube texture shape"),
        ] {
            assert_eq!(texture_bytes(&desc), Err(GpuError::ResourceLimit(expected)));
            desc.label.clear();
        }
    }

    #[test]
    fn texture_global_exact_boundary_and_plus_one_are_atomic() {
        let mut l = limits(1024, 4);
        l.caps.max_buffer_bytes = 1024;
        let global = GlobalBudget::new(252, 4);
        let mut exact = ExecutorBudget::new(l.clone(), global.clone());
        exact.preflight(64, &[Cmd::CreateTexture(1, texture_desc(crate::ir::TextureDim::D2))])
            .expect("exact global texture byte boundary");

        let mut over = ExecutorBudget::new(l, global);
        let before = over.totals;
        assert_eq!(
            over.preflight(64, &[Cmd::CreateBuffer(2, BufferDesc {
                size: 1,
                usage: buffer_usage::COPY_DST,
                label: String::new(),
            })]),
            Err(GpuError::ResourceLimit("global residency"))
        );
        assert_eq!(over.totals, before, "global +1 rejection did not partially charge connection");
        assert_eq!(exact.totals, Totals { bytes: 252, objects: 1, compiled_bytes: 0 });
    }

    // ---- Row 2: negotiated backend alignment + compiled-cache limits enforced before decode/alloc ----

    fn compute_pipeline() -> crate::ir::ComputePipelineDesc {
        crate::ir::ComputePipelineDesc {
            compute: crate::ir::ShaderRef { module: 1, entry: "main".into() },
            label: String::new(),
        }
    }

    #[test]
    fn misaligned_copy_is_rejected_before_charge_and_before_the_backend() {
        use crate::ir::{CommandBuffer, Enc};
        let mut l = limits(u64::MAX, 16);
        l.caps.max_frame_bytes = 1 << 20;
        l.copy_alignment = 4;
        let global = GlobalBudget::new(u64::MAX, 64);
        let mut budget = ExecutorBudget::new(l, global);

        // A buffer-copy whose src_offset is not a multiple of the negotiated alignment is refused.
        let misaligned = Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 3, dst: 2, dst_offset: 0, size: 4 }],
            signal: None,
        });
        assert_eq!(budget.preflight(64, &[misaligned.clone()]), Err(GpuError::ResourceLimit("copy alignment")));
        assert_eq!(budget.totals, Totals::default(), "a misaligned copy must not charge residency");

        // The same rejection short-circuits the full limited replay before the backend is ever touched.
        let wire = crate::ir::encode_stream(&[misaligned]);
        let mut backend = crate::mock::RecordingBackend::new();
        assert_eq!(
            crate::replay::replay_stream_limited(&mut backend, &wire, &mut budget),
            Err(GpuError::ResourceLimit("copy alignment"))
        );
        assert!(backend.log.is_empty(), "misaligned copy reached the backend");

        // A misaligned image-copy row stride (`bytes_per_row`) is likewise rejected…
        let bad_row = Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1, src_offset: 0, bytes_per_row: 30, dst: 2, mip: 0, width: 8, height: 4,
            }],
            signal: None,
        });
        assert_eq!(budget.preflight(64, &[bad_row]), Err(GpuError::ResourceLimit("copy alignment")));

        // …while a fully-aligned copy passes validation (it then charges nothing — no create/destroy).
        let aligned = Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 8, dst: 2, dst_offset: 24, size: 40 }],
            signal: None,
        });
        budget.preflight(64, &[aligned]).expect("aligned copy passes the negotiated alignment gate");
        assert_eq!(budget.totals, Totals::default());
    }

    #[test]
    fn compiled_cache_ceiling_is_enforced_before_allocating_a_pipeline_and_is_refunded() {
        // Room for exactly one 4096-byte compiled pipeline in the connection's compiled-cache budget.
        let mut l = limits(u64::MAX, 16);
        l.max_compiled_cache_bytes = 4096;
        let global = GlobalBudget::new(u64::MAX, 64);
        let mut budget = ExecutorBudget::new(l, global);

        budget.preflight(64, &[Cmd::CreateComputePipeline(1, compute_pipeline())]).expect("first pipeline fits");
        assert_eq!(budget.compiled_cache_bytes(), 4096);
        let before = budget.totals;

        // The second pipeline would double the compiled cache past the negotiated ceiling: rejected
        // atomically, before the executor compiles or allocates it.
        assert_eq!(
            budget.preflight(64, &[Cmd::CreateComputePipeline(2, compute_pipeline())]),
            Err(GpuError::ResourceLimit("compiled cache"))
        );
        assert_eq!(budget.totals, before, "a rejected pipeline must not charge the compiled cache");

        // Destroying the first pipeline refunds its compiled-cache footprint so a fresh one fits again.
        budget.preflight(32, &[Cmd::DestroyPipeline(1)]).expect("destroy refunds");
        assert_eq!(budget.compiled_cache_bytes(), 0);
        budget.preflight(64, &[Cmd::CreateComputePipeline(3, compute_pipeline())]).expect("refunded cache reused");
        assert_eq!(budget.compiled_cache_bytes(), 4096);
    }

    // ---- Row 3: cumulative residency also charges surfaces, fences, external allocs + ownership ----

    fn surface_desc(w: u32, h: u32) -> crate::ir::SurfaceDesc {
        crate::ir::SurfaceDesc {
            width: w,
            height: h,
            format: crate::ir::TextureFormat::Rgba8Unorm,
            hlp_surface: 1,
        }
    }

    #[test]
    fn surfaces_and_fences_are_charged_to_cumulative_residency_and_refunded() {
        let global = GlobalBudget::new(u64::MAX, 64);
        let mut budget = ExecutorBudget::new(limits(u64::MAX, 64), global);

        // A 4x3 RGBA8 surface (48 bytes) plus a fence (128 bytes) = 176 bytes across 2 objects.
        budget
            .preflight(64, &[Cmd::CreateSurface(10, surface_desc(4, 3)), Cmd::CreateFence(11)])
            .expect("surface + fence charge");
        assert_eq!(budget.residency_bytes(), 4 * 3 * 4 + FENCE_BYTES);
        assert_eq!(budget.object_count(), 2);

        // Destroying both refunds exactly — no leaked residency across create/destroy churn.
        budget.preflight(32, &[Cmd::DestroySurface(10), Cmd::DestroyFence(11)]).expect("refund");
        assert_eq!(budget.residency_bytes(), 0);
        assert_eq!(budget.object_count(), 0);
    }

    #[test]
    fn surface_and_fence_counts_are_bounded_by_the_connection_object_ceiling() {
        // Only two objects are allowed on the connection; a surface + fence exactly fills it, and a
        // third object is rejected atomically.
        let global = GlobalBudget::new(u64::MAX, 64);
        let mut budget = ExecutorBudget::new(limits(u64::MAX, 2), global);
        budget
            .preflight(64, &[Cmd::CreateSurface(1, surface_desc(2, 2)), Cmd::CreateFence(2)])
            .expect("two objects fit");
        let before = budget.totals;
        assert_eq!(
            budget.preflight(64, &[Cmd::CreateFence(3)]),
            Err(GpuError::ResourceLimit("connection residency"))
        );
        assert_eq!(budget.totals, before, "the over-count fence must not partially charge");
    }

    #[test]
    fn external_allocations_are_charged_released_and_bounded_per_connection_and_globally() {
        let global = GlobalBudget::new(4096, 8);
        let mut budget = ExecutorBudget::new(limits(4096, 8), global.clone());

        budget.charge_external_allocation(1, 1024).expect("import charges residency");
        assert_eq!((budget.residency_bytes(), budget.object_count()), (1024, 1));
        // A duplicate external id is rejected without changing the account.
        assert_eq!(budget.charge_external_allocation(1, 1), Err(GpuError::Invalid("duplicate external allocation id")));
        assert_eq!(budget.residency_bytes(), 1024);

        // A second connection cannot import past the shared global byte ceiling.
        let mut other = ExecutorBudget::new(limits(4096, 8), global);
        assert_eq!(other.charge_external_allocation(9, 4096), Err(GpuError::ResourceLimit("global residency")));

        // Release refunds exactly; releasing an unknown id is a typed error.
        budget.release_external_allocation(1).expect("release refunds");
        assert_eq!((budget.residency_bytes(), budget.object_count()), (0, 0));
        assert_eq!(
            budget.release_external_allocation(1),
            Err(GpuError::UnknownId { kind: "external allocation", id: 1 })
        );
    }

    #[test]
    fn ownership_transfer_moves_residency_between_connections() {
        // The surface's backing buffer starts resident on connection A, then its ownership is handed to
        // connection B (the compositor). The charge must move with it — not be double-counted or leaked.
        let global = GlobalBudget::new(u64::MAX, 64);
        let mut producer = ExecutorBudget::new(limits(u64::MAX, 64), global.clone());
        let mut compositor = ExecutorBudget::new(limits(u64::MAX, 64), global);

        producer.preflight(64, &[Cmd::CreateSurface(5, surface_desc(4, 4))]).expect("surface resident on A");
        let surface_charge = 4 * 4 * 4;
        assert_eq!(producer.residency_bytes(), surface_charge);

        // Hand ownership of the surface's backing to the compositor connection.
        let moved = producer.release_ownership_transfer(KIND_SURFACE, 5).expect("A releases ownership");
        assert_eq!(moved, surface_charge);
        assert_eq!((producer.residency_bytes(), producer.object_count()), (0, 0), "A no longer charged");
        compositor.accept_ownership_transfer(KIND_SURFACE, 5, moved).expect("B accepts ownership");
        assert_eq!((compositor.residency_bytes(), compositor.object_count()), (surface_charge, 1));

        // Transferring out an object that is not live here is a typed error, not a silent underflow.
        assert_eq!(
            producer.release_ownership_transfer(KIND_SURFACE, 5),
            Err(GpuError::UnknownId { kind: "owned object", id: 5 })
        );
    }
}
