//! PTX text → hl-GPU **neutral kernel-IR** ([`KernelProgram`]) front-end.
//!
//! Ported from `hl-gpu/src/ptx.rs` — the PARSER (`compile` + helpers) only. The CPU interpreter and the
//! WGSL back-end that shared that file stay host-side (they belong to the executor, not the driver), and
//! the kernel-IR value types ([`KernelProgram`], [`Inst`], [`Op`], [`Param`], the `SR_*`/`CMP_*`/… codes)
//! now live in the neutral protocol ([`hl_gpu::protocol::model::kernel`]); this port re-points its
//! imports there. The one behavioural change is the error channel: a parse failure is the protocol's
//! typed [`GpuError::Kernel`] (the old crate's `GpuError::Ptx`).
//!
//! ## Modeled SIMT subset (enough for vector-add / saxpy / elementwise / shared-mem reductions)
//! special registers `%tid.*`/`%ntid.*`/`%ctaid.*`/`%nctaid.*`; `ld.param.{u32,u64,f32}`,
//! `cvta.to.global`; integer ALU (`mov`/`add`/`sub`/`mul`/`mad` on s32/u32 + `.wide`, `shl`/`shr`,
//! `and`/`or`/`xor`); f32 ALU (`add`/`sub`/`mul`/`fma.rn`, a couple of `cvt` forms); predication + branch
//! (`setp.*`, `@p bra`/`@!p bra`/`bra`, labels); `ld/st.global.*`, `ld/st.shared.*`, `atom/red.*`,
//! `bar.sync`, `ret`. Anything outside the subset is a typed [`GpuError::Kernel`].

use hl_gpu::protocol::model::kernel::*;
use hl_gpu::{GpuError, Result};

mod body;
mod entry;
mod instruction;
mod model;
mod operand;
mod register;

use model::{Interner, SRegAlloc};

struct Ptx;

impl Ptx {
    fn error(message: impl Into<String>) -> GpuError {
        GpuError::Kernel(message.into())
    }

    fn align(value: u32, alignment: u32) -> u32 {
        if alignment == 0 {
            return value;
        }
        (value + alignment - 1) & !(alignment - 1)
    }
}

/// Canonical nvcc-style PTX (sm_86) for `vecadd(const float* a, const float* b, float* c, int n)`:
/// `c[i] = a[i] + b[i]` with the standard `mad`-computed global index and an `if (i >= n) return;`
/// bounds guard. The reference kernel the lowering tests compile end-to-end.
pub const VECADD_PTX: &str = r#"
    .version 7.5
    .target sm_86
    .address_size 64

    .visible .entry vecadd(
        .param .u64 vecadd_param_0,
        .param .u64 vecadd_param_1,
        .param .u64 vecadd_param_2,
        .param .u32 vecadd_param_3
    )
    {
        .reg .pred  %p<2>;
        .reg .f32   %f<4>;
        .reg .b32   %r<6>;
        .reg .b64   %rd<11>;

        ld.param.u64  %rd1, [vecadd_param_0];
        ld.param.u64  %rd2, [vecadd_param_1];
        ld.param.u64  %rd3, [vecadd_param_2];
        ld.param.u32  %r2,  [vecadd_param_3];
        mov.u32       %r3, %ntid.x;
        mov.u32       %r4, %ctaid.x;
        mov.u32       %r5, %tid.x;
        mad.lo.s32    %r1, %r4, %r3, %r5;
        setp.ge.s32   %p1, %r1, %r2;
        @%p1 bra      DONE;

        cvta.to.global.u64 %rd4, %rd1;
        cvta.to.global.u64 %rd5, %rd2;
        cvta.to.global.u64 %rd6, %rd3;
        mul.wide.s32  %rd7, %r1, 4;
        add.s64       %rd8, %rd4, %rd7;
        add.s64       %rd9, %rd5, %rd7;
        add.s64       %rd10, %rd6, %rd7;
        ld.global.f32 %f1, [%rd8];
        ld.global.f32 %f2, [%rd9];
        add.f32       %f3, %f1, %f2;
        st.global.f32 [%rd10], %f3;

    DONE:
        ret;
    }
"#;

/// A raw (pre-classification) parse of one entry: the instruction list, the register count, the
/// `ld.param` taint seeds, and the declared shared-memory size.
struct RawFn {
    insts: Vec<Inst>,
    reg_count: u16,
    ld_param: Vec<(u16, u16)>, // (dst reg, param ordinal) — taint seeds
    shared_bytes: u32,         // total .shared bytes declared in this entry (word-rounded)
}

/// Compile a single `.entry <entry>` from PTX `source` into a [`KernelProgram`] with block dims `block`.
pub fn compile(source: &str, entry: &str, block: [u32; 3]) -> Result<KernelProgram> {
    let (params_src, body_src) = Ptx::extract_entry(source, entry)?;
    let params = Ptx::parse_params(&params_src)?;

    let mut interner = Interner::default();
    let raw = Ptx::parse_body(&body_src, &params, &mut interner)?;

    // classify pointer parameters via a single forward taint pass.
    let mut param_is_ptr = vec![false; params.len()];
    let mut taint: Vec<Option<u16>> = vec![None; raw.reg_count as usize];
    for &(d, p) in &raw.ld_param {
        taint[d as usize] = Some(p);
    }
    let carry = |t: &mut Vec<Option<u16>>, d: u16, srcs: &[Option<u16>]| {
        for s in srcs {
            if s.is_some() {
                t[d as usize] = *s;
                return;
            }
        }
    };
    let taint_of = |t: &Vec<Option<u16>>, op: &Op| -> Option<u16> {
        match op {
            Op::Reg(r) => t[*r as usize],
            _ => None,
        }
    };
    for inst in &raw.insts {
        match inst {
            Inst::LdParam { d, param } => taint[*d as usize] = Some(*param),
            Inst::MovReg { d, s } => taint[*d as usize] = taint[*s as usize],
            Inst::Cvta { d, s } => {
                let tp = taint[*s as usize];
                taint[*d as usize] = tp;
                if let Some(p) = tp {
                    param_is_ptr[p as usize] = true; // cvta.to.global marks a pointer param
                }
            }
            Inst::IAdd { d, a, b, .. } | Inst::ISub { d, a, b, .. } => {
                let (ta, tb) = (taint_of(&taint, a), taint_of(&taint, b));
                carry(&mut taint, *d, &[ta, tb]);
            }
            Inst::IMad { d, a, b, c } => {
                let t = [
                    taint_of(&taint, a),
                    taint_of(&taint, b),
                    taint_of(&taint, c),
                ];
                carry(&mut taint, *d, &t);
            }
            Inst::IMul { d, a, b, .. } => {
                let t = [taint_of(&taint, a), taint_of(&taint, b)];
                carry(&mut taint, *d, &t);
            }
            Inst::LdGlobal { addr, .. }
            | Inst::StGlobal { addr, .. }
            | Inst::AtomGlobal { addr, .. } => {
                if let Some(p) = taint[*addr as usize] {
                    param_is_ptr[p as usize] = true; // direct global access marks a pointer param
                }
            }
            _ => {}
        }
    }

    // finalize param layout: offsets (natural alignment) + dense region ids for pointer params.
    let mut offset = 0u32;
    let mut region = 0u32;
    let mut out_params = Vec::with_capacity(params.len());
    for (i, (_, width)) in params.iter().enumerate() {
        offset = Ptx::align(offset, *width);
        let is_ptr = param_is_ptr[i];
        let r = if is_ptr {
            let r = region;
            region += 1;
            r
        } else {
            0
        };
        out_params.push(Param {
            width: *width,
            offset,
            is_ptr,
            region: r,
        });
        offset += *width;
    }

    Ok(KernelProgram {
        entry: entry.to_string(),
        block,
        params: out_params,
        param_bytes: offset,
        num_regions: region,
        shared_bytes: raw.shared_bytes,
        reg_count: raw.reg_count,
        insts: raw.insts,
    })
}
