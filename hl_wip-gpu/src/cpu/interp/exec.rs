//! The per-thread instruction step: [`run_until`] runs one thread forward until it hits a `bar.sync`
//! (rendezvous) or `ret` (retire). Ported from `run_until` in `hl-gpu/src/ptx.rs`, plus the [`Val`]
//! tagged-value model. Called by [`super::control`] once per phase per live thread.

use super::memory::{atomic_rmw, ptr_int_add, ptr_target, read_scalar, shared_addr, write_scalar};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, BIT_AND, BIT_OR, CMP_EQ, CMP_GT, CMP_LE, CMP_LT, CMP_NE,
    CVT_F32_FROM_S32, CVT_S32_FROM_F32, CVT_S64_FROM_S32, SHIFT_LEFT,
};

/// A runtime value in a register: an integer (also holding f32 bits when moved through integer ops), an
/// f32, a tagged device pointer `Ptr{region, byte-offset}`, or a predicate.
#[derive(Clone, Copy)]
pub(super) enum Val {
    I(u64),
    F(f32),
    P { region: u32, off: i64 },
    Pred(bool),
}

impl Val {
    pub(super) fn as_u64(self) -> u64 {
        match self {
            Val::I(v) => v,
            Val::F(f) => f.to_bits() as u64,
            Val::P { off, .. } => off as u64,
            Val::Pred(b) => b as u64,
        }
    }
    pub(super) fn as_i64(self) -> i64 {
        self.as_u64() as i64
    }
    fn as_f32(self) -> f32 {
        match self {
            Val::F(f) => f,
            Val::I(v) => f32::from_bits(v as u32),
            _ => 0.0,
        }
    }
    fn as_bool(self) -> bool {
        matches!(self, Val::Pred(true))
    }
}

/// Where a thread paused: at a `bar.sync` (rendezvous) or a `ret` (retired).
pub(super) enum Stop {
    Ret,
    Barrier,
}

/// Run one thread forward from `*pc` until it retires (`ret`) or reaches a barrier. `regs`/`pc` are the
/// thread's continuation state; `shared` is the block-shared byte array; `sr` is the 12 special registers
/// (`%tid.*`, `%ntid.*`, `%ctaid.*`, `%nctaid.*`). Mutates `regions` (device memory) and `shared` in place.
pub(super) fn run_until(
    prog: &KernelProgram,
    param_blob: &[u8],
    regions: &mut [Vec<u8>],
    shared: &mut [u8],
    regs: &mut [Val],
    pc: &mut usize,
    sr: &[u32; 12],
) -> Result<Stop> {
    let mut steps = 0u64;
    let step_cap = 1_000_000u64;

    let eval = |regs: &[Val], op: &Op| -> Val {
        match op {
            Op::Reg(r) => regs[*r as usize],
            Op::ImmI(i) => Val::I(*i as u64),
            Op::ImmF(b) => Val::F(f32::from_bits(*b)),
        }
    };

    while *pc < prog.insts.len() {
        steps += 1;
        if steps > step_cap {
            return Err(super::kerr("kernel exceeded step cap (suspected infinite loop)"));
        }
        match &prog.insts[*pc] {
            Inst::Ret => return Ok(Stop::Ret),
            Inst::Bar => {
                *pc += 1;
                return Ok(Stop::Barrier);
            }
            Inst::Nop => {}
            Inst::MovImmI { d, imm } => regs[*d as usize] = Val::I(*imm),
            Inst::MovImmF { d, bits } => regs[*d as usize] = Val::F(f32::from_bits(*bits)),
            Inst::MovReg { d, s } => regs[*d as usize] = regs[*s as usize],
            Inst::MovSReg { d, sreg } => regs[*d as usize] = Val::I(sr[*sreg as usize] as u64),
            Inst::LdParam { d, param } => {
                let p = &prog.params[*param as usize];
                regs[*d as usize] = if p.is_ptr {
                    Val::P { region: p.region, off: 0 }
                } else {
                    Val::I(read_scalar(param_blob, p.offset as usize, p.width as usize)?)
                };
            }
            Inst::Cvta { d, s } => regs[*d as usize] = regs[*s as usize],
            Inst::IAdd { d, a, b, wide } => {
                let (va, vb) = (eval(regs, a), eval(regs, b));
                regs[*d as usize] = ptr_int_add(va, vb, *wide, 1);
            }
            Inst::ISub { d, a, b, wide } => {
                let (va, vb) = (eval(regs, a), eval(regs, b));
                regs[*d as usize] = ptr_int_add(va, vb, *wide, -1);
            }
            Inst::IMul { d, a, b, wide, unsigned } => {
                let (va, vb) = (eval(regs, a).as_u64(), eval(regs, b).as_u64());
                regs[*d as usize] = if *wide {
                    if *unsigned {
                        Val::I((va as u32 as u64).wrapping_mul(vb as u32 as u64))
                    } else {
                        Val::I(((va as i32 as i64).wrapping_mul(vb as i32 as i64)) as u64)
                    }
                } else {
                    Val::I((va as u32).wrapping_mul(vb as u32) as u64)
                };
            }
            Inst::IMad { d, a, b, c } => {
                let va = eval(regs, a).as_i64() as i32;
                let vb = eval(regs, b).as_i64() as i32;
                let vc = eval(regs, c).as_i64() as i32;
                regs[*d as usize] = Val::I(va.wrapping_mul(vb).wrapping_add(vc) as u32 as u64);
            }
            Inst::Setp { d, a, b, cmp, unsigned } => {
                let r = if *unsigned {
                    let va = eval(regs, a).as_u64() as u32;
                    let vb = eval(regs, b).as_u64() as u32;
                    match *cmp {
                        CMP_EQ => va == vb,
                        CMP_NE => va != vb,
                        CMP_LT => va < vb,
                        CMP_LE => va <= vb,
                        CMP_GT => va > vb,
                        _ => va >= vb,
                    }
                } else {
                    let va = eval(regs, a).as_u64() as i32;
                    let vb = eval(regs, b).as_u64() as i32;
                    match *cmp {
                        CMP_EQ => va == vb,
                        CMP_NE => va != vb,
                        CMP_LT => va < vb,
                        CMP_LE => va <= vb,
                        CMP_GT => va > vb,
                        _ => va >= vb,
                    }
                };
                regs[*d as usize] = Val::Pred(r);
            }
            Inst::Bra { target, pred } => {
                let take = match pred {
                    None => true,
                    Some((p, neg)) => {
                        let v = regs[*p as usize].as_bool();
                        if *neg {
                            !v
                        } else {
                            v
                        }
                    }
                };
                if take {
                    *pc = *target as usize;
                    continue;
                }
            }
            Inst::LdGlobal { d, addr, off, ty } => {
                let (region, base) = ptr_target(regs[*addr as usize])?;
                let eff = base.wrapping_add(*off);
                if eff < 0 {
                    return Err(GpuError::OutOfBounds);
                }
                let at = eff as usize;
                let mem =
                    regions.get(region as usize).ok_or_else(|| super::kerr("ld.global: unbound region"))?;
                regs[*d as usize] = match *ty {
                    gty::F32 => Val::F(f32::from_bits(read_scalar(mem, at, 4)? as u32)),
                    gty::U32 => Val::I(read_scalar(mem, at, 4)?),
                    _ => Val::I(read_scalar(mem, at, 8)?),
                };
            }
            Inst::StGlobal { addr, off, src, ty } => {
                let (region, base) = ptr_target(regs[*addr as usize])?;
                let eff = base.wrapping_add(*off);
                if eff < 0 {
                    return Err(GpuError::OutOfBounds);
                }
                let at = eff as usize;
                let v = eval(regs, src);
                let mem = regions
                    .get_mut(region as usize)
                    .ok_or_else(|| super::kerr("st.global: unbound region"))?;
                match *ty {
                    gty::F32 => write_scalar(mem, at, 4, v.as_f32().to_bits() as u64)?,
                    gty::U32 => write_scalar(mem, at, 4, v.as_u64())?,
                    _ => write_scalar(mem, at, 8, v.as_u64())?,
                }
            }
            Inst::FAdd { d, a, b } => {
                regs[*d as usize] = Val::F(eval(regs, a).as_f32() + eval(regs, b).as_f32())
            }
            Inst::FSub { d, a, b } => {
                regs[*d as usize] = Val::F(eval(regs, a).as_f32() - eval(regs, b).as_f32())
            }
            Inst::FMul { d, a, b } => {
                regs[*d as usize] = Val::F(eval(regs, a).as_f32() * eval(regs, b).as_f32())
            }
            Inst::FFma { d, a, b, c } => {
                let (fa, fb, fc) =
                    (eval(regs, a).as_f32(), eval(regs, b).as_f32(), eval(regs, c).as_f32());
                regs[*d as usize] = Val::F(fa.mul_add(fb, fc));
            }
            Inst::Cvt { d, s, kind } => {
                let v = eval(regs, s);
                regs[*d as usize] = match *kind {
                    CVT_F32_FROM_S32 => Val::F(v.as_u64() as i32 as f32),
                    CVT_S64_FROM_S32 => Val::I(v.as_u64() as i32 as i64 as u64),
                    CVT_S32_FROM_F32 => Val::I(v.as_f32() as i32 as u32 as u64),
                    _ => v,
                };
            }
            Inst::Shift { d, a, b, dir, unsigned } => {
                let va = eval(regs, a).as_u64() as u32;
                let sh = (eval(regs, b).as_u64() as u32) & 31;
                let r = match *dir {
                    SHIFT_LEFT => va.wrapping_shl(sh),
                    _ if *unsigned => va.wrapping_shr(sh),
                    _ => (va as i32).wrapping_shr(sh) as u32,
                };
                regs[*d as usize] = Val::I(r as u64);
            }
            Inst::BitOp { d, a, b, op } => {
                let va = eval(regs, a).as_u64() as u32;
                let vb = eval(regs, b).as_u64() as u32;
                let r = match *op {
                    BIT_AND => va & vb,
                    BIT_OR => va | vb,
                    _ => va ^ vb,
                };
                regs[*d as usize] = Val::I(r as u64);
            }
            Inst::LdShared { d, base, off, ty } => {
                let at = shared_addr(eval(regs, base), *off)?;
                regs[*d as usize] = match *ty {
                    gty::F32 => Val::F(f32::from_bits(read_scalar(shared, at, 4)? as u32)),
                    gty::U32 => Val::I(read_scalar(shared, at, 4)?),
                    _ => Val::I(read_scalar(shared, at, 8)?),
                };
            }
            Inst::StShared { base, off, src, ty } => {
                let at = shared_addr(eval(regs, base), *off)?;
                let v = eval(regs, src);
                match *ty {
                    gty::F32 => write_scalar(shared, at, 4, v.as_f32().to_bits() as u64)?,
                    gty::U32 => write_scalar(shared, at, 4, v.as_u64())?,
                    _ => write_scalar(shared, at, 8, v.as_u64())?,
                }
            }
            Inst::AtomGlobal { d, addr, off, op, cmp, val, unsigned } => {
                let (region, base) = ptr_target(regs[*addr as usize])?;
                let eff = base.wrapping_add(*off);
                if eff < 0 {
                    return Err(GpuError::OutOfBounds);
                }
                let at = eff as usize;
                let cmpv = eval(regs, cmp).as_u64() as u32;
                let valv = eval(regs, val).as_u64() as u32;
                let mem = regions
                    .get_mut(region as usize)
                    .ok_or_else(|| super::kerr("atom.global: unbound region"))?;
                let old = atomic_rmw(mem, at, *op, cmpv, valv, *unsigned)?;
                if let Some(dr) = d {
                    regs[*dr as usize] = Val::I(old as u64);
                }
            }
            Inst::AtomShared { d, base, off, op, cmp, val, unsigned } => {
                let at = shared_addr(eval(regs, base), *off)?;
                let cmpv = eval(regs, cmp).as_u64() as u32;
                let valv = eval(regs, val).as_u64() as u32;
                let old = atomic_rmw(shared, at, *op, cmpv, valv, *unsigned)?;
                if let Some(dr) = d {
                    regs[*dr as usize] = Val::I(old as u64);
                }
            }
        }
        *pc += 1;
    }
    Ok(Stop::Ret)
}
