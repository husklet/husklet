//! Pre-pass planning for one render pass: the viewport-correction uniform and the concrete
//! `wgpu::BindGroup` every draw binds.
//!
//! A wgpu render pass borrows its encoder for its whole lifetime, so no bind group can be created while the
//! pass is open — every draw's binds must exist before `begin_render_pass`. This is that phase.
//!
//! **Why the memo cache.** The protocol's bind group is only a descriptor (wgpu binds against a
//! *pipeline's* layout, see `crate::bindgroup`), so a concrete `wgpu::BindGroup` has to be built per
//! (draw, set). Building one cost 2.6 µs on llvmpipe (measured 2026-07-30, `tests/frame_profile.rs`), so a
//! 600-draw frame spent ~1.6 ms rebuilding binds — and a stream that binds the SAME group id to the SAME
//! set on the SAME pipeline across draws rebuilt an identical object every time. [`Plan`] memoizes on
//! `(pipeline id, set index, bind group id)`, which is the complete identity: within one pass the op stream
//! carries no create/destroy, so those three ids cannot come to mean different resources, and the layout,
//! the pipeline's read-binding filter, and the pass's viewport buffer are all fixed by the pipeline id and
//! the pass. The dynamic offset is NOT part of the bind group — it is applied at `set_bind_group` time — so
//! per-draw offsets still differ on a shared object.

use std::collections::HashMap;

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindGroupDesc;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::pipeline::PipelineNative;
use crate::WgpuExecutor;

use super::Viewport;

/// One set bound for a draw: the set index, the concrete bind group, and its dynamic offset (set 0 carries
/// the pass's viewport-correction offset; other sets carry none).
pub(super) struct Bound {
    pub index: u32,
    pub group: wgpu::BindGroup,
    pub offset: Option<u32>,
}

/// One planned draw: the pipeline it uses and every set it binds, in set order.
pub(super) struct Draw {
    pub id: u32,
    pub pipeline: wgpu::RenderPipeline,
    pub groups: Vec<Bound>,
}

/// Everything the open pass needs that must be built before it starts.
pub(super) struct Plan {
    pub draws: Vec<Draw>,
    /// The pass's viewport-correction uniform: one `vec4` per draw at `alignment` stride. `None` only when
    /// the pass has no draws.
    pub viewport_buffer: Option<wgpu::Buffer>,
}

impl Plan {
    /// Scan `ops` and build the pass plan. `target_w`/`target_h` are the pass's render extent (the minimum
    /// across its attachments), which a GL-style out-of-bounds viewport is intersected with.
    pub(super) fn build(
        exec: &WgpuExecutor,
        res: &SessionResources,
        ops: &[Enc],
        target_formats: &[TextureFormat],
        target_w: u32,
        target_h: u32,
    ) -> Result<Self> {
        let alignment = exec
            .device()
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(16) as usize;
        let corrections = corrections(ops, target_w, target_h);
        let viewport_buffer = viewport_buffer(exec, &corrections, alignment)?;
        let mut plan = Self {
            draws: Vec::with_capacity(corrections.len()),
            viewport_buffer,
        };
        // Memoized concrete bind groups for this pass, keyed on (pipeline id, set index, bind group id).
        let mut cache: HashMap<
            (u32, u32, u32, Vec<crate::texel_buffer::Specialization>),
            wgpu::BindGroup,
        > = HashMap::new();
        let mut current: Option<u32> = None;
        // Sized to the advertised `max_bind_groups` (4); a higher index would have been rejected upstream.
        let mut bound: [Option<u32>; 4] = [None; 4];
        let mut draw_index = 0usize;
        for op in ops {
            match op {
                Enc::SetPipeline(p) => current = Some(*p),
                Enc::SetBindGroup { index, group } => {
                    let slot = *index as usize;
                    if slot >= bound.len() {
                        return Err(GpuError::Invalid(
                            "wgpu: render bind-group index out of range",
                        ));
                    }
                    bound[slot] = Some(*group);
                }
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    hl_log::hl_count!(hl_log::tag::EXEC, "draws");
                    let offset = draw_index
                        .checked_mul(alignment)
                        .and_then(|offset| u32::try_from(offset).ok())
                        .ok_or(GpuError::OutOfBounds)?;
                    draw_index += 1;
                    plan.push_draw(
                        exec,
                        res,
                        &mut cache,
                        current,
                        &bound,
                        target_formats,
                        offset,
                    )?;
                }
                _ => {}
            }
        }
        Ok(plan)
    }

    /// Plan one draw: resolve its pipeline, then build (or reuse) a bind group for every bound set.
    #[allow(clippy::too_many_arguments)]
    fn push_draw(
        &mut self,
        exec: &WgpuExecutor,
        res: &SessionResources,
        cache: &mut HashMap<
            (u32, u32, u32, Vec<crate::texel_buffer::Specialization>),
            wgpu::BindGroup,
        >,
        current: Option<u32>,
        bound: &[Option<u32>; 4],
        target_formats: &[TextureFormat],
        offset: u32,
    ) -> Result<()> {
        let pid = current.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
        let (pipeline_formats, used_bindings) = match PipelineNative::get(res, pid)? {
            PipelineNative::Render {
                color_formats,
                used_bindings,
                ..
            } => (color_formats, used_bindings),
            PipelineNative::Compute { .. } => {
                return Err(GpuError::Unsupported("wgpu: draw on a compute pipeline"))
            }
        };
        if pipeline_formats != target_formats {
            return Err(GpuError::Invalid(
                "pipeline color format mismatches render attachment",
            ));
        }
        let descriptors = bound
            .iter()
            .filter_map(|group| group.map(|id| exec.bind_group(res, id)))
            .collect::<Result<Vec<_>>>()?;
        let alignment = exec.gpu.device.limits().min_storage_buffer_offset_alignment as u64;
        let specialization = crate::texel_buffer::key(descriptors.iter().copied(), alignment)?;
        let pipeline = exec.render_pipeline_for(res, pid, &specialization)?;
        let viewport = self
            .viewport_buffer
            .as_ref()
            .ok_or(GpuError::Invalid("draw has no viewport uniform buffer"))?;
        let mut groups = Vec::with_capacity(bound.iter().filter(|g| g.is_some()).count().max(1));
        for (index, group) in bound.iter().enumerate() {
            let Some(gid) = *group else { continue };
            let index = index as u32;
            let cache_key = (pid, index, gid, specialization.clone());
            let group = match cache.get(&cache_key) {
                Some(hit) => hit.clone(),
                None => {
                    let started = exec.profile_clock();
                    // Filter the driver's entries to the bindings this pipeline's shaders actually read in
                    // THIS set, so a bind group carrying an unsampled texture/sampler pair still matches
                    // (see `bindgroup::build_bind_group`).
                    let descriptor = exec.bind_group(res, gid)?;
                    let views = crate::texel_buffer::views(res, descriptor, alignment)?;
                    let built = exec.build_bind_group(
                        res,
                        &pipeline.get_bind_group_layout(index),
                        descriptor,
                        Some(used_bindings),
                        true,
                        Some(&views),
                        (index == 0).then_some(viewport),
                        None,
                    )?;
                    exec.profile_record(|p| &mut p.draw_bind_groups, started);
                    cache.insert(cache_key, built.clone());
                    built
                }
            };
            groups.push(Bound {
                index,
                group,
                offset: (index == 0).then_some(offset),
            });
        }
        if bound[0].is_none() {
            // The bindingless draw still needs set 0 for the viewport-correction uniform. Keyed on bind
            // group id 0, which no live protocol bind group can be (ids start at 1).
            let cache_key = (pid, 0, 0, specialization.clone());
            let group = match cache.get(&cache_key) {
                Some(hit) => hit.clone(),
                None => {
                    let started = exec.profile_clock();
                    let empty = BindGroupDesc {
                        set: 0,
                        entries: Vec::new(),
                    };
                    let built = exec.build_bind_group(
                        res,
                        &pipeline.get_bind_group_layout(0),
                        &empty,
                        Some(used_bindings),
                        true,
                        None,
                        Some(viewport),
                        None,
                    )?;
                    exec.profile_record(|p| &mut p.draw_bind_groups, started);
                    cache.insert(cache_key, built.clone());
                    built
                }
            };
            groups.push(Bound {
                index: 0,
                group,
                offset: Some(offset),
            });
        }
        self.draws.push(Draw {
            id: pid,
            pipeline,
            groups,
        });
        Ok(())
    }
}

/// The viewport correction in force at each draw, in stream order — one entry per draw.
fn corrections(ops: &[Enc], target_w: u32, target_h: u32) -> Vec<[f32; 4]> {
    let draws = ops
        .iter()
        .filter(|op| matches!(op, Enc::Draw { .. } | Enc::DrawIndexed { .. }))
        .count();
    let mut out = Vec::with_capacity(draws);
    let mut viewport = Viewport::full();
    for op in ops {
        match op {
            Enc::SetViewport { x, y, w, h, .. } => {
                viewport = Viewport::new(*x, *y, *w, *h, target_w, target_h)
                    .unwrap_or_else(Viewport::full);
            }
            Enc::Draw { .. } | Enc::DrawIndexed { .. } => out.push(viewport.correction),
            _ => {}
        }
    }
    out
}

/// Pack the per-draw corrections into one uniform buffer at `alignment` stride, or `None` for a pass with
/// no draws.
fn viewport_buffer(
    exec: &WgpuExecutor,
    corrections: &[[f32; 4]],
    alignment: usize,
) -> Result<Option<wgpu::Buffer>> {
    use wgpu::util::DeviceExt;

    let size = corrections
        .len()
        .checked_mul(alignment)
        .ok_or(GpuError::OutOfBounds)?;
    if size == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0u8; size];
    for (index, correction) in corrections.iter().enumerate() {
        let start = index.checked_mul(alignment).ok_or(GpuError::OutOfBounds)?;
        let end = start.checked_add(16).ok_or(GpuError::OutOfBounds)?;
        bytes[start..end].copy_from_slice(bytemuck::cast_slice(correction));
    }
    Ok(Some(exec.device().create_buffer_init(
        &wgpu::util::BufferInitDescriptor {
            label: Some("hl-viewports"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        },
    )))
}
