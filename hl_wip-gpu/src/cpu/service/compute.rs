//! Compute dispatch: resolve the bound compute pipeline → kernel program and the bound resources → the
//! parameter blob + storage regions, run the kernel per-thread over the grid ([`crate::cpu::interp`]), and
//! write the mutated regions back. Ported from `run_dispatch` / `run_kernel` in `hl-gpu/src/software.rs`.

use crate::cpu::interp;
use crate::cpu::model::pipeline::Pipeline;
use crate::cpu::model::shader::ShaderModule;
use crate::cpu::model::{bind_group, buffer, buffer_mut, pipeline, shader};
use crate::cpu::service::copy::buffer_slice_bounds;
use crate::protocol::model::descriptor::{BindGroupDesc, BindResource};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::kernel::KernelProgram;
use crate::runtime::model::resources::SessionResources;

/// Hard ceiling on the total launch-block count (grid_x * grid_y * grid_z) for a single dispatch over a
/// REAL kernel program. The per-thread step cap (1M, in [`crate::cpu::interp`]) bounds work WITHIN a
/// block, but the block COUNT was uncapped: a validated `Dispatch` with a huge grid over a real kernel
/// would iterate blocks unbounded (up to `u32::MAX^3`), a compute-time DoS.
///
/// It is a RUNTIME cap only — no wire bytes change — and is set far above any legitimate dispatch: the
/// largest real grid the oracle ever runs is ~262k blocks (a 1<<20-element vecadd with a 4-wide block),
/// so this 1<<26 (67,108,864) ceiling is ~256x that headroom. Every valid dispatch runs unchanged; only
/// a pathological grid is rejected — BEFORE any block iterates — with a typed `ResourceLimit`.
pub(crate) const MAX_DISPATCH_BLOCKS: u64 = 1 << 26;

/// Execute a compute `Dispatch`. A dispatch with no pipeline/bind group bound is a no-op (a malformed
/// stream the validation pass already rejected); a SPIR-V (non-kernel) module is recorded but not run.
pub(crate) fn run_dispatch(
    res: &mut SessionResources,
    pipeline_id: Option<u32>,
    bind_group_id: Option<u32>,
    grid: (u32, u32, u32),
) -> Result<()> {
    let (pid, bgid) = match (pipeline_id, bind_group_id) {
        (Some(p), Some(b)) => (p, b),
        _ => return Ok(()),
    };
    let shader_id = match pipeline(res, pid)? {
        Pipeline::Compute { shader } => *shader,
        Pipeline::Render { .. } => return Err(GpuError::Unsupported("dispatch on a render pipeline")),
    };
    // Clone the program out so the shader-table borrow is released before we touch buffers.
    let prog = match shader(res, shader_id)? {
        ShaderModule::Kernel(p) => (**p).clone(),
        ShaderModule::Spirv => return Ok(()), // software oracle cannot run SPIR-V
    };
    let bg = bind_group(res, bgid)?.desc.clone();
    run_kernel(res, &prog, &bg, grid)
}

fn run_kernel(
    res: &mut SessionResources,
    prog: &KernelProgram,
    bg: &BindGroupDesc,
    grid: (u32, u32, u32),
) -> Result<()> {
    // Reject a pathological grid BEFORE iterating a single block. The block loop in `interp::execute`
    // visits `gx.max(1) * gy.max(1) * gz.max(1)` blocks, so cap that exact product (via checked mul, so a
    // `u32^3` grid that overflows `u64` is treated as over-cap).
    let blocks = (grid.0.max(1) as u64)
        .checked_mul(grid.1.max(1) as u64)
        .and_then(|v| v.checked_mul(grid.2.max(1) as u64));
    match blocks {
        Some(b) if b <= MAX_DISPATCH_BLOCKS => {}
        _ => return Err(GpuError::ResourceLimit("dispatch grid blocks")),
    }
    // Gather the parameter blob (binding 0) and each pointer region (binding r+1 → region r).
    let mut param_blob: Vec<u8> = Vec::new();
    let mut regions: Vec<Vec<u8>> = vec![Vec::new(); prog.num_regions as usize];
    let mut writeback: Vec<Option<(u32, u64)>> = vec![None; prog.num_regions as usize];
    for e in &bg.entries {
        if let BindResource::Buffer { id, offset, size } = e.resource {
            let buf = buffer(res, id)?;
            let (off, len) = buffer_slice_bounds(buf.data.len(), offset, size)?;
            let bytes = buf.data[off..off + len].to_vec();
            if e.binding == 0 {
                param_blob = bytes;
            } else {
                let r = (e.binding - 1) as usize;
                if r < regions.len() {
                    regions[r] = bytes;
                    writeback[r] = Some((id, offset));
                }
            }
        }
    }
    interp::execute(prog, &param_blob, &mut regions, grid)?;
    for (r, wb) in writeback.iter().enumerate() {
        if let Some((id, offset)) = wb {
            let buf = buffer_mut(res, *id)?;
            let off = *offset as usize;
            let end = off
                .checked_add(regions[r].len())
                .filter(|e| *e <= buf.data.len())
                .ok_or(GpuError::OutOfBounds)?;
            buf.data[off..end].copy_from_slice(&regions[r]);
        }
    }
    Ok(())
}
