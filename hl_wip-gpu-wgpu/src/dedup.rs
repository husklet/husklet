//! Content-dedup caches for `CreateShader` and `CreateRenderPipeline`.
//!
//! Chrome/Skia's offscreen frame re-issues the SAME shader source and the SAME render-pipeline descriptor
//! under fresh resource ids on every `glFlush` (the client recreates instead of reusing). Without dedup
//! each create materializes a NEW `wgpu::ShaderModule` / `RenderPipeline` and holds a full copy resident;
//! over hundreds of frames the executor pins N identical backings and the wgpu device runs out of room.
//!
//! This module keys each backing on its exact COMPILATION-DETERMINING content — a shader on
//! `(payload-kind, the exact payload words)`, a render pipeline on `(each stage's deduped shader identity +
//! entry point, plus every fixed-function state field)` — so a create whose content matches a live backing
//! ALIASES it: the new id gets a cheap clone of the shared `Arc`-backed wgpu handle and charges ~0
//! incremental residency. A refcount of live alias ids per backing keeps the backing resident until the
//! LAST alias is destroyed. Keys are compared by full value (exact `Vec<u32>` / derived `PartialEq`), never
//! by a lossy hash, so two genuinely different sources/descriptors never falsely share.
//!
//! **Transaction-consistency.** The runtime dispatches every batch inside an all-tables transaction
//! (`begin_txn` → `execute` → `commit_txn`/`rollback_txn`): if the executor's `execute` returns `Err` the
//! id tables roll back to the pre-batch state. These caches live OUTSIDE `SessionResources`, so they journal
//! every mutation within a batch and, on batch failure, replay the inverses — leaving the caches exactly as
//! they were before the batch, in lock-step with the rolled-back id tables. On success the journal is
//! cleared and any backing whose refcount reached zero is swept (its wgpu handle dropped, its residency
//! already released).

use std::collections::HashMap;

use hl_gpu::protocol::model::descriptor::{
    ColorTargetState, DepthState, RenderPipelineDesc, VertexLayout,
};
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};

use crate::reflect::Reflected;

/// Residency (bytes) charged for one unique compiled shader backing. Mirrors the runtime's `KIND_SHADER`
/// footprint estimate (`payload_words * 4`), so the executor-side residency of a deduped set of shaders is
/// "one payload's worth", not N.
pub fn shader_backing_bytes(words: &[u32]) -> u64 {
    (words.len() as u64).saturating_mul(4)
}

/// Residency (bytes) charged for one unique compiled render-pipeline backing. Mirrors the runtime's
/// `KIND_PIPELINE` compiled-cache footprint estimate (a flat 4096).
pub const PIPELINE_BACKING_BYTES: u64 = 4096;

/// The exact content identity of a shader payload: the payload kind plus the verbatim payload words. Two
/// `CreateShader`s share a backing iff this matches EXACTLY — full-value `Eq`, never a lossy hash, so no
/// two distinct sources can collide.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ShaderKey {
    pub kind: u8,
    pub words: Vec<u32>,
}

/// The full content identity of a render pipeline: each stage's deduped shader identity + entry point, plus
/// every fixed-function state field that changes the compiled pipeline. The shader identity is the stage
/// module's [`ShaderKey`] (its CONTENT), so two pipelines built from different shader IDS that carry the
/// same source dedup to one pipeline. `label` is intentionally excluded (cosmetic, does not affect the PSO).
/// Compared by derived `PartialEq` (exact field equality), so no two distinct descriptors falsely share.
#[derive(Clone, PartialEq)]
pub struct RenderPipeKey {
    pub vertex: (ShaderKey, String),
    pub fragment: Option<(ShaderKey, String)>,
    pub vertex_buffers: Vec<VertexLayout>,
    pub color_targets: Vec<ColorTargetState>,
    pub depth: Option<DepthState>,
    pub topology: Topology,
    pub cull: u32,
    pub front_face: u32,
    pub sample_count: u32,
}

impl RenderPipeKey {
    /// Build the key from a pipeline descriptor and the already-resolved content keys of its stages.
    pub fn from_desc(
        desc: &RenderPipelineDesc,
        vertex_key: ShaderKey,
        fragment_key: Option<ShaderKey>,
    ) -> Self {
        RenderPipeKey {
            vertex: (vertex_key, desc.vertex.entry.clone()),
            fragment: desc
                .fragment
                .as_ref()
                .zip(fragment_key)
                .map(|(f, k)| (k, f.entry.clone())),
            vertex_buffers: desc.vertex_buffers.clone(),
            color_targets: desc.color_targets.clone(),
            depth: desc.depth.clone(),
            topology: desc.topology,
            cull: desc.cull,
            front_face: desc.front_face,
            sample_count: desc.sample_count,
        }
    }
}

/// One compiled shader backing shared by every id aliasing this content.
struct ShaderBacking {
    module: wgpu::ShaderModule,
    reflected: Reflected,
    bytes: u64,
    /// Live alias ids referencing this backing. The backing is resident (its bytes counted) while > 0.
    refcount: u32,
}

/// One compiled render-pipeline backing shared by every id aliasing this descriptor.
struct PipelineBacking {
    key: RenderPipeKey,
    pipeline: wgpu::RenderPipeline,
    color_formats: Vec<TextureFormat>,
    used_bindings: Vec<(u32, u32)>,
    bytes: u64,
    refcount: u32,
}

/// The inverse of one cache mutation, recorded for transactional rollback of a failed batch.
enum Undo {
    ShaderAcquire(ShaderKey),
    ShaderInstall(ShaderKey),
    ShaderRelease(ShaderKey),
    PipelineAcquire(u64),
    PipelineInstall(u64),
    PipelineRelease(u64),
}

/// The executor's content-dedup state: the shader + pipeline backing caches, the running residency
/// counters, and the per-batch undo journal.
#[derive(Default)]
pub struct DedupCaches {
    shaders: HashMap<ShaderKey, ShaderBacking>,
    pipelines: HashMap<u64, PipelineBacking>,
    next_pipeline_id: u64,
    shader_bytes: u64,
    pipeline_bytes: u64,
    journal: Vec<Undo>,
}

impl DedupCaches {
    // ---- residency accessors (observed by tests / diagnostics) -------------------------------------

    /// Bytes of shader-backing residency currently held (one backing's worth per UNIQUE live source, not
    /// per alias id).
    pub fn shader_resident_bytes(&self) -> u64 {
        self.shader_bytes
    }

    /// Bytes of render-pipeline-backing residency currently held (one backing's worth per UNIQUE live
    /// descriptor, not per alias id).
    pub fn pipeline_resident_bytes(&self) -> u64 {
        self.pipeline_bytes
    }

    /// Total deduped backing residency (shaders + pipelines).
    pub fn resident_bytes(&self) -> u64 {
        self.shader_bytes.saturating_add(self.pipeline_bytes)
    }

    /// Count of distinct live shader backings (unique compiled modules).
    pub fn shader_backing_count(&self) -> usize {
        self.shaders.values().filter(|e| e.refcount > 0).count()
    }

    /// Count of distinct live render-pipeline backings (unique compiled pipelines).
    pub fn pipeline_backing_count(&self) -> usize {
        self.pipelines.values().filter(|e| e.refcount > 0).count()
    }

    // ---- batch transaction hooks -------------------------------------------------------------------

    /// Start a fresh batch: clear the undo journal (the previous batch was already committed or rolled
    /// back).
    pub fn begin_batch(&mut self) {
        self.journal.clear();
    }

    /// Commit the batch: drop the journal and sweep any backing whose last alias was destroyed this batch
    /// (its residency was already released when the refcount hit zero).
    pub fn commit_batch(&mut self) {
        self.journal.clear();
        self.shaders.retain(|_, e| e.refcount > 0);
        self.pipelines.retain(|_, e| e.refcount > 0);
    }

    /// Roll the batch back: replay every recorded inverse in reverse order, restoring the caches and the
    /// residency counters to the exact pre-batch state (mirrors the id tables' `rollback_txn`).
    pub fn rollback_batch(&mut self) {
        while let Some(u) = self.journal.pop() {
            match u {
                Undo::ShaderAcquire(k) => {
                    if let Some(e) = self.shaders.get_mut(&k) {
                        if e.refcount > 0 {
                            e.refcount -= 1;
                            if e.refcount == 0 {
                                self.shader_bytes = self.shader_bytes.saturating_sub(e.bytes);
                            }
                        }
                    }
                }
                Undo::ShaderInstall(k) => {
                    if let Some(e) = self.shaders.remove(&k) {
                        if e.refcount > 0 {
                            self.shader_bytes = self.shader_bytes.saturating_sub(e.bytes);
                        }
                    }
                }
                Undo::ShaderRelease(k) => {
                    if let Some(e) = self.shaders.get_mut(&k) {
                        if e.refcount == 0 {
                            self.shader_bytes = self.shader_bytes.saturating_add(e.bytes);
                        }
                        e.refcount += 1;
                    }
                }
                Undo::PipelineAcquire(id) => {
                    if let Some(e) = self.pipelines.get_mut(&id) {
                        if e.refcount > 0 {
                            e.refcount -= 1;
                            if e.refcount == 0 {
                                self.pipeline_bytes = self.pipeline_bytes.saturating_sub(e.bytes);
                            }
                        }
                    }
                }
                Undo::PipelineInstall(id) => {
                    if let Some(e) = self.pipelines.remove(&id) {
                        if e.refcount > 0 {
                            self.pipeline_bytes = self.pipeline_bytes.saturating_sub(e.bytes);
                        }
                    }
                }
                Undo::PipelineRelease(id) => {
                    if let Some(e) = self.pipelines.get_mut(&id) {
                        if e.refcount == 0 {
                            self.pipeline_bytes = self.pipeline_bytes.saturating_add(e.bytes);
                        }
                        e.refcount += 1;
                    }
                }
            }
        }
    }

    // ---- shader cache ------------------------------------------------------------------------------

    /// Look for a live shader backing matching `key`. On a hit, register one more alias (bump refcount,
    /// re-count residency if the backing had been released this batch) and return a cheap clone of the
    /// shared module + its reflection for the new id. On a miss, returns `None` (the caller compiles then
    /// calls [`shader_install`](Self::shader_install)).
    pub fn shader_get(&mut self, key: &ShaderKey) -> Option<(wgpu::ShaderModule, Reflected)> {
        let e = self.shaders.get_mut(key)?;
        if e.refcount == 0 {
            self.shader_bytes = self.shader_bytes.saturating_add(e.bytes);
        }
        e.refcount += 1;
        let out = (e.module.clone(), e.reflected.clone());
        self.journal.push(Undo::ShaderAcquire(key.clone()));
        Some(out)
    }

    /// Install a freshly-compiled shader backing (refcount 1) and charge its residency. Called on a cache
    /// miss AFTER the new id was inserted into the id table.
    pub fn shader_install(
        &mut self,
        key: ShaderKey,
        module: wgpu::ShaderModule,
        reflected: Reflected,
        bytes: u64,
    ) {
        self.shader_bytes = self.shader_bytes.saturating_add(bytes);
        self.shaders.insert(key.clone(), ShaderBacking { module, reflected, bytes, refcount: 1 });
        self.journal.push(Undo::ShaderInstall(key));
    }

    /// Release one alias of a shader backing (a `DestroyShader`). Drops residency only when the last alias
    /// is gone; the backing itself is swept at commit.
    pub fn shader_release(&mut self, key: &ShaderKey) {
        if let Some(e) = self.shaders.get_mut(key) {
            if e.refcount == 0 {
                return;
            }
            e.refcount -= 1;
            if e.refcount == 0 {
                self.shader_bytes = self.shader_bytes.saturating_sub(e.bytes);
            }
            self.journal.push(Undo::ShaderRelease(key.clone()));
        }
    }

    // ---- pipeline cache ----------------------------------------------------------------------------

    /// Look for a live render-pipeline backing whose descriptor matches `key` (exact full-value compare).
    /// On a hit, register one more alias and return a cheap clone of the shared pipeline plus its cached
    /// draw-time metadata and the backing id (stored on the aliasing pipeline for release). On a miss,
    /// returns `None`.
    #[allow(clippy::type_complexity)]
    pub fn pipeline_get(
        &mut self,
        key: &RenderPipeKey,
    ) -> Option<(wgpu::RenderPipeline, Vec<TextureFormat>, Vec<(u32, u32)>, u64)> {
        let id = self
            .pipelines
            .iter()
            .find(|(_, e)| e.refcount > 0 && &e.key == key)
            .map(|(id, _)| *id)?;
        let e = self.pipelines.get_mut(&id).expect("id just found");
        e.refcount += 1;
        let out = (e.pipeline.clone(), e.color_formats.clone(), e.used_bindings.clone(), id);
        self.journal.push(Undo::PipelineAcquire(id));
        Some(out)
    }

    /// Install a freshly-built render-pipeline backing (refcount 1), charge its residency, and return the
    /// new backing id (stored on the aliasing pipeline so a later destroy can release it).
    pub fn pipeline_install(
        &mut self,
        key: RenderPipeKey,
        pipeline: wgpu::RenderPipeline,
        color_formats: Vec<TextureFormat>,
        used_bindings: Vec<(u32, u32)>,
        bytes: u64,
    ) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = self.next_pipeline_id.wrapping_add(1);
        self.pipeline_bytes = self.pipeline_bytes.saturating_add(bytes);
        self.pipelines.insert(
            id,
            PipelineBacking { key, pipeline, color_formats, used_bindings, bytes, refcount: 1 },
        );
        self.journal.push(Undo::PipelineInstall(id));
        id
    }

    /// Release one alias of a render-pipeline backing (a `DestroyPipeline` of a deduped render pipeline).
    pub fn pipeline_release(&mut self, backing_id: u64) {
        if let Some(e) = self.pipelines.get_mut(&backing_id) {
            if e.refcount == 0 {
                return;
            }
            e.refcount -= 1;
            if e.refcount == 0 {
                self.pipeline_bytes = self.pipeline_bytes.saturating_sub(e.bytes);
            }
            self.journal.push(Undo::PipelineRelease(backing_id));
        }
    }
}
