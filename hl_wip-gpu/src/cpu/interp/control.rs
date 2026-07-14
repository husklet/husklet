//! The grid/CTA driver: [`execute`] walks the launch grid, and [`run_block`] cooperatively runs one
//! thread block so `bar.sync` is modeled faithfully (every live thread reaches the barrier before the
//! block advances past it). Ported from `execute` / `run_block` in `hl-gpu/src/ptx.rs`.

use super::exec::{run_until, Stop, Val};
use crate::protocol::model::error::Result;
use crate::protocol::model::kernel::{Inst, KernelProgram};

/// Execute `prog` over the launch `grid` (in blocks). `param_blob` is the flat kernel-parameter bytes
/// (binding 0); `regions[r]` is the mutable device memory of pointer-parameter region `r`. Buffers are
/// mutated in place; the caller writes them back. Pure CPU, no GPU.
pub fn execute(
    prog: &KernelProgram,
    param_blob: &[u8],
    regions: &mut [Vec<u8>],
    grid: (u32, u32, u32),
) -> Result<()> {
    let [bx, by, bz] = prog.block;
    let (gx, gy, gz) = grid;
    // Kernels with shared memory or barriers need cooperative, block-scoped execution; elementwise
    // kernels keep the run-each-thread-independently path.
    let coop = prog.shared_bytes > 0 || prog.insts.iter().any(|i| matches!(i, Inst::Bar));
    for cz in 0..gz.max(1) {
        for cy in 0..gy.max(1) {
            for cx in 0..gx.max(1) {
                if coop {
                    run_block(prog, param_blob, regions, &[bx, by, bz], [cx, cy, cz], [gx, gy, gz])?;
                    continue;
                }
                for tz in 0..bz.max(1) {
                    for ty in 0..by.max(1) {
                        for tx in 0..bx.max(1) {
                            let sr = [
                                tx, ty, tz, bx, by, bz, cx, cy, cz, gx.max(1), gy.max(1), gz.max(1),
                            ];
                            let mut regs = vec![Val::I(0); prog.reg_count as usize];
                            let mut pc = 0usize;
                            let mut shared: Vec<u8> = Vec::new();
                            run_until(prog, param_blob, regions, &mut shared, &mut regs, &mut pc, &sr)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Cooperatively execute one thread block (CTA), correctly modeling `bar.sync`: every live thread runs
/// forward to its next barrier (or `ret`); only once all live threads have arrived does the block advance
/// past the barrier. All threads share one `shared` byte array (workgroup memory).
fn run_block(
    prog: &KernelProgram,
    param_blob: &[u8],
    regions: &mut [Vec<u8>],
    block: &[u32; 3],
    cta: [u32; 3],
    grid: [u32; 3],
) -> Result<()> {
    let [bx, by, bz] = *block;
    let [gx, gy, gz] = grid;
    let mut shared = vec![0u8; prog.shared_bytes as usize];
    struct T {
        regs: Vec<Val>,
        pc: usize,
        done: bool,
        sr: [u32; 12],
    }
    let mut threads: Vec<T> = Vec::new();
    for tz in 0..bz.max(1) {
        for ty in 0..by.max(1) {
            for tx in 0..bx.max(1) {
                threads.push(T {
                    regs: vec![Val::I(0); prog.reg_count as usize],
                    pc: 0,
                    done: false,
                    sr: [
                        tx, ty, tz, bx, by, bz, cta[0], cta[1], cta[2], gx.max(1), gy.max(1),
                        gz.max(1),
                    ],
                });
            }
        }
    }
    let phase_cap = prog.insts.len() as u64 * 64 + 1024;
    let mut phases = 0u64;
    loop {
        let mut all_done = true;
        for t in &mut threads {
            if t.done {
                continue;
            }
            match run_until(prog, param_blob, regions, &mut shared, &mut t.regs, &mut t.pc, &t.sr)? {
                Stop::Ret => t.done = true,
                Stop::Barrier => {}
            }
            if !t.done {
                all_done = false;
            }
        }
        if all_done {
            return Ok(());
        }
        phases += 1;
        if phases > phase_cap {
            return Err(super::kerr(
                "kernel block exceeded barrier-phase cap (barrier not reached by all threads?)",
            ));
        }
    }
}
