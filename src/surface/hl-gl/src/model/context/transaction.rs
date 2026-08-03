use super::local::SurfaceTarget;
use super::*;

/// The IR residency state mutated while lowering one frame.
///
/// A command sink accepts a batch transactionally. Lowering must follow the same contract: if submission
/// fails, every allocation and cache insertion made while assembling that batch is restored so a retry
/// emits the resource-creation commands again instead of referencing host objects that never committed.
#[derive(Clone)]
pub struct FrameState {
    default_targets: HashMap<u64, SurfaceTarget>,
    external_targets: HashMap<(u32, u64), hl_gpu::SurfaceToken>,
    default_placeholder_tex: [u32; 3],
    default_placeholder_samp: u32,
    fbo_targets: HashMap<(u32, u64), (u32, u32)>,
    depth_targets: HashMap<DepthTargetKey, u32>,
    depth_target_current: HashMap<(u32, bool), (DepthTargetKey, u32)>,
    depth_aspect_current: HashMap<(u32, u64), u32>,
    stencil_aspect_current: HashMap<(u32, u64), u32>,
    tex_ir_cache: HashMap<u32, (u32, (u64, u64, bool))>,
    shared_tex_ir_cache: HashMap<(u64, u64, u32, u32, u32), SharedTextureResidency>,
    shared_target_cache: HashMap<u64, SharedTargetResidency>,
    buf_ir_cache: HashMap<(u32, u32), (u32, u64)>,
    prog_shader_cache: HashMap<(u32, u64), (u32, u32, u64)>,
    prog_pipeline_cache: HashMap<(u32, u64), (u32, u64)>,
    sampler_ir_cache: Vec<(hl_gpu::protocol::model::descriptor::SamplerDesc, u32)>,
    clear_shader_ir: HashMap<u32, (u32, u32)>,
    clear_pipeline_cache: HashMap<ClearPipelineKey, u32>,
    pending_destroys: Vec<Cmd>,
    transform_feedback_readbacks: Vec<TransformFeedbackReadback>,
    transform_feedback_cleanup: Vec<Cmd>,
}

impl GlContext {
    /// Snapshot the resource state that frame lowering may mutate.
    pub fn frame_state(&self) -> FrameState {
        // A new frame starts a new allocation ledger: only names issued from here on belong to the batch
        // this snapshot can roll back.
        self.frame_ledger().clear();
        FrameState {
            default_targets: self.local.default_targets.clone(),
            external_targets: self.external_targets.clone(),
            default_placeholder_tex: self.default_placeholder_tex,
            default_placeholder_samp: self.default_placeholder_samp,
            fbo_targets: self.fbo_targets.clone(),
            depth_targets: self.depth_targets.clone(),
            depth_target_current: self.depth_target_current.clone(),
            depth_aspect_current: self.depth_aspect_current.clone(),
            stencil_aspect_current: self.stencil_aspect_current.clone(),
            tex_ir_cache: self.tex_ir_cache.clone(),
            shared_tex_ir_cache: self.shared_tex_ir_cache.clone(),
            shared_target_cache: self.shared_target_cache.clone(),
            buf_ir_cache: self.buf_ir_cache.clone(),
            prog_shader_cache: self.prog_shader_cache.clone(),
            prog_pipeline_cache: self.prog_pipeline_cache.clone(),
            sampler_ir_cache: self.sampler_ir_cache.clone(),
            clear_shader_ir: self.clear_shader_ir.clone(),
            clear_pipeline_cache: self.clear_pipeline_cache.clone(),
            pending_destroys: self.pending_destroys.clone(),
            transform_feedback_readbacks: self.local.transform_feedback_readbacks.clone(),
            transform_feedback_cleanup: self.local.transform_feedback_cleanup.clone(),
        }
    }

    /// Restore a pre-lowering resource snapshot after the sink rejects the generated batch.
    pub fn restore_frame_state(&mut self, state: FrameState) {
        self.local.default_targets = state.default_targets;
        self.external_targets = state.external_targets;
        self.default_placeholder_tex = state.default_placeholder_tex;
        self.default_placeholder_samp = state.default_placeholder_samp;
        self.fbo_targets = state.fbo_targets;
        self.depth_targets = state.depth_targets;
        self.depth_target_current = state.depth_target_current;
        self.depth_aspect_current = state.depth_aspect_current;
        self.stencil_aspect_current = state.stencil_aspect_current;
        self.tex_ir_cache = state.tex_ir_cache;
        self.shared_tex_ir_cache = state.shared_tex_ir_cache;
        self.shared_target_cache = state.shared_target_cache;
        self.buf_ir_cache = state.buf_ir_cache;
        self.prog_shader_cache = state.prog_shader_cache;
        self.prog_pipeline_cache = state.prog_pipeline_cache;
        self.sampler_ir_cache = state.sampler_ir_cache;
        self.clear_shader_ir = state.clear_shader_ir;
        self.clear_pipeline_cache = state.clear_pipeline_cache;
        self.pending_destroys = state.pending_destroys;
        self.local.transform_feedback_readbacks = state.transform_feedback_readbacks;
        self.local.transform_feedback_cleanup = state.transform_feedback_cleanup;
        // Return every IR name this frame issued. hl-gpu rolls its own id tables back exactly to the
        // pre-frame state on a NACK, so none of these reached a live host object; reissuing them lets the
        // retry emit the identical resource-creation stream rather than leaking a name per rejection.
        let released: Vec<_> = self.frame_ledger().drain(..).collect();
        for (kind, id) in released {
            self.allocator.release(kind, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_frame_recreates_internal_clear_resources_on_retry() {
        let mut context = GlContext::new();
        let before = context.frame_state();
        let (vertex, fragment, create_shaders) = context.clear_shader_ir(1).unwrap();
        let key = ClearPipelineKey {
            color_formats: [1, 0, 0, 0],
            color_target_count: 1,
            depth_format: 2,
            color_write_masks: [0xf, 0, 0, 0],
            depth_write: true,
            stencil_write_mask: 0xff,
        };
        let (pipeline, create_pipeline) = context.clear_pipeline_ir(key).unwrap();
        assert!(create_shaders);
        assert!(create_pipeline);

        context.restore_frame_state(before);

        let (retry_vertex, retry_fragment, recreate_shaders) =
            context.clear_shader_ir(1).unwrap();
        let (retry_pipeline, recreate_pipeline) = context.clear_pipeline_ir(key).unwrap();
        assert_eq!((retry_vertex, retry_fragment), (vertex, fragment));
        assert_eq!(retry_pipeline, pipeline);
        assert!(recreate_shaders, "the rejected shader modules do not exist on the host");
        assert!(recreate_pipeline, "the rejected pipeline does not exist on the host");
    }
}
