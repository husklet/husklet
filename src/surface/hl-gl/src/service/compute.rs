//! Compute dispatch (GLES3.1) — the GL analogue of the CUDA compute path.
//!
//! Unlike the render ops (deferred to `eglSwapBuffers`), a `glDispatchCompute` lowers + submits
//! IMMEDIATELY, exactly as `hl_cuda`'s `cuLaunchKernel` submits per-launch: a compute program
//! (`glCreateShader(GL_COMPUTE_SHADER)` → `glLinkProgram`) becomes a `CreateShader` +
//! `CreateComputePipeline`, its `glBindBufferBase`/`glBindBufferRange` SSBO/UBO bindings become a bind
//! group, and the grid becomes a `Dispatch` inside a compute pass.
//!
//! HONEST LIMIT: the software CPU oracle runs only neutral KERNEL programs (classified by
//! `KERNEL_MAGIC`); a GLSL-compute program lowers to a `Msl` shader payload the CPU executor
//! accepts but does not run (`ShaderModule::Spirv => Ok(())` in the dispatch path). So this drives the
//! full lowering + submits the exact `Cmd` stream — asserted by the lowering tests — but the result is
//! not materialized on the CPU oracle (a real Metal/Vulkan host would run the `cmain` kernel). The
//! mirror of the cuda path is the *lowering*, not the software execution.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{Cmd, CommandBuffer, CommandSink, GpuError, Result, ShaderPayloadKind};

/// The largest per-dimension work-group count this driver advertises (ES3.1 minimum is 65535).
const MAX_COMPUTE_WORK_GROUP_COUNT: u32 = 65535;
const UNIFORM_BINDING_BASE: u32 = MAX_SHADER_STORAGE_BUFFER_BINDINGS;

/// `glDispatchCompute(x, y, z)` — lower the bound compute program + its SSBO/UBO bindings into a
/// `CreateComputePipeline` + a `Dispatch`, and submit. A non-compute / unlinked bound program raises
/// `GL_INVALID_OPERATION` (and submits nothing); an out-of-range group count raises `GL_INVALID_VALUE`.
pub fn dispatch_compute(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    x: u32,
    y: u32,
    z: u32,
) -> Result<()> {
    if x > MAX_COMPUTE_WORK_GROUP_COUNT
        || y > MAX_COMPUTE_WORK_GROUP_COUNT
        || z > MAX_COMPUTE_WORK_GROUP_COUNT
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return Ok(());
    }
    lower_dispatch(ctx, sink, (x, y, z))
}

/// `glDispatchComputeIndirect(indirect)` — read the three `GLuint` group counts from the buffer bound to
/// `GL_DISPATCH_INDIRECT_BUFFER` at byte offset `indirect`, then dispatch them. Honest: the indirect args
/// live in a host-visible buffer in this model, so they are read at record time. A negative offset, no
/// bound indirect buffer, or an out-of-range read raises `GL_INVALID_OPERATION`/`GL_INVALID_VALUE`.
pub fn dispatch_compute_indirect(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    indirect: isize,
) -> Result<()> {
    if indirect < 0 || indirect % 4 != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return Ok(());
    }
    let buf = ctx.buffer_for_target(GL_DISPATCH_INDIRECT_BUFFER);
    if ctx.buffers.is_mapped(buf) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return Ok(());
    }
    let off = indirect as usize;
    let grid = ctx
        .buffers
        .get(buf)
        .filter(|b| off + 12 <= b.data.len())
        .map(|b| {
            let rd = |i: usize| {
                u32::from_le_bytes([b.data[i], b.data[i + 1], b.data[i + 2], b.data[i + 3]])
            };
            (rd(off), rd(off + 4), rd(off + 8))
        });
    let Some(grid) = grid else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return Ok(());
    };
    lower_dispatch(ctx, sink, grid)
}

/// Shared lowering for both dispatch entry points: build + submit the compute `Cmd` stream for the bound
/// compute program at the given grid.
fn lower_dispatch(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    grid: (u32, u32, u32),
) -> Result<()> {
    if ctx
        .local
        .indexed_buffers
        .iter()
        .any(|(&(target, _), binding)| {
            matches!(target, GL_SHADER_STORAGE_BUFFER | GL_UNIFORM_BUFFER)
                && ctx.buffers.is_mapped(binding.buffer)
        })
    {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return Ok(());
    }
    // The bound program must be a linked compute program.
    let compute_ir = match ctx.programs.program(ctx.local.cur_prog) {
        Some(p) if p.is_compute() => p.compute_ir.clone().unwrap_or_default(),
        _ => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return Ok(());
        }
    };

    let mut cmds: Vec<Cmd> = Vec::new();

    // Compute shader + pipeline.
    let shader_ir = ctx.alloc_shader_ir()?;
    cmds.push(Cmd::CreateShader {
        id: shader_ir,
        kind: ShaderPayloadKind::Glsl,
        spirv: compute_ir,
    });
    let pipeline_ir = ctx.alloc_pipeline_ir()?;
    cmds.push(Cmd::CreateComputePipeline(
        pipeline_ir,
        ComputePipelineDesc {
            compute: ShaderRef {
                module: shader_ir,
                entry: "cmain".into(),
            },
            label: "gl-compute".into(),
        },
    ));

    // Bind group: every SSBO + UBO indexed binding, sorted by binding index for a deterministic stream.
    // `(target, index, binding)`; SSBO → STORAGE, UBO → UNIFORM buffer usage.
    let mut bound: Vec<(u32, u32, crate::model::context::IndexedBinding)> = ctx
        .local
        .indexed_buffers
        .iter()
        .filter(|((target, _), _)| {
            *target == GL_SHADER_STORAGE_BUFFER || *target == GL_UNIFORM_BUFFER
        })
        .map(|((target, index), b)| (*target, *index, *b))
        .collect();
    bound.sort_by_key(|(target, index, _)| (*index, *target));
    let mut entries: Vec<BindEntry> = Vec::new();
    let mut transient_buffers = Vec::new();
    let mut writebacks = Vec::new();
    for (target, index, b) in &bound {
        let data = ctx
            .buffers
            .get(b.buffer)
            .map(|gb| gb.data.clone())
            .unwrap_or_default();
        if data.is_empty() {
            continue;
        }
        let usage = if *target == GL_SHADER_STORAGE_BUFFER {
            buffer_usage::STORAGE | buffer_usage::COPY_SRC | buffer_usage::COPY_DST
        } else {
            buffer_usage::UNIFORM | buffer_usage::COPY_DST
        };
        let ir = ctx.alloc_buffer_ir()?;
        transient_buffers.push(ir);
        let size = data.len() as u64;
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size,
                usage,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: ir,
            offset: 0,
            data: data.as_ref().clone(),
        });
        let bind_off = b.offset.max(0) as u64;
        let bind_size = if b.size > 0 {
            b.size as u64
        } else {
            size.saturating_sub(bind_off)
        };
        let binding = if *target == GL_UNIFORM_BUFFER {
            UNIFORM_BINDING_BASE + *index
        } else {
            *index
        };
        entries.push(BindEntry {
            binding,
            resource: BindResource::Buffer {
                id: ir,
                offset: bind_off,
                size: bind_size,
            },
        });
        if *target == GL_SHADER_STORAGE_BUFFER {
            writebacks.push((b.buffer, ir, bind_off, bind_size as usize));
        }
    }
    entries.sort_by_key(|entry| entry.binding);
    let bind_group_ir = ctx.alloc_bind_group_ir()?;
    cmds.push(Cmd::CreateBindGroup(
        bind_group_ir,
        BindGroupDesc { set: 0, entries },
    ));

    // The compute pass: bind the pipeline + resources and dispatch the grid.
    let (gx, gy, gz) = grid;
    let ops = vec![
        Enc::BeginComputePass,
        Enc::SetPipeline(pipeline_ir),
        Enc::SetBindGroup {
            index: 0,
            group: bind_group_ir,
        },
        Enc::Dispatch {
            x: gx,
            y: gy,
            z: gz,
        },
        Enc::EndComputePass,
    ];
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: ops,
        signal: None,
    }));

    sink.submit(&cmds)?;

    let writeback_result = (|| {
        let mut completed = Vec::new();
        for (buffer, ir, offset, length) in writebacks {
            let bytes = sink.read_buffer(hl_gpu::BufferId(ir), offset, length)?;
            if bytes.len() != length {
                return Err(GpuError::Invalid("compute SSBO readback length"));
            }
            completed.push((buffer, offset as usize, bytes));
        }
        for (buffer, offset, bytes) in completed {
            ctx.buffers.set_sub_data(buffer, offset, &bytes);
        }
        Ok(())
    })();

    let mut cleanup = Vec::with_capacity(transient_buffers.len() + 3);
    cleanup.extend(transient_buffers.into_iter().map(Cmd::DestroyBuffer));
    cleanup.push(Cmd::DestroyBindGroup(bind_group_ir));
    cleanup.push(Cmd::DestroyPipeline(pipeline_ir));
    cleanup.push(Cmd::DestroyShader(shader_ir));
    let cleanup_result = sink.submit(&cleanup);

    writeback_result?;
    cleanup_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::record;
    use hl_gpu::RecordingSink;

    #[test]
    fn dispatch_rejects_mapped_ssbo_and_ubo_bindings_before_snapshot() {
        for target in [GL_SHADER_STORAGE_BUFFER, GL_UNIFORM_BUFFER] {
            let mut context = GlContext::new();
            let mut sink = RecordingSink::with_full_caps();
            let buffer = context.buffers.gen();
            record::bind_buffer(&mut context, target, buffer);
            record::buffer_data(&mut context, target, &[0; 16], 0);
            record::bind_buffer_base(&mut context, target, 0, buffer);
            assert_eq!(
                context.buffers.map_range(buffer, 0, 16, GL_MAP_WRITE_BIT),
                Some(0)
            );
            let pointer = context.buffers.mapped_ptr(buffer, 0).unwrap();

            dispatch_compute(&mut context, &mut sink, 1, 1, 1).unwrap();

            assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
            assert!(sink.batches.is_empty());
            // SAFETY: rejected dispatch did not snapshot or replace the mapped allocation.
            unsafe { pointer.write(7) };
        }
    }

    #[test]
    fn indirect_dispatch_rejects_a_mapped_argument_buffer() {
        let mut context = GlContext::new();
        let mut sink = RecordingSink::with_full_caps();
        let buffer = context.buffers.gen();
        record::bind_buffer(&mut context, GL_DISPATCH_INDIRECT_BUFFER, buffer);
        record::buffer_data(
            &mut context,
            GL_DISPATCH_INDIRECT_BUFFER,
            &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0],
            0,
        );
        assert_eq!(
            context.buffers.map_range(buffer, 0, 12, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(buffer, 0).unwrap();

        dispatch_compute_indirect(&mut context, &mut sink, 0).unwrap();

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        assert!(sink.batches.is_empty());
        // SAFETY: rejected dispatch did not read, snapshot, or replace the mapped allocation.
        unsafe { pointer.write(7) };
    }
}
