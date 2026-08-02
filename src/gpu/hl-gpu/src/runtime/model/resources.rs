//! The per-connection resource ownership + residency-cost data the runtime owns.
//!
//! Two responsibilities live here, both pure data + arithmetic (no workflow — that is `service/`):
//!
//! * [`SessionResources`] — the singular owner of protocol id → executor-native handle. One
//!   [`ResourceTable`] per resource kind; a [`GpuExecutor`](crate::runtime::port::executor::GpuExecutor)
//!   stores its own native object (a `Box<dyn Any>`) behind each entry, so lifecycle checking (duplicate
//!   create / use-after-free / double-free) is enforced once, uniformly, by the protocol table while the
//!   native type stays private to whichever executor (CPU / wgpu) is injected.
//! * the residency-cost model — [`texture_bytes`], [`surface_bytes`], [`create_charge`], [`destroy_key`]
//!   and the per-connection [`Ledger`] / shared [`GlobalLedger`] accounting *state*. The transactional
//!   charge/commit *workflow* over this state is `service/account.rs`.
//!
//! Ported from `hl-gpu/src/limits.rs` (the `texture_bytes`/`surface_bytes`/`create_charge`/`destroy_key`
//! math + the `Totals`/`GlobalBudget` accounting shape), split so the state lives in `model/` and the
//! transaction lives in `service/`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::protocol::model::command::Cmd;
use crate::protocol::model::descriptor::{BindResource, SurfaceDesc, TextureDesc};
use crate::protocol::model::enums::TextureDim;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{
    BindGroupId, BufferId, FenceId, PipelineId, ResourceTable, SamplerId, ShaderId, SurfaceId,
    TextureId,
};

// ---------------------------------------------------------------------------------------------------
// SessionResources — protocol ids → executor-native handles
// ---------------------------------------------------------------------------------------------------

/// An executor's native object for one resource, stored type-erased behind its protocol id. The CPU
/// reference executor puts its own buffer/texture struct here; a wgpu executor puts a `wgpu::Buffer`;
/// the runtime neither knows nor names the concrete type — it only enforces the id lifecycle.
pub type Native = Box<dyn Any>;

/// The singular per-connection owner of `protocol id → executor-native handle`, one table per kind.
///
/// The injected [`GpuExecutor`](crate::runtime::port::executor::GpuExecutor) mutates this in `execute`:
/// on a create it inserts its native object (an id already live is a typed `DuplicateId`), on a destroy
/// it removes it (an unknown id is a typed `UnknownId`). Because the table is the *only* id map, a
/// use-after-free becomes a clean `GpuError` instead of undefined behaviour on a real driver.
pub struct SessionResources {
    pub buffers: ResourceTable<Native>,
    pub textures: ResourceTable<Native>,
    pub samplers: ResourceTable<Native>,
    pub shaders: ResourceTable<Native>,
    pub pipelines: ResourceTable<Native>,
    pub bind_groups: ResourceTable<Native>,
    pub surfaces: ResourceTable<Native>,
    pub fences: ResourceTable<Native>,
}

impl SessionResources {
    pub fn new() -> Self {
        Self {
            buffers: ResourceTable::new(BufferId::KIND),
            textures: ResourceTable::new(TextureId::KIND),
            samplers: ResourceTable::new(SamplerId::KIND),
            shaders: ResourceTable::new(ShaderId::KIND),
            pipelines: ResourceTable::new(PipelineId::KIND),
            bind_groups: ResourceTable::new(BindGroupId::KIND),
            surfaces: ResourceTable::new(SurfaceId::KIND),
            fences: ResourceTable::new(FenceId::KIND),
        }
    }

    /// Begin an all-tables transaction so a whole batch's resource-lifecycle mutations (the executor's
    /// creates/destroys during `execute`) can be rolled back as a unit if the frame NACKs. Paired with
    /// [`commit_txn`](Self::commit_txn) on success or [`rollback_txn`](Self::rollback_txn) on failure.
    pub fn begin_txn(&mut self) {
        self.buffers.begin();
        self.textures.begin();
        self.samplers.begin();
        self.shaders.begin();
        self.pipelines.begin();
        self.bind_groups.begin();
        self.surfaces.begin();
        self.fences.begin();
    }

    /// Commit the in-flight all-tables transaction: every destroyed native parked during the batch is
    /// freed for real. Call after the executor accepted the whole batch.
    pub fn commit_txn(&mut self) {
        self.buffers.commit();
        self.textures.commit();
        self.samplers.commit();
        self.shaders.commit();
        self.pipelines.commit();
        self.bind_groups.commit();
        self.surfaces.commit();
        self.fences.commit();
    }

    /// Roll back the in-flight all-tables transaction: every id created during the batch is dropped and
    /// every id destroyed during the batch is restored, leaving the tables EXACTLY as they were before the
    /// frame. Call when the executor NACKed (failed mid-batch) so a retry / subsequent destroy is consistent.
    pub fn rollback_txn(&mut self) {
        self.buffers.rollback();
        self.textures.rollback();
        self.samplers.rollback();
        self.shaders.rollback();
        self.pipelines.rollback();
        self.bind_groups.rollback();
        self.surfaces.rollback();
        self.fences.rollback();
    }

    /// Total live objects across every kind — a cheap invariant check for tests/diagnostics.
    pub fn live_count(&self) -> usize {
        self.buffers.len()
            + self.textures.len()
            + self.samplers.len()
            + self.shaders.len()
            + self.pipelines.len()
            + self.bind_groups.len()
            + self.surfaces.len()
            + self.fences.len()
    }
}

impl Default for SessionResources {
    fn default() -> Self {
        Self::new()
    }
}

mod account;
pub use account::{Account, GlobalLedger, Ledger, Totals};

// ---------------------------------------------------------------------------------------------------
// residency-cost model (charge kinds + byte-footprint math)
// ---------------------------------------------------------------------------------------------------

// Residency accounting kinds (the first tuple element of the `(kind, id)` live-object key). Distinct per
// resource class so a buffer id and a surface id sharing a numeric value are charged separately.
pub const KIND_BUFFER: u8 = 1;
pub const KIND_TEXTURE: u8 = 2;
pub const KIND_SAMPLER: u8 = 3;
pub const KIND_SHADER: u8 = 4;
pub const KIND_PIPELINE: u8 = 5;
pub const KIND_BIND_GROUP: u8 = 6;
pub const KIND_SURFACE: u8 = 7;
pub const KIND_FENCE: u8 = 8;
pub const KIND_EXTERNAL: u8 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    Buffer,
    Texture,
    Sampler,
    Shader,
    Pipeline,
    BindGroup,
    Surface,
    Fence,
    External,
    Unknown,
}

impl ResourceKind {
    pub fn from_code(code: u8) -> Self {
        match code {
            KIND_BUFFER => Self::Buffer,
            KIND_TEXTURE => Self::Texture,
            KIND_SAMPLER => Self::Sampler,
            KIND_SHADER => Self::Shader,
            KIND_PIPELINE => Self::Pipeline,
            KIND_BIND_GROUP => Self::BindGroup,
            KIND_SURFACE => Self::Surface,
            KIND_FENCE => Self::Fence,
            KIND_EXTERNAL => Self::External,
            _ => Self::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Buffer => BufferId::KIND,
            Self::Texture => TextureId::KIND,
            Self::Sampler => SamplerId::KIND,
            Self::Shader => ShaderId::KIND,
            Self::Pipeline => PipelineId::KIND,
            Self::BindGroup => BindGroupId::KIND,
            Self::Surface => SurfaceId::KIND,
            Self::Fence => FenceId::KIND,
            Self::External => "external allocation",
            Self::Unknown => "resource",
        }
    }
}

/// Fixed residency charged for a timeline fence's host-side signal/wait state.
pub const FENCE_BYTES: u64 = 128;

/// The human-readable resource-kind tag for an accounting `kind` byte — the SAME string the executor's
/// [`ResourceTable`] carries for that kind, so a `DuplicateId`/`UnknownId` the account raises is
/// byte-identical to the one the executor would raise for the same id. `KIND_EXTERNAL` has no protocol
/// resource table (it is an imported allocation, not an IR-created object), so it names itself.
/// Backing residency (bytes) an executor keeps resident for a presentable surface: one full-frame render
/// target at the surface's format footprint.
impl SurfaceDesc {
    pub(crate) fn residency_bytes(&self) -> u64 {
        // `bytes_per_texel` answers None for depth/stencil and for every block-compressed format, so
        // taking 4 here invents a footprint for exactly the formats it could not describe. The texture
        // path below already found this and special-cased Depth24PlusStencil8 as an 8-byte plane —
        // "charging the 4-byte default undercounted it by 2x" — but the surface path kept the bare
        // default, so the same format was charged 4 as a surface and 8 as a texture. Measured at
        // 256x256: depth24+stencil8 undercharged 2x, and BC1 overcharged 8x against its half-byte
        // texel. Nothing validates a surface's format, so both are reachable.
        //
        // Undercharging is the direction that matters: residency limits stop reflecting what the
        // executor actually keeps resident.
        let texel = self.format.footprint_bytes_per_texel() as u64;
        (self.width as u64)
            .saturating_mul(self.height as u64)
            .saturating_mul(texel)
    }
}

/// Full mip-chain byte footprint of a texture, also validating its shape (zero/oversized dims, mip count
/// vs. extent, dimensionality-specific constraints) — a malformed descriptor is a typed `ResourceLimit`
/// rejection before any charge or executor allocation.
impl TextureDesc {
    pub(crate) fn residency_bytes(&self) -> Result<u64> {
        let d = self;
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
            TextureDim::Cube
                if d.width != d.height || !d.depth.is_multiple_of(6) || d.sample_count != 1 =>
            {
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
            return Err(GpuError::ResourceLimit(
                "texture mip levels exceed dimensions",
            ));
        }
        // Depth/stencil formats report `bytes_per_texel() == None` (the software backend can't clear-fill
        // them) but for footprint accounting they occupy a real target; charge it instead of rejecting a
        // valid depth render target. The per-format footprint lives in `footprint_bytes_per_texel` so
        // the surface charge path cannot disagree with this one about the same format.
        let texel = d.format.footprint_bytes_per_texel() as u64;
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
            total = total
                .checked_add(level)
                .ok_or(GpuError::ResourceLimit("texture footprint overflow"))?;
            w >>= 1;
            h >>= 1;
            if d.dim == TextureDim::D3 {
                depth >>= 1;
            }
        }
        Ok(total)
    }
}

/// The residency charge a `Create*` command incurs: `(kind, id, bytes)`, or `None` for a non-charging
/// command. Errors only when a texture descriptor is malformed (via [`texture_bytes`]).
impl Cmd {
    pub(crate) fn create_charge(&self) -> Result<Option<(u8, u32, u64)>> {
        Ok(match self {
            Cmd::CreateBuffer(id, d) => Some((KIND_BUFFER, *id, d.size)),
            Cmd::CreateTexture(id, d) => Some((KIND_TEXTURE, *id, d.residency_bytes()?)),
            Cmd::CreateTextureView(id, _) => Some((KIND_TEXTURE, *id, 64)),
            Cmd::CreateSampler(id, _) => Some((KIND_SAMPLER, *id, 64)),
            Cmd::CreateShader { id, spirv, .. } => {
                Some((KIND_SHADER, *id, (spirv.len() as u64).saturating_mul(4)))
            }
            // A pipeline's charge is its compiled-cache (PSO/AIR) footprint; the account service also meters
            // it against the negotiated per-connection compiled-cache ceiling.
            Cmd::CreateRenderPipeline(id, _)
            | Cmd::CreateComputePipeline(id, _)
            | Cmd::CreateRenderPipelineLayout(id, _, _, _)
            | Cmd::CreateComputePipelineLayout(id, _, _) => Some((KIND_PIPELINE, *id, 4096)),
            Cmd::CreateBindGroup(id, d) => {
                let referenced = d
                    .entries
                    .iter()
                    .map(|e| match e.resource {
                        BindResource::Buffer { size, .. } => size,
                        _ => 64,
                    })
                    .sum::<u64>();
                Some((
                    KIND_BIND_GROUP,
                    *id,
                    256u64.saturating_add(referenced.min(4096)),
                ))
            }
            // A presentable surface pins one full-frame render target resident on the executor.
            Cmd::CreateSurface(id, d) => Some((KIND_SURFACE, *id, d.residency_bytes())),
            // A timeline fence pins a small amount of host-side signal state.
            Cmd::CreateFence(id) => Some((KIND_FENCE, *id, FENCE_BYTES)),
            _ => None,
        })
    }

    /// The `(kind, id)` a `Destroy*` command refunds, or `None` for a non-refunding command.
    pub(crate) fn destroy_key(&self) -> Option<(u8, u32)> {
        match self {
            Cmd::DestroyBuffer(id) => Some((KIND_BUFFER, *id)),
            Cmd::DestroyTexture(id) => Some((KIND_TEXTURE, *id)),
            Cmd::DestroyTextureView(id) => Some((KIND_TEXTURE, *id)),
            Cmd::DestroySampler(id) => Some((KIND_SAMPLER, *id)),
            Cmd::DestroyShader(id) => Some((KIND_SHADER, *id)),
            Cmd::DestroyPipeline(id) => Some((KIND_PIPELINE, *id)),
            Cmd::DestroyBindGroup(id) => Some((KIND_BIND_GROUP, *id)),
            Cmd::DestroySurface(id) => Some((KIND_SURFACE, *id)),
            Cmd::DestroyFence(id) => Some((KIND_FENCE, *id)),
            _ => None,
        }
    }
}
