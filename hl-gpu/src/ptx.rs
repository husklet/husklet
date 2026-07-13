//! PTX → dd-GPU **kernel IR** front-end + a CPU interpreter — the compute core that lets a real
//! CUDA PTX kernel execute end-to-end through dd's stack on the [`crate::software::SoftwareBackend`]
//! with **no GPU**, as the standing correctness oracle for the future Metal backend.
//!
//! ## What this is (and the honest boundary)
//! On a real Apple-silicon host the kernel path is `PTX → SPIR-V → MSL → AIR` (host-side, research-grade
//! — see `docs/ideas/CUDA_ON_METAL.md §5`). Here, as the *oracle*, we instead lower a **bounded, tested
//! subset of PTX** to a compact internal op list ([`KernelProgram`]) and interpret it per-thread on the
//! CPU. Both are "compile PTX to something a [`crate::backend::GpuBackend`] runs"; the trait is the seam,
//! so `software.rs` (this interpreter) and a future `metal.rs` (SPIR-V/MSL) are interchangeable.
//!
//! ## Modeled SIMT subset (enough for vector-add / saxpy / elementwise)
//! * special registers: `%tid.{x,y,z}`, `%ntid.*`, `%ctaid.*`, `%nctaid.*`;
//! * `ld.param.{u32,u64,f32}` (kernel arguments), `cvta.to.global`;
//! * integer ALU: `mov`, `add/sub/mul/mad(.lo)` on s32/u32 and `.wide` widening, address arithmetic;
//! * f32 ALU: `add/sub/mul/fma.rn`; a couple of `cvt` forms;
//! * predication + branch: `setp.{ge,gt,le,lt,eq,ne}.s32`, `@p bra` / `@!p bra` / `bra`, labels — the
//!   bounds guard `if (i >= n) return;`;
//! * `ld.global.{f32,u32,u64}` / `st.global.*`, `ret`, and `bar.sync` (a no-op here — no shared memory).
//!
//! Anything outside this subset (warp intrinsics `shfl`/`vote`, shared memory, atomics, `printf`, f64,
//! textures, inline `asm`, dynamic parallelism) is deliberately rejected with a typed [`GpuError::Ptx`]
//! — the long tail lives on the Metal host.
//!
//! ## Memory model
//! Each kernel **pointer parameter** becomes its own storage-buffer "region" (binding), exactly like a
//! Metal `device float*` argument. A register holding a global address is a tagged pointer
//! `Ptr{region, offset}`; address arithmetic updates `offset`; `ld/st.global` index that region's bytes.
//! This models CUDA's per-allocation access faithfully; the one gap (a kernel chasing raw pointers
//! *across* allocations under CUDA's flat unified VA) is the documented Metal limitation.

use crate::wire::{Decoder, Encoder};
use crate::{GpuError, Result};

fn err(m: impl Into<String>) -> GpuError {
    GpuError::Ptx(m.into())
}

/// Canonical nvcc-style PTX (sm_86) for `vecadd(const float* a, const float* b, float* c, int n)`:
/// `c[i] = a[i] + b[i]` with the standard `mad`-computed global index and an `if (i >= n) return;`
/// bounds guard. Used by the end-to-end tests + the `hl-gpu/cuda` dlopen ABI test as the reference
/// kernel that executes all the way through dd's stack on the software backend.
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
        @%p1 bra      $L__BB0_2;

        cvta.to.global.u64 %rd4, %rd1;
        mul.wide.s32  %rd5, %r1, 4;
        add.s64       %rd6, %rd4, %rd5;
        cvta.to.global.u64 %rd7, %rd2;
        add.s64       %rd8, %rd7, %rd5;
        ld.global.f32 %f1, [%rd8];
        ld.global.f32 %f2, [%rd6];
        add.f32       %f3, %f2, %f1;
        cvta.to.global.u64 %rd9, %rd3;
        add.s64       %rd10, %rd9, %rd5;
        st.global.f32 [%rd10], %f3;

    $L__BB0_2:
        ret;
    }
"#;

/// Canonical block-level **reduction** PTX for `block_reduce(const int* in, int* out, int n)`:
/// each thread block sums up to `blockDim.x` elements of `in` into shared memory, does a `bar.sync`
/// tree reduction, and thread 0 `atomicAdd`s the block partial into `*out`. It exercises the whole
/// shared-memory + barrier + atomic subset in one kernel and is the reduction oracle test. Hand-written
/// (like [`VECADD_PTX`]) so the exact addressing forms stay within the modeled subset. Launch with
/// `block = (256,1,1)` and shared size 1024 bytes (declared `.shared` below).
pub const REDUCE_PTX: &str = r#"
    .version 7.5
    .target sm_86
    .address_size 64

    .visible .entry block_reduce(
        .param .u64 block_reduce_param_0,
        .param .u64 block_reduce_param_1,
        .param .u32 block_reduce_param_2
    )
    {
        .reg .pred  %p<4>;
        .reg .b32   %r<20>;
        .reg .b64   %rd<8>;
        .shared .align 4 .b8 sdata[1024];

        ld.param.u64  %rd1, [block_reduce_param_0];
        ld.param.u64  %rd2, [block_reduce_param_1];
        ld.param.u32  %r2,  [block_reduce_param_2];

        mov.u32       %r3, %ntid.x;
        mov.u32       %r4, %ctaid.x;
        mov.u32       %r5, %tid.x;
        mad.lo.s32    %r6, %r4, %r3, %r5;      // i = blockIdx*blockDim + tid

        mul.lo.s32    %r8, %r5, 4;             // tid*4
        mov.u32       %r9, sdata;              // shared base offset
        add.s32       %r10, %r9, %r8;          // &sdata[tid]

        setp.ge.s32   %p1, %r6, %r2;
        @%p1 bra      $LOAD_ZERO;
        cvta.to.global.u64 %rd3, %rd1;
        mul.wide.s32  %rd4, %r6, 4;
        add.s64       %rd5, %rd3, %rd4;
        ld.global.u32 %r11, [%rd5];            // input[i]
        bra           $HAVE_VAL;
    $LOAD_ZERO:
        mov.u32       %r11, 0;
    $HAVE_VAL:
        st.shared.u32 [%r10], %r11;
        bar.sync      0;

        shr.u32       %r12, %r3, 1;            // s = blockDim >> 1
    $LOOP:
        setp.eq.s32   %p2, %r12, 0;
        @%p2 bra      $DONE;
        setp.ge.u32   %p3, %r5, %r12;          // tid >= s ?
        @%p3 bra      $SKIP;
        mul.lo.s32    %r13, %r12, 4;           // s*4
        add.s32       %r14, %r10, %r13;        // &sdata[tid+s]
        ld.shared.u32 %r15, [%r10];
        ld.shared.u32 %r16, [%r14];
        add.s32       %r17, %r15, %r16;
        st.shared.u32 [%r10], %r17;
    $SKIP:
        bar.sync      0;
        shr.u32       %r12, %r12, 1;           // s >>= 1
        bra           $LOOP;
    $DONE:
        setp.ne.s32   %p4, %r5, 0;
        @%p4 bra      $END;
        ld.shared.u32 %r18, [%r10];            // sdata[0]  (tid==0 → %r10 == base)
        cvta.to.global.u64 %rd6, %rd2;
        atom.global.add.u32 %r19, [%rd6], %r18;
    $END:
        ret;
    }
"#;

// ===================================================================================================
// compiled kernel IR
// ===================================================================================================

/// Scalar/pointer type tag for a `ld.global`/`st.global` access.
pub mod gty {
    pub const F32: u8 = 0;
    pub const U32: u8 = 1;
    pub const U64: u8 = 2;
}

/// One kernel parameter, in CUDA ABI order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Param {
    /// Byte width in the flat parameter blob (4 for u32/f32, 8 for u64).
    pub width: u32,
    /// Byte offset of this parameter within the flat parameter blob (natural alignment).
    pub offset: u32,
    /// True if this parameter is a device pointer (reaches a `cvta.to.global` / global memory access).
    pub is_ptr: bool,
    /// Dense storage-binding index among pointer parameters (only meaningful when `is_ptr`).
    pub region: u32,
}

/// An instruction operand: an interned register, or an immediate.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Reg(u16),
    ImmI(i64),
    ImmF(u32), // f32 bits
}

/// One compiled instruction over an interned register file.
#[derive(Clone, PartialEq, Debug)]
pub enum Inst {
    MovImmI { d: u16, imm: u64 },
    MovImmF { d: u16, bits: u32 },
    MovReg { d: u16, s: u16 },
    /// Read a special register into `d`. `sreg` is one of the `SR_*` constants.
    MovSReg { d: u16, sreg: u8 },
    LdParam { d: u16, param: u16 },
    Cvta { d: u16, s: u16 },
    /// Integer add. Pointer-aware: `Ptr + int → Ptr` with offset advanced. `wide` keeps 64 bits.
    IAdd { d: u16, a: Op, b: Op, wide: bool },
    ISub { d: u16, a: Op, b: Op, wide: bool },
    /// `d = a*b + c` (low 32 bits).
    IMad { d: u16, a: Op, b: Op, c: Op },
    /// Integer multiply. `wide` = 32×32→64; else low 32. `unsigned` selects zero- vs sign-extension
    /// for the `.wide` form (`mul.wide.u32` vs `mul.wide.s32`) — the low-32 form is sign-agnostic.
    IMul { d: u16, a: Op, b: Op, wide: bool, unsigned: bool },
    /// Set predicate `d` = (a `cmp` b). `cmp` is a `CMP_*` constant; `unsigned` picks the u32 vs s32
    /// comparison (`setp.lt.u32` must compare unsigned, not signed).
    Setp { d: u16, a: Op, b: Op, cmp: u8, unsigned: bool },
    /// Branch to instruction index `target`, optionally guarded by predicate reg (negated if `.1`).
    Bra { target: u32, pred: Option<(u16, bool)> },
    LdGlobal { d: u16, addr: u16, off: i64, ty: u8 },
    StGlobal { addr: u16, off: i64, src: Op, ty: u8 },
    /// `ld.shared` — read a 32-bit word from workgroup shared memory. `base`+`off` is the byte offset
    /// into the block's shared-memory array (see [`KernelProgram::shared_bytes`]).
    LdShared { d: u16, base: Op, off: i64, ty: u8 },
    /// `st.shared` — write a 32-bit word to workgroup shared memory.
    StShared { base: Op, off: i64, src: Op, ty: u8 },
    /// `atom.global.<op>` / `red.global.<op>` — atomic read-modify-write on a pointer region. `d` is the
    /// (optional) old-value destination; `cmp` is the compare operand (used only for `ATOM_CAS`).
    AtomGlobal { d: Option<u16>, addr: u16, off: i64, op: u8, cmp: Op, val: Op, unsigned: bool },
    /// `atom.shared.<op>` / `red.shared.<op>` — atomic read-modify-write on workgroup shared memory.
    AtomShared { d: Option<u16>, base: Op, off: i64, op: u8, cmp: Op, val: Op, unsigned: bool },
    /// Logical/arithmetic shift. `dir` is `SHIFT_*`; `unsigned` selects logical (`shr.u*`) vs arithmetic
    /// (`shr.s*`) right shift. Left shift ignores `unsigned`.
    Shift { d: u16, a: Op, b: Op, dir: u8, unsigned: bool },
    /// Bitwise `and`/`or`/`xor` (`op` is `BIT_*`).
    BitOp { d: u16, a: Op, b: Op, op: u8 },
    /// `bar.sync` — a workgroup execution+memory barrier (real, not a no-op: shared-memory kernels rely
    /// on it). The interpreter rendezvouses the block here; the WGSL back-end lowers it via
    /// `workgroupBarrier()`.
    Bar,
    FAdd { d: u16, a: Op, b: Op },
    FSub { d: u16, a: Op, b: Op },
    FMul { d: u16, a: Op, b: Op },
    FFma { d: u16, a: Op, b: Op, c: Op },
    /// `cvt` conversions we model: see `CVT_*`.
    Cvt { d: u16, s: Op, kind: u8 },
    Ret,
    Nop,
}

// special-register ids
pub const SR_TID_X: u8 = 0;
pub const SR_TID_Y: u8 = 1;
pub const SR_TID_Z: u8 = 2;
pub const SR_NTID_X: u8 = 3;
pub const SR_NTID_Y: u8 = 4;
pub const SR_NTID_Z: u8 = 5;
pub const SR_CTAID_X: u8 = 6;
pub const SR_CTAID_Y: u8 = 7;
pub const SR_CTAID_Z: u8 = 8;
pub const SR_NCTAID_X: u8 = 9;
pub const SR_NCTAID_Y: u8 = 10;
pub const SR_NCTAID_Z: u8 = 11;

// comparison ops
pub const CMP_EQ: u8 = 0;
pub const CMP_NE: u8 = 1;
pub const CMP_LT: u8 = 2;
pub const CMP_LE: u8 = 3;
pub const CMP_GT: u8 = 4;
pub const CMP_GE: u8 = 5;

// cvt kinds
pub const CVT_F32_FROM_S32: u8 = 0;
pub const CVT_S64_FROM_S32: u8 = 1;
pub const CVT_S32_FROM_F32: u8 = 2;
pub const CVT_IDENTITY: u8 = 3;

// atomic ops (see [`Inst::AtomGlobal`] / [`Inst::AtomShared`]). All operate on 32-bit words.
pub const ATOM_ADD: u8 = 0;
pub const ATOM_MIN: u8 = 1;
pub const ATOM_MAX: u8 = 2;
pub const ATOM_AND: u8 = 3;
pub const ATOM_OR: u8 = 4;
pub const ATOM_XOR: u8 = 5;
pub const ATOM_EXCH: u8 = 6;
pub const ATOM_CAS: u8 = 7;

// bitwise-shift directions (see [`Inst::Shift`]).
pub const SHIFT_LEFT: u8 = 0;
pub const SHIFT_RIGHT: u8 = 1;

// bitwise binary ops (see [`Inst::BitOp`]).
pub const BIT_AND: u8 = 0;
pub const BIT_OR: u8 = 1;
pub const BIT_XOR: u8 = 2;

/// A compiled kernel: the entry name, threadgroup (block) dims baked in as the WebGPU-style
/// `local_size`, the parameter layout, and the instruction list over `reg_count` registers.
#[derive(Clone, PartialEq, Debug)]
pub struct KernelProgram {
    pub entry: String,
    pub block: [u32; 3],
    pub params: Vec<Param>,
    /// Total size of the flat parameter blob (binding 0).
    pub param_bytes: u32,
    /// Number of pointer regions (storage bindings 1..=num_regions).
    pub num_regions: u32,
    /// Total workgroup shared-memory size in bytes (from the kernel's `.shared` declarations), rounded
    /// up to a 4-byte word. `0` for kernels that use no shared memory. The WGSL back-end sizes a
    /// `var<workgroup>` array from this; the interpreter allocates a per-block byte array.
    pub shared_bytes: u32,
    pub reg_count: u16,
    pub insts: Vec<Inst>,
}

// ===================================================================================================
// descriptor codec — how a compiled kernel crosses the dd-GPU IR shader channel (`CreateShader` words)
// ===================================================================================================

/// Magic leading word marking `CreateShader.spirv` words as a dd-GPU **kernel descriptor** (PTX text +
/// launch config) rather than SPIR-V. A software/oracle backend compiles it here; a Metal backend would
/// instead carry real SPIR-V. This is the honest per-backend shader-ABI seam.
pub const KERNEL_MAGIC: u32 = 0xDD6B_0001;

/// The guest-forwarded kernel descriptor: the PTX source, the entry point, and the launch block dims.
/// The host compiles PTX → [`KernelProgram`] (matching the doc's "forward PTX, translate host-side").
#[derive(Clone, PartialEq, Debug)]
pub struct KernelDescriptor {
    pub ptx: String,
    pub entry: String,
    pub block: [u32; 3],
}

impl KernelDescriptor {
    /// Serialize into `CreateShader` shader words: `[MAGIC, byte_len, ...packed bytes...]`.
    pub fn to_words(&self) -> Vec<u32> {
        let mut e = Encoder::new();
        e.str(&self.ptx);
        e.str(&self.entry);
        for v in self.block {
            e.u32(v);
        }
        let bytes = e.into_vec();
        let mut words = Vec::with_capacity(2 + bytes.len() / 4 + 1);
        words.push(KERNEL_MAGIC);
        words.push(bytes.len() as u32);
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(b));
        }
        words
    }

    /// Decode from shader words. Returns `None` if the words are not a kernel descriptor (i.e. SPIR-V).
    pub fn from_words(words: &[u32]) -> Option<Result<Self>> {
        if words.len() < 2 || words[0] != KERNEL_MAGIC {
            return None;
        }
        let byte_len = words[1] as usize;
        let mut bytes = Vec::with_capacity((words.len() - 2) * 4);
        for &w in &words[2..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() < byte_len {
            return Some(Err(err("kernel descriptor truncated")));
        }
        bytes.truncate(byte_len);
        let mut d = Decoder::new(&bytes);
        Some((|| {
            let ptx = d.str()?;
            let entry = d.str()?;
            let block = [d.u32()?, d.u32()?, d.u32()?];
            Ok(KernelDescriptor { ptx, entry, block })
        })())
    }
}

// ===================================================================================================
// front-end: PTX text → KernelProgram
// ===================================================================================================

/// A raw (pre-interning) instruction, register operands kept as interned indices already; only used
/// as an intermediate so classification (pointer-vs-scalar params) can run before we finalize.
struct RawFn {
    insts: Vec<Inst>,
    reg_count: u16,
    ld_param: Vec<(u16, u16)>, // (dst reg, param ordinal) — taint seeds
    shared_bytes: u32,         // total .shared bytes declared in this entry (word-rounded)
}

/// Compile a single `.entry <entry>` from PTX `source` into a [`KernelProgram`] with block dims `block`.
pub fn compile(source: &str, entry: &str, block: [u32; 3]) -> Result<KernelProgram> {
    let (params_src, body_src) = extract_entry(source, entry)?;
    let params = parse_params(&params_src)?;

    let mut interner = Interner::default();
    let raw = parse_body(&body_src, &params, &mut interner)?;

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
                let t = [taint_of(&taint, a), taint_of(&taint, b), taint_of(&taint, c)];
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
        offset = align_up(offset, *width);
        let is_ptr = param_is_ptr[i];
        let r = if is_ptr {
            let r = region;
            region += 1;
            r
        } else {
            0
        };
        out_params.push(Param { width: *width, offset, is_ptr, region: r });
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

fn align_up(v: u32, a: u32) -> u32 {
    if a == 0 {
        return v;
    }
    (v + a - 1) & !(a - 1)
}

/// Slice out `(param_list, body)` for `.entry <entry>` (or `.visible .entry <entry>`).
fn extract_entry(source: &str, entry: &str) -> Result<(String, String)> {
    // find ".entry <entry>" then the (...) param list and the {...} body.
    let bytes = source;
    let mut search = 0usize;
    loop {
        let idx = bytes[search..]
            .find(".entry")
            .ok_or_else(|| err(format!("entry `{entry}` not found")))?
            + search;
        let after = &bytes[idx + ".entry".len()..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name == entry {
            let rest = &bytes[idx..];
            let lp = rest.find('(');
            let lb = rest
                .find('{')
                .ok_or_else(|| err(format!("entry `{entry}` has no body")))?;
            // a param list exists only if '(' precedes '{'
            let params_src = if let Some(lp) = lp {
                if lp < lb {
                    let rp = rest[lp..].find(')').ok_or_else(|| err("unterminated param list"))? + lp;
                    rest[lp + 1..rp].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            // body: matched braces from lb.
            let body = matched_braces(&rest[lb..])?;
            return Ok((params_src, body));
        }
        search = idx + ".entry".len();
    }
}

/// Return the substring inside the first `{...}` (brace-matched), excluding the outer braces.
fn matched_braces(s: &str) -> Result<String> {
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = Some(i + 1);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let st = start.unwrap();
                    return Ok(s[st..i].to_string());
                }
            }
            _ => {}
        }
    }
    Err(err("unterminated kernel body"))
}

/// Parse `.param .TYPE name` entries (comma-separated) into `(name, width)`.
fn parse_params(src: &str) -> Result<Vec<(String, u32)>> {
    let mut out = Vec::new();
    for raw in src.split(',') {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let toks: Vec<&str> = s.split_whitespace().collect();
        // expect [.param, .TYPE, name] — tolerate extra qualifiers before the name.
        if toks.is_empty() || toks[0] != ".param" {
            return Err(err(format!("bad .param decl: `{s}`")));
        }
        // find the type token (starts with '.') and the name (last token, may carry `[..]`).
        let ty = toks
            .iter()
            .skip(1)
            .find(|t| t.starts_with('.'))
            .ok_or_else(|| err(format!("param missing type: `{s}`")))?;
        let width = type_width(ty)?;
        let name_tok = *toks.last().unwrap();
        if name_tok.starts_with('.') {
            return Err(err(format!("param missing name: `{s}`")));
        }
        let name: String = name_tok
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name.is_empty() {
            return Err(err(format!("param missing name: `{s}`")));
        }
        // arrays / by-value structs (`.b8 name[32]`) are out of the modeled subset.
        if name_tok.contains('[') {
            return Err(err(format!("array/struct param unsupported: `{s}`")));
        }
        out.push((name, width));
    }
    Ok(out)
}

fn type_width(ty: &str) -> Result<u32> {
    Ok(match ty {
        ".u8" | ".s8" | ".b8" => 1,
        ".u16" | ".s16" | ".b16" | ".f16" => 2,
        ".u32" | ".s32" | ".b32" | ".f32" => 4,
        ".u64" | ".s64" | ".b64" | ".f64" => 8,
        other => return Err(err(format!("unsupported param type `{other}`"))),
    })
}

#[derive(Default)]
struct Interner {
    map: std::collections::HashMap<String, u16>,
}
impl Interner {
    fn get(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.map.get(name) {
            return i;
        }
        let i = self.map.len() as u16;
        self.map.insert(name.to_string(), i);
        i
    }
    fn count(&self) -> u16 {
        self.map.len() as u16
    }
}

/// Strip PTX comments: `//` line comments and `/* … */` block comments (as nvcc-emitted PTX carries).
/// Replaces each comment with a single space so token boundaries are preserved.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            out.push(' ');
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

fn parse_body(
    body: &str,
    params: &[(String, u32)],
    interner: &mut Interner,
) -> Result<RawFn> {
    // statement split: strip comments, newlines→spaces, then split on ';'. Labels (`X:`) carry no ';'
    // and get folded onto the following statement; we peel leading `label:` tokens off.
    let flat = strip_comments(body).replace('\n', " ").replace('\r', " ");
    let raw_stmts: Vec<&str> = flat.split(';').collect();

    // First pass: collect (label_defs, instructions_as_text) so we can resolve labels → indices.
    // Along the way, resolve `.shared` variable declarations to (name → byte offset) with a running,
    // naturally-aligned shared-memory cursor — the block's shared array layout.
    let mut labels: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut stmts: Vec<String> = Vec::new();
    let mut shared_syms: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut shared_cursor: u32 = 0;
    for stmt in raw_stmts {
        let mut s = stmt.trim().to_string();
        if s.is_empty() {
            continue;
        }
        // peel any number of leading `label:` tokens.
        loop {
            let mut it = s.splitn(2, char::is_whitespace);
            let first = it.next().unwrap_or("");
            if let Some(name) = first.strip_suffix(':') {
                labels.insert(name.to_string(), stmts.len() as u32);
                s = it.next().unwrap_or("").trim().to_string();
                if s.is_empty() {
                    break;
                }
            } else {
                break;
            }
        }
        if s.is_empty() {
            continue;
        }
        // `.shared` state-space declaration → reserve space + record the symbol's base offset.
        if s.starts_with(".shared") {
            parse_shared_decl(&s, &mut shared_syms, &mut shared_cursor)?;
            continue;
        }
        // skip other directives inside the body (e.g. `.reg .b32 %r<6>`, `.loc ...`).
        if s.starts_with('.') {
            continue;
        }
        stmts.push(s);
    }
    let shared_bytes = align_up(shared_cursor, 4);

    let param_idx: std::collections::HashMap<&str, u16> = params
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i as u16))
        .collect();

    let mut insts = Vec::with_capacity(stmts.len());
    let mut ld_param = Vec::new();
    for s in &stmts {
        let inst = parse_inst(s, &param_idx, &labels, &shared_syms, interner)?;
        if let Inst::LdParam { d, param } = inst {
            ld_param.push((d, param));
        }
        insts.push(inst);
    }

    Ok(RawFn {
        insts,
        reg_count: interner.count(),
        ld_param,
        shared_bytes,
    })
}

/// Parse a `.shared` variable declaration, reserving space in the block shared array and recording the
/// symbol's base byte offset. Handles both `.shared .align A .b8 name[bytes];` (nvcc's canonical form)
/// and typed arrays/scalars `.shared .TYPE name[count];` / `.shared .TYPE name;`. Dynamic `extern`
/// shared (`.shared .align A .b8 name[]`, sized by `cuLaunchKernel`'s `sharedMemBytes`) is rejected —
/// out of the statically-sized subset this lowering models.
fn parse_shared_decl(
    s: &str,
    syms: &mut std::collections::HashMap<String, u32>,
    cursor: &mut u32,
) -> Result<()> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    // find the element type (`.b8`, `.u32`, `.f32`, …) and the trailing `name[count]` / `name` token.
    let ty = toks
        .iter()
        .skip(1)
        .find(|t| t.starts_with('.') && *t != &".align" && *t != &".shared")
        .copied();
    let name_tok = *toks.last().ok_or_else(|| err(format!("bad .shared decl: `{s}`")))?;
    let elem = ty.map(type_width).transpose()?.unwrap_or(1);
    // split `name[count]`
    let (name, count) = if let Some(lb) = name_tok.find('[') {
        let name = &name_tok[..lb];
        let inner = name_tok[lb + 1..].trim_end_matches([']', ';']).trim();
        if inner.is_empty() {
            return Err(err(format!("dynamic (extern) .shared unsupported: `{s}`")));
        }
        let count: u32 = parse_imm_i(inner)
            .map_err(|_| err(format!("bad .shared array size: `{s}`")))? as u32;
        (name, count)
    } else {
        (name_tok.trim_end_matches(';'), 1u32)
    };
    let name: String = name.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
    if name.is_empty() {
        return Err(err(format!(".shared decl missing name: `{s}`")));
    }
    // natural-align the symbol to its element width, then reserve elem*count bytes.
    *cursor = align_up(*cursor, elem);
    syms.insert(name, *cursor);
    *cursor += elem.saturating_mul(count.max(1));
    Ok(())
}

fn parse_inst(
    stmt: &str,
    params: &std::collections::HashMap<&str, u16>,
    labels: &std::collections::HashMap<String, u32>,
    shared_syms: &std::collections::HashMap<String, u32>,
    interner: &mut Interner,
) -> Result<Inst> {
    // optional predicate guard: `@%p1` or `@!%p1`
    let mut rest = stmt.trim();
    let mut guard: Option<(u16, bool)> = None;
    if let Some(after) = rest.strip_prefix('@') {
        let (pred_tok, tail) = split_first(after);
        let (neg, pname) = if let Some(p) = pred_tok.strip_prefix('!') {
            (true, p)
        } else {
            (false, pred_tok)
        };
        guard = Some((interner.get(strip_reg(pname)), neg));
        rest = tail.trim();
    }

    let (opcode, args_str) = split_first(rest);
    let ops: Vec<String> = split_operands(args_str);

    // dotted opcode: base + modifiers
    let base = opcode.split('.').next().unwrap_or(opcode);
    let has = |m: &str| opcode.split('.').any(|x| x == m);

    let reg = |i: &mut Interner, tok: &str| -> u16 { i.get(strip_reg(tok)) };

    macro_rules! need {
        ($n:expr) => {
            if ops.len() < $n {
                return Err(err(format!("`{opcode}` expects {} operands: `{stmt}`", $n)));
            }
        };
    }

    let inst = match base {
        "ret" => Inst::Ret,
        "bar" | "barrier" => Inst::Bar, // bar.sync / barrier.sync — workgroup rendezvous
        "membar" | "fence" => Inst::Nop, // memory fence — the barrier already orders shared memory
        "bra" => {
            need!(1);
            let target = *labels
                .get(strip_label(&ops[0]))
                .ok_or_else(|| err(format!("unknown branch label `{}`", ops[0])))?;
            Inst::Bra { target, pred: guard }
        }
        "mov" => {
            need!(2);
            let d = reg(interner, &ops[0]);
            let src = &ops[1];
            if let Some(sr) = parse_sreg(src) {
                Inst::MovSReg { d, sreg: sr }
            } else if is_reg(src) {
                Inst::MovReg { d, s: reg(interner, src) }
            } else if let Some(&off) = shared_syms.get(src.trim()) {
                // `mov.u32 %r, sdata` — the address of a shared variable is its byte offset in the
                // block shared array (a plain integer the shared ld/st then indexes).
                Inst::MovImmI { d, imm: off as u64 }
            } else if has("f32") {
                Inst::MovImmF { d, bits: parse_imm_f(src)? }
            } else {
                Inst::MovImmI { d, imm: parse_imm_i(src)? as u64 }
            }
        }
        "ld" => {
            need!(2);
            let d = reg(interner, &ops[0]);
            if has("param") {
                let name = strip_mem_name(&ops[1]);
                let p = *params
                    .get(name)
                    .ok_or_else(|| err(format!("unknown param `{name}`")))?;
                Inst::LdParam { d, param: p }
            } else if has("global") {
                let (addr, off) = parse_mem(&ops[1], interner)?;
                Inst::LdGlobal { d, addr, off, ty: gtype(opcode)? }
            } else if has("shared") {
                let (base, off) = parse_shared_mem(&ops[1], shared_syms, interner)?;
                Inst::LdShared { d, base, off, ty: gtype(opcode)? }
            } else {
                return Err(err(format!("unsupported ld space: `{stmt}`")));
            }
        }
        "st" => {
            need!(2);
            if has("global") {
                let (addr, off) = parse_mem(&ops[0], interner)?;
                let src = parse_op(&ops[1], interner, has("f32"))?;
                Inst::StGlobal { addr, off, src, ty: gtype(opcode)? }
            } else if has("shared") {
                let (base, off) = parse_shared_mem(&ops[0], shared_syms, interner)?;
                let src = parse_op(&ops[1], interner, has("f32"))?;
                Inst::StShared { base, off, src, ty: gtype(opcode)? }
            } else {
                return Err(err(format!("unsupported st space: `{stmt}`")));
            }
        }
        "cvta" => {
            need!(2);
            Inst::Cvta { d: reg(interner, &ops[0]), s: reg(interner, &ops[1]) }
        }
        "add" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            if has("f32") {
                Inst::FAdd { d, a: parse_op(&ops[1], interner, true)?, b: parse_op(&ops[2], interner, true)? }
            } else {
                Inst::IAdd {
                    d,
                    a: parse_op(&ops[1], interner, false)?,
                    b: parse_op(&ops[2], interner, false)?,
                    wide: has("s64") || has("u64") || has("b64"),
                }
            }
        }
        "sub" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            if has("f32") {
                Inst::FSub { d, a: parse_op(&ops[1], interner, true)?, b: parse_op(&ops[2], interner, true)? }
            } else {
                Inst::ISub {
                    d,
                    a: parse_op(&ops[1], interner, false)?,
                    b: parse_op(&ops[2], interner, false)?,
                    wide: has("s64") || has("u64") || has("b64"),
                }
            }
        }
        "mul" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            if has("f32") {
                Inst::FMul { d, a: parse_op(&ops[1], interner, true)?, b: parse_op(&ops[2], interner, true)? }
            } else {
                Inst::IMul {
                    d,
                    a: parse_op(&ops[1], interner, false)?,
                    b: parse_op(&ops[2], interner, false)?,
                    wide: has("wide"),
                    unsigned: has("u32") || has("u16") || has("u8"),
                }
            }
        }
        "mad" => {
            need!(4);
            let d = reg(interner, &ops[0]);
            Inst::IMad {
                d,
                a: parse_op(&ops[1], interner, false)?,
                b: parse_op(&ops[2], interner, false)?,
                c: parse_op(&ops[3], interner, false)?,
            }
        }
        "fma" => {
            need!(4);
            let d = reg(interner, &ops[0]);
            Inst::FFma {
                d,
                a: parse_op(&ops[1], interner, true)?,
                b: parse_op(&ops[2], interner, true)?,
                c: parse_op(&ops[3], interner, true)?,
            }
        }
        "setp" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            let cmp = if has("eq") {
                CMP_EQ
            } else if has("ne") {
                CMP_NE
            } else if has("lt") {
                CMP_LT
            } else if has("le") {
                CMP_LE
            } else if has("gt") {
                CMP_GT
            } else if has("ge") {
                CMP_GE
            } else {
                return Err(err(format!("unsupported setp cmp: `{stmt}`")));
            };
            Inst::Setp {
                d,
                a: parse_op(&ops[1], interner, false)?,
                b: parse_op(&ops[2], interner, false)?,
                cmp,
                unsigned: has("u32") || has("u64") || has("u16") || has("u8"),
            }
        }
        "cvt" => {
            need!(2);
            let d = reg(interner, &ops[0]);
            let s = parse_op(&ops[1], interner, has("f32"))?;
            // opcode form is `cvt.<dst>.<src>` (plus optional rounding). Inspect the type tokens.
            let mods: Vec<&str> = opcode.split('.').skip(1).collect();
            let tys: Vec<&str> = mods
                .into_iter()
                .filter(|m| m.starts_with('f') || m.starts_with('s') || m.starts_with('u'))
                .collect();
            let kind = match (tys.first(), tys.get(1)) {
                (Some(&d), Some(&s)) if d.starts_with('f') && s.starts_with('s') => CVT_F32_FROM_S32,
                (Some(&d), Some(&s)) if d == "s64" && s.starts_with('s') => CVT_S64_FROM_S32,
                (Some(&d), Some(&s)) if d.starts_with('s') && s.starts_with('f') => CVT_S32_FROM_F32,
                _ => CVT_IDENTITY,
            };
            Inst::Cvt { d, s, kind }
        }
        "shl" | "shr" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            Inst::Shift {
                d,
                a: parse_op(&ops[1], interner, false)?,
                b: parse_op(&ops[2], interner, false)?,
                dir: if base == "shl" { SHIFT_LEFT } else { SHIFT_RIGHT },
                unsigned: has("u32") || has("u64") || has("u16") || has("b32") || has("b64") || has("b16"),
            }
        }
        "and" | "or" | "xor" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            let op = match base {
                "and" => BIT_AND,
                "or" => BIT_OR,
                _ => BIT_XOR,
            };
            Inst::BitOp {
                d,
                a: parse_op(&ops[1], interner, false)?,
                b: parse_op(&ops[2], interner, false)?,
                op,
            }
        }
        // `atom.<space>.<op>[.type] [d,] [addr], val [, cas_cmp]` and the destination-less `red.*` form.
        "atom" | "red" => {
            let has_dest = base == "atom";
            let op = atom_op(opcode)?;
            let unsigned = has("u32") || has("u64") || has("u16") || has("b32") || has("b64");
            if opcode.contains("f32") || opcode.contains("f64") {
                return Err(err(format!("floating-point atomics unsupported (WGSL has no f32 atomics): `{stmt}`")));
            }
            // operand shape: atom → [d, addr, val (, cmp_for_cas... actually val,newval)]; red → [addr, val].
            // CUDA CAS is `atom.cas d, [addr], compare, swap` → operands: d, addr, compare, swap.
            let mut i = 0;
            let d = if has_dest {
                let r = reg(interner, &ops[i]);
                i += 1;
                Some(r)
            } else {
                None
            };
            need!(i + 2);
            let addr_tok = &ops[i];
            i += 1;
            let (cmp, val) = if op == ATOM_CAS {
                need!(i + 2);
                let cmp = parse_op(&ops[i], interner, false)?;
                let val = parse_op(&ops[i + 1], interner, false)?;
                (cmp, val)
            } else {
                (Op::ImmI(0), parse_op(&ops[i], interner, false)?)
            };
            if has("shared") {
                let (b, off) = parse_shared_mem(addr_tok, shared_syms, interner)?;
                Inst::AtomShared { d, base: b, off, op, cmp, val, unsigned }
            } else if has("global") || !has("shared") {
                let (addr, off) = parse_mem(addr_tok, interner)?;
                Inst::AtomGlobal { d, addr, off, op, cmp, val, unsigned }
            } else {
                return Err(err(format!("unsupported atom space: `{stmt}`")));
            }
        }
        other => return Err(err(format!("unsupported opcode `{other}`: `{stmt}`"))),
    };
    Ok(inst)
}

/// Map an `atom.*`/`red.*` opcode to an `ATOM_*` code.
fn atom_op(opcode: &str) -> Result<u8> {
    let m = |x: &str| opcode.split('.').any(|t| t == x);
    Ok(if m("add") || m("inc") {
        ATOM_ADD
    } else if m("min") {
        ATOM_MIN
    } else if m("max") {
        ATOM_MAX
    } else if m("and") {
        ATOM_AND
    } else if m("or") {
        ATOM_OR
    } else if m("xor") {
        ATOM_XOR
    } else if m("exch") {
        ATOM_EXCH
    } else if m("cas") {
        ATOM_CAS
    } else {
        return Err(err(format!("unsupported atomic op: `{opcode}`")));
    })
}

/// Parse a shared-memory address operand `[base(+off)]` where `base` is either a register or a `.shared`
/// symbol name (resolved to its byte offset). Returns `(base_op, byte_off)`; the effective shared offset
/// is `eval(base_op) + byte_off`.
fn parse_shared_mem(
    tok: &str,
    shared_syms: &std::collections::HashMap<String, u32>,
    interner: &mut Interner,
) -> Result<(Op, i64)> {
    let inner = tok.trim().trim_start_matches('[').trim_end_matches(']').trim();
    // split an optional +/- displacement (not part of a register name).
    let (base_tok, off) = if let Some(pos) = inner.find(['+', '-']) {
        let (b, o) = inner.split_at(pos);
        (b.trim(), parse_imm_i(o.trim())?)
    } else {
        (inner, 0)
    };
    if is_reg(base_tok) {
        Ok((Op::Reg(interner.get(strip_reg(base_tok))), off))
    } else if let Some(&sym) = shared_syms.get(base_tok) {
        Ok((Op::ImmI(sym as i64), off))
    } else {
        // a bare numeric base offset into shared memory.
        Ok((Op::ImmI(parse_imm_i(base_tok)?), off))
    }
}

// ---- small parser helpers ----

fn split_first(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

/// Split an operand list on commas, respecting `[a+b]` brackets (no commas occur inside for our subset).
fn split_operands(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn is_reg(tok: &str) -> bool {
    tok.starts_with('%')
}
fn strip_reg(tok: &str) -> &str {
    tok.trim().trim_start_matches('%')
}
fn strip_label(tok: &str) -> &str {
    tok.trim()
}
fn strip_mem_name(tok: &str) -> &str {
    tok.trim().trim_start_matches('[').trim_end_matches(']').trim()
}

fn parse_sreg(tok: &str) -> Option<u8> {
    Some(match strip_reg(tok) {
        "tid.x" => SR_TID_X,
        "tid.y" => SR_TID_Y,
        "tid.z" => SR_TID_Z,
        "ntid.x" => SR_NTID_X,
        "ntid.y" => SR_NTID_Y,
        "ntid.z" => SR_NTID_Z,
        "ctaid.x" => SR_CTAID_X,
        "ctaid.y" => SR_CTAID_Y,
        "ctaid.z" => SR_CTAID_Z,
        "nctaid.x" => SR_NCTAID_X,
        "nctaid.y" => SR_NCTAID_Y,
        "nctaid.z" => SR_NCTAID_Z,
        _ => return None,
    })
}

fn gtype(opcode: &str) -> Result<u8> {
    if opcode.contains("f32") {
        Ok(gty::F32)
    } else if opcode.contains("u64") || opcode.contains("s64") || opcode.contains("b64") {
        Ok(gty::U64)
    } else if opcode.contains("u32") || opcode.contains("s32") || opcode.contains("b32") {
        Ok(gty::U32)
    } else {
        Err(err(format!("unsupported global access type: `{opcode}`")))
    }
}

/// `[%reg]`, `[%reg+off]`, or `[%reg-off]` → (reg, byte offset).
fn parse_mem(tok: &str, interner: &mut Interner) -> Result<(u16, i64)> {
    let inner = tok.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if let Some(pos) = inner.find(['+', '-']) {
        let (r, o) = inner.split_at(pos);
        let off = parse_imm_i(o.trim())?;
        Ok((interner.get(strip_reg(r.trim())), off))
    } else {
        Ok((interner.get(strip_reg(inner)), 0))
    }
}

fn parse_op(tok: &str, interner: &mut Interner, want_float: bool) -> Result<Op> {
    let t = tok.trim();
    if is_reg(t) {
        Ok(Op::Reg(interner.get(strip_reg(t))))
    } else if want_float {
        Ok(Op::ImmF(parse_imm_f(t)?))
    } else {
        Ok(Op::ImmI(parse_imm_i(t)?))
    }
}

fn parse_imm_i(t: &str) -> Result<i64> {
    let t = t.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).map_err(|_| err(format!("bad hex immediate `{t}`")))
    } else {
        t.parse::<i64>().map_err(|_| err(format!("bad integer immediate `{t}`")))
    }
}

/// PTX float immediates are `0f<hex bits>` (single) — the exact 32-bit encoding. Also accept decimals.
fn parse_imm_f(t: &str) -> Result<u32> {
    let t = t.trim();
    if let Some(h) = t.strip_prefix("0f").or_else(|| t.strip_prefix("0F")) {
        u32::from_str_radix(h, 16).map_err(|_| err(format!("bad float immediate `{t}`")))
    } else {
        t.parse::<f32>()
            .map(|f| f.to_bits())
            .map_err(|_| err(format!("bad float immediate `{t}`")))
    }
}

// ===================================================================================================
// interpreter — run a KernelProgram over the launch grid on the CPU
// ===================================================================================================

#[derive(Clone, Copy)]
enum Val {
    I(u64),
    F(f32),
    P { region: u32, off: i64 },
    Pred(bool),
}

impl Val {
    fn as_u64(self) -> u64 {
        match self {
            Val::I(v) => v,
            Val::F(f) => f.to_bits() as u64,
            Val::P { off, .. } => off as u64,
            Val::Pred(b) => b as u64,
        }
    }
    fn as_i64(self) -> i64 {
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
    // Kernels with shared memory or barriers need cooperative, block-scoped execution (a per-thread
    // run-to-completion would not model shared-memory hand-offs across `bar.sync`). Elementwise kernels
    // keep the original run-each-thread-independently path (unchanged behavior).
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

/// Where a thread paused: at a `bar.sync` (rendezvous) or a `ret` (retired).
enum Stop {
    Ret,
    Barrier,
}

/// Cooperatively execute one thread block (CTA), correctly modeling `bar.sync`: every live thread runs
/// forward to its next barrier (or `ret`); only once all live threads have arrived does the block advance
/// past the barrier. All threads share one `shared` byte array (workgroup memory). This is the CPU oracle
/// for shared-memory + barrier + atomic kernels.
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
    // Per-thread continuation state.
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
                        tx, ty, tz, bx, by, bz, cta[0], cta[1], cta[2], gx.max(1), gy.max(1), gz.max(1),
                    ],
                });
            }
        }
    }
    // Phase loop: run every live thread to its next sync point; repeat until all have retired. A phase
    // count cap guards against a barrier only some threads reach (a malformed kernel) looping forever.
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
                Stop::Barrier => {} // parked just past the barrier; resumes next phase
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
            return Err(err("kernel block exceeded barrier-phase cap (barrier not reached by all threads?)"));
        }
    }
}

fn run_until(
    prog: &KernelProgram,
    param_blob: &[u8],
    regions: &mut [Vec<u8>],
    shared: &mut Vec<u8>,
    regs: &mut Vec<Val>,
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
            return Err(err("kernel exceeded step cap (suspected infinite loop)"));
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
                        // mul.wide.u32: zero-extend both 32-bit operands to 64-bit.
                        Val::I((va as u32 as u64).wrapping_mul(vb as u32 as u64))
                    } else {
                        // mul.wide.s32: sign-extend both 32-bit operands to 64-bit.
                        Val::I(((va as i32 as i64).wrapping_mul(vb as i32 as i64)) as u64)
                    }
                } else {
                    // mul.lo: low 32 bits; two's-complement makes this sign-agnostic.
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
                        _ => va >= vb, // CMP_GE
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
                        _ => va >= vb, // CMP_GE
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
                let mem = regions
                    .get(region as usize)
                    .ok_or_else(|| err("ld.global: unbound region"))?;
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
                    .ok_or_else(|| err("st.global: unbound region"))?;
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
                let (fa, fb, fc) = (eval(regs, a).as_f32(), eval(regs, b).as_f32(), eval(regs, c).as_f32());
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
                let sh = (eval(regs, b).as_u64() as u32) & 31; // shift amount mod 32 (PTX/HW semantics)
                let r = match *dir {
                    SHIFT_LEFT => va.wrapping_shl(sh),
                    _ if *unsigned => va.wrapping_shr(sh),
                    _ => (va as i32).wrapping_shr(sh) as u32, // arithmetic right shift
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
                    .ok_or_else(|| err("atom.global: unbound region"))?;
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

/// Resolve a shared-memory byte address from a base value + constant displacement, rejecting negatives.
fn shared_addr(base: Val, off: i64) -> Result<usize> {
    let eff = (base.as_i64()).wrapping_add(off);
    if eff < 0 {
        return Err(GpuError::OutOfBounds);
    }
    Ok(eff as usize)
}

/// Perform a 32-bit atomic read-modify-write on `mem` at byte offset `at`, returning the OLD value.
/// The CPU oracle is single-threaded within a barrier phase, so the read-modify-write is trivially
/// serialized (matching the total order a real atomic imposes).
fn atomic_rmw(mem: &mut [u8], at: usize, op: u8, cmp: u32, val: u32, unsigned: bool) -> Result<u32> {
    let old = read_scalar(mem, at, 4)? as u32;
    let new = match op {
        ATOM_ADD => old.wrapping_add(val),
        ATOM_MIN => {
            if unsigned {
                old.min(val)
            } else {
                (old as i32).min(val as i32) as u32
            }
        }
        ATOM_MAX => {
            if unsigned {
                old.max(val)
            } else {
                (old as i32).max(val as i32) as u32
            }
        }
        ATOM_AND => old & val,
        ATOM_OR => old | val,
        ATOM_XOR => old ^ val,
        ATOM_EXCH => val,
        ATOM_CAS => {
            if old == cmp {
                val
            } else {
                old
            }
        }
        _ => return Err(err("unknown atomic op")),
    };
    write_scalar(mem, at, 4, new as u64)?;
    Ok(old)
}

fn ptr_int_add(a: Val, b: Val, wide: bool, sign: i64) -> Val {
    match (a, b) {
        (Val::P { region, off }, _) => Val::P { region, off: off + sign * b.as_i64() },
        (_, Val::P { region, off }) => Val::P { region, off: off + sign * a.as_i64() },
        _ => {
            let r = (a.as_i64()).wrapping_add(sign.wrapping_mul(b.as_i64()));
            if wide {
                Val::I(r as u64)
            } else {
                Val::I(r as u32 as u64)
            }
        }
    }
}

fn ptr_target(v: Val) -> Result<(u32, i64)> {
    match v {
        Val::P { region, off } => Ok((region, off)),
        _ => Err(err("global access through a non-pointer value (unsupported flat addressing)")),
    }
}

fn read_scalar(mem: &[u8], at: usize, width: usize) -> Result<u64> {
    if at + width > mem.len() {
        return Err(GpuError::OutOfBounds);
    }
    let mut b = [0u8; 8];
    b[..width].copy_from_slice(&mem[at..at + width]);
    Ok(u64::from_le_bytes(b))
}

fn write_scalar(mem: &mut [u8], at: usize, width: usize, val: u64) -> Result<()> {
    if at + width > mem.len() {
        return Err(GpuError::OutOfBounds);
    }
    mem[at..at + width].copy_from_slice(&val.to_le_bytes()[..width]);
    Ok(())
}

// ===================================================================================================
// WGSL back-end — lower a compiled KernelProgram to a WGSL compute shader (the REAL-GPU path)
// ===================================================================================================
//
// The software interpreter above is the CPU oracle. For the real Metal GPU we lower the SAME
// [`KernelProgram`] to a WGSL compute entry point that `dd-gpu-wgpu` hands to naga → wgpu → MSL. This
// keeps ONE PTX front-end (`compile`) feeding both backends; only the code-gen tail differs (CPU
// interpret vs. WGSL emit), exactly the `software.rs` / `metal.rs` seam the module header describes.
//
// ## Shape of the emitted shader (matches the launch ABI in `cuda.rs`)
// * `@group(0) @binding(0) var<storage, read> params: array<u32>;` — the flat kernel-parameter blob;
//   a scalar param at byte `off` is `params[off/4]`.
// * `@group(0) @binding(r+1) var<storage, read_write> region{r}: array<u32>;` — one storage buffer per
//   pointer parameter (region `r`), addressed as raw 32-bit words; f32 travels as its bit pattern.
// * `@workgroup_size(bx,by,bz)` baked from the launch block; `%tid`→`local_invocation_id`,
//   `%ntid`→the constant block dims, `%ctaid`→`workgroup_id`, `%nctaid`→`num_workgroups`.
//
// Registers become function-scope `var r{n}: u32` (all 32-bit; f32 via `bitcast`, pointers as their
// byte-offset within a region — mirroring `Val::P.off`). Unstructured PTX control flow (`bra`) is made
// WGSL-legal by a `pc`-dispatch `loop { switch pc { case k: {…} } }`, so an arbitrary forward/back
// branch is just `pc = target;`. This is the general translation of the register machine; no relooper.
//
// ## Which region a `ld/st.global` touches
// The storage binding must be statically known (WGSL can't index across bindings), so we run the same
// pointer-taint the interpreter does, but per register: a register carries `Some(region)` iff it holds
// a device address, propagated through `ld.param`(ptr) / `cvta` / `mov` / pointer±int. A global access
// through a register with no region is rejected (the flat-unified-VA case Metal can't model).

/// Byte width a `gty` global access reads/writes.
fn gty_elem_bytes(ty: u8) -> Result<u32> {
    match ty {
        gty::F32 | gty::U32 => Ok(4),
        // 64-bit global access can't be expressed over an `array<u32>` word view without a two-word
        // split; outside the elementwise slice this lowering targets, so reject it honestly.
        gty::U64 => Err(err("wgsl lowering: 64-bit global load/store unsupported (elementwise subset)")),
        other => Err(err(format!("wgsl lowering: unknown global type {other}"))),
    }
}

/// Static per-register pointer-region analysis: `out[r] == Some(region)` iff register `r` holds a
/// device address into that pointer-parameter region at its most recent definition. Forward, last-write-
/// wins — correct for the straight-line-with-forward-guard subset this lowering accepts.
fn region_analysis(prog: &KernelProgram) -> Vec<Option<u32>> {
    let mut reg: Vec<Option<u32>> = vec![None; prog.reg_count as usize];
    let of = |reg: &Vec<Option<u32>>, op: &Op| -> Option<u32> {
        match op {
            Op::Reg(r) => reg[*r as usize],
            _ => None,
        }
    };
    for inst in &prog.insts {
        match inst {
            Inst::LdParam { d, param } => {
                let p = &prog.params[*param as usize];
                reg[*d as usize] = if p.is_ptr { Some(p.region) } else { None };
            }
            Inst::MovReg { d, s } => reg[*d as usize] = reg[*s as usize],
            Inst::Cvta { d, s } => reg[*d as usize] = reg[*s as usize],
            Inst::IAdd { d, a, b, .. } | Inst::ISub { d, a, b, .. } => {
                reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b));
            }
            Inst::IMad { d, a, b, c } => {
                reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b)).or_else(|| of(&reg, c));
            }
            Inst::IMul { d, a, b, .. } => reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b)),
            // Every other value-producing opcode yields a plain (non-pointer) value.
            Inst::MovImmI { d, .. }
            | Inst::MovImmF { d, .. }
            | Inst::MovSReg { d, .. }
            | Inst::LdGlobal { d, .. }
            | Inst::LdShared { d, .. }
            | Inst::Setp { d, .. }
            | Inst::Shift { d, .. }
            | Inst::BitOp { d, .. }
            | Inst::FAdd { d, .. }
            | Inst::FSub { d, .. }
            | Inst::FMul { d, .. }
            | Inst::FFma { d, .. }
            | Inst::Cvt { d, .. } => reg[*d as usize] = None,
            Inst::AtomGlobal { d, .. } | Inst::AtomShared { d, .. } => {
                if let Some(dr) = d {
                    reg[*dr as usize] = None;
                }
            }
            Inst::StGlobal { .. }
            | Inst::StShared { .. }
            | Inst::Bra { .. }
            | Inst::Bar
            | Inst::Ret
            | Inst::Nop => {}
        }
    }
    reg
}

/// Which pointer regions are accessed atomically anywhere in the kernel → those storage buffers must be
/// typed `array<atomic<u32>>` in WGSL (and ALL their accesses, including plain `ld/st.global`, routed
/// through `atomicLoad`/`atomicStore`). A region touched by BOTH a plain and an atomic access is legal
/// here because we always route through the atomic accessors when the region is atomic.
fn atomic_regions(prog: &KernelProgram) -> Result<Vec<bool>> {
    let region = region_analysis(prog);
    let mut atomic = vec![false; prog.num_regions as usize];
    for inst in &prog.insts {
        if let Inst::AtomGlobal { addr, .. } = inst {
            let r = region[*addr as usize].ok_or_else(|| {
                err("wgsl lowering: atomic through a value with no static pointer region")
            })?;
            atomic[r as usize] = true;
        }
    }
    Ok(atomic)
}

/// A special-register id → the WGSL builtin/constant expression that yields its value.
fn sreg_expr(sreg: u8, block: [u32; 3]) -> String {
    match sreg {
        SR_TID_X => "lid.x".into(),
        SR_TID_Y => "lid.y".into(),
        SR_TID_Z => "lid.z".into(),
        SR_NTID_X => format!("{}u", block[0]),
        SR_NTID_Y => format!("{}u", block[1]),
        SR_NTID_Z => format!("{}u", block[2]),
        SR_CTAID_X => "wid.x".into(),
        SR_CTAID_Y => "wid.y".into(),
        SR_CTAID_Z => "wid.z".into(),
        SR_NCTAID_X => "nwg.x".into(),
        SR_NCTAID_Y => "nwg.y".into(),
        _ => "nwg.z".into(), // SR_NCTAID_Z
    }
}

fn op_u32(op: &Op) -> String {
    match op {
        Op::Reg(r) => format!("r{r}"),
        Op::ImmI(i) => format!("{}u", *i as u32),
        Op::ImmF(b) => format!("{b}u"),
    }
}
fn op_i32(op: &Op) -> String {
    match op {
        Op::Reg(r) => format!("bitcast<i32>(r{r})"),
        Op::ImmI(i) => format!("i32({})", *i as i32),
        Op::ImmF(b) => format!("bitcast<i32>({b}u)"),
    }
}
fn op_f32(op: &Op) -> String {
    match op {
        Op::Reg(r) => format!("bitcast<f32>(r{r})"),
        Op::ImmF(b) => format!("bitcast<f32>({b}u)"),
        Op::ImmI(i) => format!("f32({i})"),
    }
}

/// Lower a compiled [`KernelProgram`] to a WGSL compute shader whose entry point is `prog.entry`. The
/// emitted module is what `dd-gpu-wgpu` compiles to a real `wgpu::ComputePipeline` (naga WGSL→MSL). See
/// the section header for the ABI and control-flow model. Returns [`GpuError::Ptx`] for any instruction
/// outside the elementwise subset this lowering supports (e.g. a 64-bit global access, or a global
/// access through a register with no statically-known region).
pub fn kernel_to_wgsl(prog: &KernelProgram) -> Result<String> {
    let region = region_analysis(prog);
    let region_of = |addr: u16| -> Result<u32> {
        region[addr as usize].ok_or_else(|| {
            err("wgsl lowering: global access through a value with no static pointer region")
        })
    };
    // Regions accessed atomically must be typed `array<atomic<u32>>`; all their accesses (incl. plain
    // ld/st.global) route through atomicLoad/atomicStore. Shared memory is likewise atomic-typed iff any
    // `atom.shared` touches it. Barriers force the cooperative (over-synchronizing) loop form.
    let atomic = atomic_regions(prog)?;
    let shared_atomic = prog.insts.iter().any(|i| matches!(i, Inst::AtomShared { .. }));
    let coop = prog.insts.iter().any(|i| matches!(i, Inst::Bar));

    // shared-memory index expression `(base + off) / elem` (byte offset → word index).
    let shared_idx = |base: &Op, off: i64, elem: u32| format!("(({} + {}u) / {elem}u)", op_u32(base), off as u32);

    let mut s = String::new();
    // --- resource bindings (params + one storage buffer per pointer region) ---
    s.push_str("@group(0) @binding(0) var<storage, read> params: array<u32>;\n");
    for r in 0..prog.num_regions {
        let elem_ty = if atomic[r as usize] { "atomic<u32>" } else { "u32" };
        s.push_str(&format!(
            "@group(0) @binding({}) var<storage, read_write> region{r}: array<{elem_ty}>;\n",
            r + 1
        ));
    }
    // --- workgroup shared memory (a `.shared` byte array, viewed as 32-bit words) ---
    if prog.shared_bytes > 0 {
        let words = (prog.shared_bytes / 4).max(1);
        let elem_ty = if shared_atomic { "atomic<u32>" } else { "u32" };
        s.push_str(&format!("var<workgroup> shmem: array<{elem_ty}, {words}>;\n"));
    }
    // --- cooperative-loop bookkeeping: a workgroup counter of retired threads (see the loop below) ---
    if coop {
        s.push_str("var<workgroup> dd_retired_count: atomic<u32>;\n");
    }
    s.push('\n');

    // --- entry point signature (block dims baked as the workgroup size) ---
    let [bx, by, bz] = prog.block;
    let total_threads = bx.max(1) * by.max(1) * bz.max(1);
    s.push_str(&format!("@compute @workgroup_size({}, {}, {})\n", bx.max(1), by.max(1), bz.max(1)));
    s.push_str(&format!("fn {}(\n", prog.entry));
    s.push_str("    @builtin(local_invocation_id) lid: vec3<u32>,\n");
    if coop {
        s.push_str("    @builtin(local_invocation_index) lidx: u32,\n");
    }
    s.push_str("    @builtin(workgroup_id) wid: vec3<u32>,\n");
    s.push_str("    @builtin(num_workgroups) nwg: vec3<u32>,\n");
    s.push_str(") {\n");

    // --- register file (all u32; f32 via bitcast, pointers as their region byte-offset) ---
    for r in 0..prog.reg_count {
        s.push_str(&format!("    var r{r}: u32 = 0u;\n"));
    }

    // === control flow ===
    // Both forms are a `pc`-dispatch `loop { switch pc { case k: … } }` — the general lowering of
    // unstructured PTX `bra` into WGSL's structured control flow (no relooper).
    //
    // The COOPERATIVE form (kernels with `bar.sync`) additionally executes ONE `workgroupBarrier()`
    // UNCONDITIONALLY at the bottom of every loop iteration. Because that barrier is outside all `pc`-
    // dependent control flow it is trivially uniform (naga-legal), and because every thread runs the loop
    // in iteration-lockstep it is reached by the whole workgroup each pass — an over-approximation of the
    // program's real `bar.sync` points (which is always safe: extra synchronization can never introduce a
    // race). A thread that hits `ret` increments `dd_retired_count` and idles (its `switch` is skipped)
    // but keeps hitting the barrier, so no thread is ever left waiting on a retired peer; the whole block
    // breaks together the iteration the last thread retires. A real `bar.sync` is thus a plain pc-advance.
    if coop {
        s.push_str("    if (lidx == 0u) { atomicStore(&dd_retired_count, 0u); }\n");
        s.push_str("    workgroupBarrier();\n");
        s.push_str("    var pc: i32 = 0;\n");
        s.push_str("    var dd_retired: bool = false;\n");
        s.push_str("    loop {\n");
        s.push_str("        if (!dd_retired) {\n");
        s.push_str("            switch pc {\n");
    } else {
        // Termination sets `pc = -1`; the loop-top guard is the only real loop `break`.
        s.push_str("    var pc: i32 = 0;\n");
        s.push_str("    loop {\n");
        s.push_str("        if (pc < 0) { break; }\n");
        s.push_str("        switch pc {\n");
    }
    let indent = if coop { "                " } else { "            " };
    // The statement a `ret` (or falling off the end) lowers to.
    let retire = if coop {
        "atomicAdd(&dd_retired_count, 1u); dd_retired = true; pc = -1;"
    } else {
        "pc = -1;"
    };
    for (k, inst) in prog.insts.iter().enumerate() {
        let mut body = String::new();
        let mut branched = false; // instruction sets pc itself (no fallthrough advance)
        match inst {
            Inst::Ret => {
                body.push_str(retire);
                branched = true;
            }
            Inst::Nop => {}
            Inst::Bar => {} // real sync is the unconditional per-iteration barrier below; just advance pc
            Inst::MovImmI { d, imm } => body.push_str(&format!("r{d} = {}u;", *imm as u32)),
            Inst::MovImmF { d, bits } => body.push_str(&format!("r{d} = {bits}u;")),
            Inst::MovReg { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
            Inst::MovSReg { d, sreg } => {
                body.push_str(&format!("r{d} = {};", sreg_expr(*sreg, prog.block)))
            }
            Inst::LdParam { d, param } => {
                let p = &prog.params[*param as usize];
                if p.is_ptr {
                    body.push_str(&format!("r{d} = 0u;"));
                } else {
                    body.push_str(&format!("r{d} = params[{}u];", p.offset / 4));
                }
            }
            Inst::Cvta { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
            Inst::IAdd { d, a, b, .. } => {
                body.push_str(&format!("r{d} = {} + {};", op_u32(a), op_u32(b)))
            }
            Inst::ISub { d, a, b, .. } => {
                body.push_str(&format!("r{d} = {} - {};", op_u32(a), op_u32(b)))
            }
            Inst::IMul { d, a, b, .. } => {
                body.push_str(&format!("r{d} = {} * {};", op_u32(a), op_u32(b)))
            }
            Inst::IMad { d, a, b, c } => {
                body.push_str(&format!("r{d} = {} * {} + {};", op_u32(a), op_u32(b), op_u32(c)))
            }
            Inst::Shift { d, a, b, dir, unsigned } => {
                let sh = format!("({} & 31u)", op_u32(b));
                let e = match *dir {
                    SHIFT_LEFT => format!("{} << {sh}", op_u32(a)),
                    _ if *unsigned => format!("{} >> {sh}", op_u32(a)),
                    _ => format!("bitcast<u32>({} >> {sh})", op_i32(a)), // arithmetic right shift
                };
                body.push_str(&format!("r{d} = {e};"));
            }
            Inst::BitOp { d, a, b, op } => {
                let sym = match *op {
                    BIT_AND => "&",
                    BIT_OR => "|",
                    _ => "^",
                };
                body.push_str(&format!("r{d} = {} {sym} {};", op_u32(a), op_u32(b)));
            }
            Inst::Setp { d, a, b, cmp, unsigned } => {
                let sym = match *cmp {
                    CMP_EQ => "==",
                    CMP_NE => "!=",
                    CMP_LT => "<",
                    CMP_LE => "<=",
                    CMP_GT => ">",
                    _ => ">=", // CMP_GE
                };
                let cond = if *unsigned {
                    format!("{} {sym} {}", op_u32(a), op_u32(b))
                } else {
                    format!("{} {sym} {}", op_i32(a), op_i32(b))
                };
                body.push_str(&format!("r{d} = select(0u, 1u, {cond});"));
            }
            Inst::Bra { target, pred } => {
                match pred {
                    None => body.push_str(&format!("pc = {target};")),
                    Some((p, neg)) => {
                        let cond = if *neg { format!("r{p} == 0u") } else { format!("r{p} != 0u") };
                        body.push_str(&format!("if ({cond}) {{ pc = {target}; }} else {{ pc = {}; }}", k + 1));
                    }
                }
                branched = true;
            }
            Inst::LdGlobal { d, addr, off, ty } => {
                let r = region_of(*addr)?;
                let elem = gty_elem_bytes(*ty)?;
                let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                if atomic[r as usize] {
                    body.push_str(&format!("r{d} = atomicLoad(&region{r}[{idx}]);"));
                } else {
                    body.push_str(&format!("r{d} = region{r}[{idx}];"));
                }
            }
            Inst::StGlobal { addr, off, src, ty } => {
                let r = region_of(*addr)?;
                let elem = gty_elem_bytes(*ty)?;
                let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                if atomic[r as usize] {
                    body.push_str(&format!("atomicStore(&region{r}[{idx}], {});", op_u32(src)));
                } else {
                    body.push_str(&format!("region{r}[{idx}] = {};", op_u32(src)));
                }
            }
            Inst::LdShared { d, base, off, ty } => {
                let elem = gty_elem_bytes(*ty)?;
                let idx = shared_idx(base, *off, elem);
                if shared_atomic {
                    body.push_str(&format!("r{d} = atomicLoad(&shmem[{idx}]);"));
                } else {
                    body.push_str(&format!("r{d} = shmem[{idx}];"));
                }
            }
            Inst::StShared { base, off, src, ty } => {
                let elem = gty_elem_bytes(*ty)?;
                let idx = shared_idx(base, *off, elem);
                if shared_atomic {
                    body.push_str(&format!("atomicStore(&shmem[{idx}], {});", op_u32(src)));
                } else {
                    body.push_str(&format!("shmem[{idx}] = {};", op_u32(src)));
                }
            }
            Inst::AtomGlobal { d, addr, off, op, cmp, val, unsigned } => {
                let r = region_of(*addr)?;
                let idx = format!("(r{addr} + {}u) / 4u", *off as u32);
                let ptr = format!("&region{r}[{idx}]");
                emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
            }
            Inst::AtomShared { d, base, off, op, cmp, val, unsigned } => {
                if !shared_atomic {
                    return Err(err("wgsl lowering: atom.shared requires an atomic shared array"));
                }
                let idx = shared_idx(base, *off, 4);
                let ptr = format!("&shmem[{idx}]");
                emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
            }
            Inst::FAdd { d, a, b } => {
                body.push_str(&format!("r{d} = bitcast<u32>({} + {});", op_f32(a), op_f32(b)))
            }
            Inst::FSub { d, a, b } => {
                body.push_str(&format!("r{d} = bitcast<u32>({} - {});", op_f32(a), op_f32(b)))
            }
            Inst::FMul { d, a, b } => {
                body.push_str(&format!("r{d} = bitcast<u32>({} * {});", op_f32(a), op_f32(b)))
            }
            Inst::FFma { d, a, b, c } => body.push_str(&format!(
                "r{d} = bitcast<u32>(fma({}, {}, {}));",
                op_f32(a),
                op_f32(b),
                op_f32(c)
            )),
            Inst::Cvt { d, s: src, kind } => {
                let e = match *kind {
                    CVT_F32_FROM_S32 => format!("bitcast<u32>(f32({}))", op_i32(src)),
                    CVT_S64_FROM_S32 => op_u32(src),
                    CVT_S32_FROM_F32 => format!("bitcast<u32>(i32({}))", op_f32(src)),
                    _ => op_u32(src),
                };
                body.push_str(&format!("r{d} = {e};"));
            }
        }
        if !branched {
            body.push_str(&format!(" pc = {};", k + 1));
        }
        s.push_str(&format!("{indent}case {k}: {{ {body} }}\n"));
    }
    // Fell off the end (or a branch past the last case) → retire this thread.
    s.push_str(&format!("{indent}default: {{ {retire} }}\n"));
    if coop {
        s.push_str("            }\n"); // close switch
        s.push_str("        }\n"); // close `if (!dd_retired)`
        s.push_str("        workgroupBarrier();\n");
        s.push_str(&format!("        if (atomicLoad(&dd_retired_count) == {total_threads}u) {{ break; }}\n"));
        s.push_str("    }\n}\n"); // close loop + fn
    } else {
        s.push_str("        }\n    }\n}\n"); // close switch + loop + fn
    }
    Ok(s)
}

/// Emit a WGSL atomic read-modify-write on `ptr` (`&region…[…]` / `&shmem[…]`, an `atomic<u32>`),
/// optionally capturing the old value into `r{d}`. `CAS` is emulated with a strong compare-exchange
/// loop (WGSL's `atomicCompareExchangeWeak` may fail spuriously). Signed `min`/`max` are rejected — the
/// atomic word is unsigned-typed, so only the unsigned variants have correct semantics.
fn emit_atomic(
    body: &mut String,
    ptr: &str,
    op: u8,
    cmp: &Op,
    val: &Op,
    d: Option<u16>,
    unsigned: bool,
) -> Result<()> {
    let dst = |body: &mut String, expr: &str| match d {
        Some(dr) => body.push_str(&format!("r{dr} = {expr};")),
        None => body.push_str(&format!("{expr};")), // `red.*` form: discard the old value
    };
    match op {
        ATOM_ADD => dst(body, &format!("atomicAdd({ptr}, {})", op_u32(val))),
        ATOM_AND => dst(body, &format!("atomicAnd({ptr}, {})", op_u32(val))),
        ATOM_OR => dst(body, &format!("atomicOr({ptr}, {})", op_u32(val))),
        ATOM_XOR => dst(body, &format!("atomicXor({ptr}, {})", op_u32(val))),
        ATOM_EXCH => dst(body, &format!("atomicExchange({ptr}, {})", op_u32(val))),
        ATOM_MIN if unsigned => dst(body, &format!("atomicMin({ptr}, {})", op_u32(val))),
        ATOM_MAX if unsigned => dst(body, &format!("atomicMax({ptr}, {})", op_u32(val))),
        ATOM_MIN | ATOM_MAX => {
            return Err(err("wgsl lowering: signed atomic min/max unsupported (atomic word is u32)"))
        }
        ATOM_CAS => {
            // Strong CAS via a weak-CAS retry loop: retry only on a spurious failure (old == compare but
            // not exchanged). Either way `res.old_value` is the true prior value.
            let (c, v) = (op_u32(cmp), op_u32(val));
            match d {
                Some(dr) => body.push_str(&format!(
                    "loop {{ let res = atomicCompareExchangeWeak({ptr}, {c}, {v}); \
                     if (res.exchanged || res.old_value != {c}) {{ r{dr} = res.old_value; break; }} }}"
                )),
                None => body.push_str(&format!(
                    "loop {{ let res = atomicCompareExchangeWeak({ptr}, {c}, {v}); \
                     if (res.exchanged || res.old_value != {c}) {{ break; }} }}"
                )),
            }
        }
        _ => return Err(err("wgsl lowering: unknown atomic op")),
    }
    Ok(())
}

// ===================================================================================================
// tests — parser round-trips + malformed rejection (headless, no GPU)
// ===================================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_vecadd_and_classifies_params() {
        let prog = compile(VECADD_PTX, "vecadd", [256, 1, 1]).unwrap();
        assert_eq!(prog.entry, "vecadd");
        assert_eq!(prog.block, [256, 1, 1]);
        assert_eq!(prog.params.len(), 4);
        // a,b,c are pointers → regions 0,1,2 ; n is a scalar at offset 24.
        assert!(prog.params[0].is_ptr && prog.params[0].region == 0);
        assert!(prog.params[1].is_ptr && prog.params[1].region == 1);
        assert!(prog.params[2].is_ptr && prog.params[2].region == 2);
        assert!(!prog.params[3].is_ptr);
        assert_eq!(prog.params[3].offset, 24);
        assert_eq!(prog.params[3].width, 4);
        assert_eq!(prog.num_regions, 3);
        assert_eq!(prog.param_bytes, 28);
        // a branch was compiled, resolving the `$L__BB0_2` label to a real index.
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::Bra { .. })));
    }

    #[test]
    fn descriptor_words_round_trip() {
        let d = KernelDescriptor { ptx: VECADD_PTX.to_string(), entry: "vecadd".into(), block: [128, 1, 1] };
        let words = d.to_words();
        assert_eq!(words[0], KERNEL_MAGIC);
        let back = KernelDescriptor::from_words(&words).unwrap().unwrap();
        assert_eq!(back, d);
        // non-kernel words (SPIR-V-looking) are not mistaken for a descriptor.
        assert!(KernelDescriptor::from_words(&[0x0723_0203, 1, 2]).is_none());
    }

    #[test]
    fn rejects_malformed_ptx() {
        // unknown entry
        assert!(matches!(compile(VECADD_PTX, "nope", [1, 1, 1]), Err(GpuError::Ptx(_))));
        // unsupported opcode (warp shuffle is outside the shared-mem/atomics/barrier subset)
        let bad = ".entry k(.param .u32 x) { shfl.sync.down.b32 %r1, %r2, 1, 31, -1; ret; }";
        assert!(matches!(compile(bad, "k", [1, 1, 1]), Err(GpuError::Ptx(_))));
        // unterminated body
        let bad2 = ".entry k(.param .u32 x) { ret;";
        assert!(matches!(compile(bad2, "k", [1, 1, 1]), Err(GpuError::Ptx(_))));
        // unknown branch label
        let bad3 = ".entry k(.param .u32 x) { bra NOWHERE; ret; }";
        assert!(matches!(compile(bad3, "k", [1, 1, 1]), Err(GpuError::Ptx(_))));
    }

    /// Run a kernel whose single `.u64` pointer parameter is the only region, returning that region's
    /// bytes after execution. `out_bytes` sizes the output region.
    fn run_single_region(ptx: &str, entry: &str, out_bytes: usize) -> Vec<u8> {
        let prog = compile(ptx, entry, [1, 1, 1]).unwrap();
        let blob = vec![0u8; prog.param_bytes as usize];
        let mut regions = vec![vec![0u8; out_bytes]];
        execute(&prog, &blob, &mut regions, (1, 1, 1)).unwrap();
        regions.pop().unwrap()
    }

    #[test]
    fn fma_rn_is_fused_single_rounding() {
        // fma.rn.f32 must be a single-rounding fused multiply-add, not an unfused mul-then-add.
        // For 0.1f * 10 - 1: unfused rounds a*b to exactly 1.0 → result 0.0; fused keeps the
        // exact product → ~1.49e-8. The C mirror (cuda_shim.c) had the unfused bug.
        let ptx = r#"
            .entry fmac(.param .u64 p) {
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.f32 %f1, 0f3DCCCCCD;
                mov.f32 %f2, 0f41200000;
                mov.f32 %f3, 0fBF800000;
                fma.rn.f32 %f4, %f1, %f2, %f3;
                st.global.f32 [%rd2], %f4;
                ret;
            }
        "#;
        let out = run_single_region(ptx, "fmac", 4);
        let got = f32::from_le_bytes(out[..4].try_into().unwrap());
        let want = 0.1f32.mul_add(10.0, -1.0); // fused reference
        assert_eq!(got.to_bits(), want.to_bits());
        assert_ne!(got, 0.0, "an unfused mul-then-add would collapse to exactly 0.0");
    }

    #[test]
    fn setp_u32_compares_unsigned() {
        // setp.gt.u32 on 0x8000_0000 vs 1: unsigned → true (store 1.0). A signed compare would read
        // 0x8000_0000 as INT_MIN and yield false (store 0.0).
        let ptx = r#"
            .entry setpu(.param .u64 p) {
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.u32 %r1, 2147483648;
                mov.u32 %r2, 1;
                setp.gt.u32 %p1, %r1, %r2;
                @%p1 bra $T;
                mov.f32 %f1, 0f00000000;
                bra $S;
            $T:
                mov.f32 %f1, 0f3F800000;
            $S:
                st.global.f32 [%rd2], %f1;
                ret;
            }
        "#;
        let out = run_single_region(ptx, "setpu", 4);
        let got = f32::from_le_bytes(out[..4].try_into().unwrap());
        assert_eq!(got, 1.0, "0x80000000 > 1 must be true under unsigned comparison");
        // Sanity: the signed form of the same comparison is false.
        let ptx_s = ptx.replace("setp.gt.u32", "setp.gt.s32");
        let out_s = run_single_region(&ptx_s, "setpu", 4);
        let got_s = f32::from_le_bytes(out_s[..4].try_into().unwrap());
        assert_eq!(got_s, 0.0, "signed compare of INT_MIN > 1 is false");
    }

    #[test]
    fn mul_wide_u32_zero_extends() {
        // mul.wide.u32 0x8000_0000 * 2 = 0x1_0000_0000 (zero-extended). The signed form would
        // sign-extend to 0xFFFF_FFFF_0000_0000.
        let ptx = r#"
            .entry mulu(.param .u64 p) {
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.u32 %r1, 2147483648;
                mul.wide.u32 %rd3, %r1, 2;
                st.global.u64 [%rd2], %rd3;
                ret;
            }
        "#;
        let out = run_single_region(ptx, "mulu", 8);
        let got = u64::from_le_bytes(out[..8].try_into().unwrap());
        assert_eq!(got, 0x1_0000_0000u64);
        // The signed wide form sign-extends the negative operand.
        let ptx_s = ptx.replace("mul.wide.u32", "mul.wide.s32");
        let out_s = run_single_region(&ptx_s, "mulu", 8);
        let got_s = u64::from_le_bytes(out_s[..8].try_into().unwrap());
        assert_eq!(got_s, 0xFFFF_FFFF_0000_0000u64);
    }

    #[test]
    fn negative_global_offset_is_clean_out_of_bounds() {
        // A negative effective address must return a typed OutOfBounds, not panic on a slice index.
        let ptx = r#"
            .entry neg(.param .u64 p) {
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.f32 %f1, 0f3F800000;
                st.global.f32 [%rd2-16], %f1;
                ret;
            }
        "#;
        let prog = compile(ptx, "neg", [1, 1, 1]).unwrap();
        let blob = vec![0u8; prog.param_bytes as usize];
        let mut regions = vec![vec![0u8; 64]];
        let r = execute(&prog, &blob, &mut regions, (1, 1, 1));
        assert!(matches!(r, Err(GpuError::OutOfBounds)));
    }

    #[test]
    fn vecadd_lowers_to_wgsl_compute() {
        // The WGSL back-end emits a compute entry point with the launch ABI: params at binding 0, one
        // storage buffer per pointer region (a,b,c → region0/1/2 at bindings 1/2/3), the block dims as
        // the workgroup size, and the pc-dispatch loop that makes the `if (i>=n) return;` bra legal.
        let prog = compile(VECADD_PTX, "vecadd", [256, 1, 1]).unwrap();
        let wgsl = kernel_to_wgsl(&prog).unwrap();
        assert!(wgsl.contains("@compute @workgroup_size(256, 1, 1)"), "{wgsl}");
        assert!(wgsl.contains("fn vecadd("), "{wgsl}");
        assert!(wgsl.contains("var<storage, read> params: array<u32>;"), "{wgsl}");
        assert!(wgsl.contains("@binding(1) var<storage, read_write> region0: array<u32>;"), "{wgsl}");
        assert!(wgsl.contains("@binding(3) var<storage, read_write> region2: array<u32>;"), "{wgsl}");
        // the store lands in region2 (c), the load reads region0/region1 (a/b), and the index is a global
        // thread id built from ctaid*ntid+tid → workgroup_id / num / local builtins are referenced.
        assert!(wgsl.contains("region2[") && wgsl.contains("= r"), "store into c: {wgsl}");
        assert!(wgsl.contains("lid.x") && wgsl.contains("wid.x"), "uses SIMT builtins: {wgsl}");
        assert!(wgsl.contains("loop {") && wgsl.contains("switch pc {"), "pc-dispatch: {wgsl}");
        // the scalar bound `n` (param 3, byte offset 24) is read from the param blob at word 6.
        assert!(wgsl.contains("params[6u]"), "reads n from params: {wgsl}");
    }

    #[test]
    fn wgsl_rejects_64bit_global_access() {
        // A 64-bit global store can't be expressed over the `array<u32>` word view without a two-word
        // split, so it's outside this lowering's elementwise subset — a typed rejection, not a bad emit.
        let ptx = r#"
            .entry st64(.param .u64 p) {
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.u32 %r1, 7;
                cvt.s64.s32 %rd3, %r1;
                st.global.u64 [%rd2], %rd3;
                ret;
            }
        "#;
        let prog = compile(ptx, "st64", [1, 1, 1]).unwrap();
        assert!(matches!(kernel_to_wgsl(&prog), Err(GpuError::Ptx(_))));
    }

    #[test]
    fn interpreter_runs_vecadd_directly() {
        // Compile then run the interpreter directly over host buffers (no backend), N=10.
        let n = 10usize;
        let prog = compile(VECADD_PTX, "vecadd", [4, 1, 1]).unwrap();
        let a: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..n).map(|i| (2 * i) as f32).collect();
        let to_bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let mut regions = vec![to_bytes(&a), to_bytes(&b), vec![0u8; n * 4]];
        // param blob: a,b,c pointers (values irrelevant to the interpreter) + n at offset 24.
        let mut blob = vec![0u8; 28];
        blob[24..28].copy_from_slice(&(n as u32).to_le_bytes());
        let grid = (((n + 3) / 4) as u32, 1, 1);
        execute(&prog, &blob, &mut regions, grid).unwrap();
        for i in 0..n {
            let c = f32::from_le_bytes(regions[2][i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(c, a[i] + b[i]);
        }
    }

    #[test]
    fn compiles_reduction_with_shared_bar_atom() {
        // The reduction kernel declares 1024 bytes of shared memory and lowers `bar.sync` + the global
        // `atomicAdd` to the new IR forms.
        let prog = compile(REDUCE_PTX, "block_reduce", [256, 1, 1]).unwrap();
        assert_eq!(prog.shared_bytes, 1024);
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::Bar)));
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::StShared { .. })));
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::LdShared { .. })));
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::AtomGlobal { op, .. } if *op == ATOM_ADD)));
        assert!(prog.insts.iter().any(|i| matches!(i, Inst::Shift { .. })));
        // `in` (region 0) is plain; `out` (region 1) is atomic-only.
        let atomic = atomic_regions(&prog).unwrap();
        assert_eq!(atomic, vec![false, true]);
    }

    /// CPU-oracle reduction: sum `data` over the grid the same way the GPU would, returning `*out`.
    fn oracle_reduce(data: &[i32], block: u32) -> i32 {
        let n = data.len() as u32;
        let prog = compile(REDUCE_PTX, "block_reduce", [block, 1, 1]).unwrap();
        let in_bytes: Vec<u8> = data.iter().flat_map(|x| x.to_le_bytes()).collect();
        let mut regions = vec![in_bytes, vec![0u8; 4]]; // region0=in, region1=out (single accumulator)
        let mut blob = vec![0u8; prog.param_bytes as usize];
        // param 2 (n) is the only scalar; find its offset.
        let off = prog.params[2].offset as usize;
        blob[off..off + 4].copy_from_slice(&n.to_le_bytes());
        let grid = (n.div_ceil(block), 1, 1);
        execute(&prog, &blob, &mut regions, grid).unwrap();
        i32::from_le_bytes(regions[1][..4].try_into().unwrap())
    }

    #[test]
    fn interpreter_runs_reduction_via_shared_and_barriers() {
        // A block-level tree reduction with shared memory + bar.sync + a final global atomicAdd, summed
        // across multiple blocks, must equal the plain arithmetic sum. This is the cooperative-execution
        // oracle: a naive run-each-thread-to-completion would mis-order the shared-memory hand-offs.
        for &n in &[1usize, 5, 256, 257, 1000, 2048] {
            let data: Vec<i32> = (0..n as i32).map(|i| (i % 17) - 8).collect();
            let want: i32 = data.iter().sum();
            let got = oracle_reduce(&data, 256);
            assert_eq!(got, want, "reduction sum mismatch at n={n}");
        }
    }

    #[test]
    fn reduction_lowers_to_wgsl_with_shared_barrier_atomic() {
        let prog = compile(REDUCE_PTX, "block_reduce", [256, 1, 1]).unwrap();
        let wgsl = kernel_to_wgsl(&prog).unwrap();
        // shared memory → a workgroup array of 256 words; the barrier scheme + atomic accumulator appear.
        assert!(wgsl.contains("var<workgroup> shmem: array<u32, 256>;"), "{wgsl}");
        assert!(wgsl.contains("workgroupBarrier();"), "{wgsl}");
        assert!(wgsl.contains("shmem["), "shared access: {wgsl}");
        // region1 (out) is atomic-typed and hit by atomicAdd; region0 (in) stays a plain u32 buffer.
        assert!(wgsl.contains("region1: array<atomic<u32>>;"), "{wgsl}");
        assert!(wgsl.contains("region0: array<u32>;"), "{wgsl}");
        assert!(wgsl.contains("atomicAdd(&region1["), "{wgsl}");
        // the cooperative retire-counter bookkeeping is present.
        assert!(wgsl.contains("dd_retired_count"), "{wgsl}");
        assert!(wgsl.contains("local_invocation_index"), "{wgsl}");
    }

    #[test]
    fn elementwise_wgsl_unchanged_by_shared_support() {
        // A kernel with no barrier keeps the simple (non-cooperative) loop: no workgroup barrier, no
        // shared array, no retire-counter — the elementwise path is byte-for-byte as before.
        let prog = compile(VECADD_PTX, "vecadd", [256, 1, 1]).unwrap();
        let wgsl = kernel_to_wgsl(&prog).unwrap();
        assert!(!wgsl.contains("workgroupBarrier"), "elementwise must not synchronize: {wgsl}");
        assert!(!wgsl.contains("var<workgroup>"), "elementwise has no shared memory: {wgsl}");
        assert!(!wgsl.contains("dd_retired_count"), "elementwise has no coop bookkeeping: {wgsl}");
        assert!(wgsl.contains("if (pc < 0) { break; }"), "keeps the simple loop guard: {wgsl}");
    }

    #[test]
    fn global_atomic_add_accumulates() {
        // A standalone global atomicAdd kernel: every thread adds its own `tid+1` into a single
        // accumulator; the result must be the triangular number sum(1..=N).
        let ptx = r#"
            .entry accum(.param .u64 p) {
                .reg .b32 %r<5>;
                .reg .b64 %rd<3>;
                ld.param.u64 %rd1, [p];
                cvta.to.global.u64 %rd2, %rd1;
                mov.u32 %r1, %tid.x;
                add.s32 %r2, %r1, 1;
                atom.global.add.u32 %r3, [%rd2], %r2;
                ret;
            }
        "#;
        let block = 64u32;
        let prog = compile(ptx, "accum", [block, 1, 1]).unwrap();
        assert_eq!(atomic_regions(&prog).unwrap(), vec![true]);
        let mut regions = vec![vec![0u8; 4]];
        let blob = vec![0u8; prog.param_bytes as usize];
        execute(&prog, &blob, &mut regions, (1, 1, 1)).unwrap();
        let got = u32::from_le_bytes(regions[0][..4].try_into().unwrap());
        assert_eq!(got, (block * (block + 1)) / 2);
        // it also lowers to WGSL cleanly (atomic region typing).
        assert!(kernel_to_wgsl(&prog).is_ok());
    }

    #[test]
    fn shared_copy_kernel_roundtrips() {
        // st.shared → bar.sync → ld.shared: thread `tid` writes `tid*3` to sdata[tid], barrier, then
        // reads sdata[blockDim-1-tid] and stores it to out[tid] (a reversal that only works if the
        // whole block's shared writes are visible after the barrier).
        let ptx = r#"
            .entry shcopy(.param .u64 p) {
                .reg .pred %p<2>;
                .reg .b32 %r<12>;
                .reg .b64 %rd<6>;
                .shared .align 4 .b8 sdata[32];
                ld.param.u64 %rd1, [p];
                mov.u32 %r1, %tid.x;
                mov.u32 %r2, %ntid.x;
                mul.lo.s32 %r3, %r1, 4;
                mov.u32 %r4, sdata;
                add.s32 %r5, %r4, %r3;
                mul.lo.s32 %r6, %r1, 3;
                st.shared.u32 [%r5], %r6;
                bar.sync 0;
                sub.s32 %r7, %r2, 1;
                sub.s32 %r8, %r7, %r1;
                mul.lo.s32 %r9, %r8, 4;
                add.s32 %r10, %r4, %r9;
                ld.shared.u32 %r11, [%r10];
                cvta.to.global.u64 %rd2, %rd1;
                mul.wide.u32 %rd3, %r1, 4;
                add.s64 %rd4, %rd2, %rd3;
                st.global.u32 [%rd4], %r11;
                ret;
            }
        "#;
        let block = 8u32;
        let prog = compile(ptx, "shcopy", [block, 1, 1]).unwrap();
        assert_eq!(prog.shared_bytes, 32);
        let mut regions = vec![vec![0u8; (block * 4) as usize]];
        let blob = vec![0u8; prog.param_bytes as usize];
        execute(&prog, &blob, &mut regions, (1, 1, 1)).unwrap();
        for tid in 0..block {
            let got = u32::from_le_bytes(regions[0][(tid * 4) as usize..(tid * 4 + 4) as usize].try_into().unwrap());
            assert_eq!(got, (block - 1 - tid) * 3, "reversal at tid={tid}");
        }
        assert!(kernel_to_wgsl(&prog).is_ok());
    }
}
