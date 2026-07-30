use super::*;

/// The wgpu-native backing of one protocol pipeline.
pub enum PipelineNative {
    Render {
        pipeline: wgpu::RenderPipeline,
        /// Exact protocol layouts retained for draw-time range diagnostics. The native pipeline does not
        /// expose this descriptor after creation.
        vertex_buffers: Vec<VertexLayout>,
        /// The color-target formats the pipeline was built for — retained for draw-time attachment
        /// compatibility checks (the CPU oracle rejects a format mismatch); the frozen suite's single
        /// target already matches, so it is not yet consulted.
        color_formats: Vec<TextureFormat>,
        /// The `(group, binding)` slots this pipeline's shaders actually READ — the union of its vertex +
        /// fragment entry points' usage ([`crate::reflect`]), which is exactly the set the EXPLICIT pipeline
        /// layout exposes (that layout is built from the same merge). A bind group `submit` builds is
        /// FILTERED to these bindings so the GL driver's per-bound-resource entries (which routinely include
        /// textures/samplers the compiled shader never samples) match the layout's set instead of NACKing
        /// (5-vs-3). Empty ⇒ no filtering (a bindingless pipeline, e.g. the conformance triangle).
        used_bindings: Vec<(u32, u32)>,
        /// The dedup-cache backing id this render pipeline aliases. Identical descriptors share one
        /// compiled `wgpu::RenderPipeline`; this is the handle a `DestroyPipeline` releases so the backing
        /// is freed only when its last alias is gone (see [`crate::dedup`]).
        backing: u64,
    },
    /// A compute pipeline. Both the PTX-kernel ABI path (built with an *explicit* group-0 layout so a
    /// binding the WGSL doesn't read — e.g. a kernel's `params` blob — is still declared) and the SPIR-V/
    /// GLSL path (built with wgpu's *auto* layout, which derives the bind-group layouts + push-constant
    /// range from the module) store just the pipeline: at dispatch the concrete per-group layout is taken
    /// from the pipeline itself via `get_bind_group_layout(index)`, which returns the explicit layout for
    /// the kernel path and the auto-derived one for the SPIR-V path — so a bind group built against it
    /// matches in both cases, and 2+ groups bind at their declared indices.
    Compute {
        pipeline: wgpu::ComputePipeline,
        /// Guest group-zero bindings were shifted to reserve the host viewport slot during shader lowering.
        remap_group_zero: bool,
    },
}

/// Downcast a live pipeline id to its native handle.
impl PipelineNative {
    pub fn get(res: &SessionResources, id: u32) -> Result<&PipelineNative> {
        res.pipelines
            .get(id)?
            .downcast_ref::<PipelineNative>()
            .ok_or(GpuError::Invalid("wgpu: pipeline native type mismatch"))
    }
}
