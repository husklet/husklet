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
/// bounds guard. Used by the end-to-end tests + the `dd-gpu/cuda` dlopen ABI test as the reference
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
            Inst::LdGlobal { addr, .. } | Inst::StGlobal { addr, .. } => {
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

fn parse_body(
    body: &str,
    params: &[(String, u32)],
    interner: &mut Interner,
) -> Result<RawFn> {
    // statement split: newlines→spaces, then split on ';'. Labels (`X:`) carry no ';' and get folded
    // onto the following statement; we peel leading `label:` tokens off.
    let flat = body.replace('\n', " ").replace('\r', " ");
    let raw_stmts: Vec<&str> = flat.split(';').collect();

    // First pass: collect (label_defs, instructions_as_text) so we can resolve labels → indices.
    let mut labels: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut stmts: Vec<String> = Vec::new();
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
        // skip directives inside the body (e.g. `.reg .b32 %r<6>`, `.loc ...`).
        if s.starts_with('.') {
            continue;
        }
        stmts.push(s);
    }

    let param_idx: std::collections::HashMap<&str, u16> = params
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i as u16))
        .collect();

    let mut insts = Vec::with_capacity(stmts.len());
    let mut ld_param = Vec::new();
    for s in &stmts {
        let inst = parse_inst(s, &param_idx, &labels, interner)?;
        if let Inst::LdParam { d, param } = inst {
            ld_param.push((d, param));
        }
        insts.push(inst);
    }

    Ok(RawFn {
        insts,
        reg_count: interner.count(),
        ld_param,
    })
}

fn parse_inst(
    stmt: &str,
    params: &std::collections::HashMap<&str, u16>,
    labels: &std::collections::HashMap<String, u32>,
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
        "bar" => Inst::Nop, // bar.sync — no shared memory in this oracle
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
            } else {
                return Err(err(format!("unsupported ld space: `{stmt}`")));
            }
        }
        "st" => {
            need!(2);
            if !has("global") {
                return Err(err(format!("unsupported st space: `{stmt}`")));
            }
            let (addr, off) = parse_mem(&ops[0], interner)?;
            let src = parse_op(&ops[1], interner, has("f32"))?;
            Inst::StGlobal { addr, off, src, ty: gtype(opcode)? }
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
        other => return Err(err(format!("unsupported opcode `{other}`: `{stmt}`"))),
    };
    Ok(inst)
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
    for cz in 0..gz.max(1) {
        for cy in 0..gy.max(1) {
            for cx in 0..gx.max(1) {
                for tz in 0..bz.max(1) {
                    for ty in 0..by.max(1) {
                        for tx in 0..bx.max(1) {
                            let sr = [
                                tx, ty, tz, bx, by, bz, cx, cy, cz, gx.max(1), gy.max(1), gz.max(1),
                            ];
                            run_thread(prog, param_blob, regions, &sr)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn run_thread(
    prog: &KernelProgram,
    param_blob: &[u8],
    regions: &mut [Vec<u8>],
    sr: &[u32; 12],
) -> Result<()> {
    let mut regs = vec![Val::I(0); prog.reg_count as usize];
    let mut pc = 0usize;
    let mut steps = 0u64;
    let step_cap = 1_000_000u64;

    let eval = |regs: &Vec<Val>, op: &Op| -> Val {
        match op {
            Op::Reg(r) => regs[*r as usize],
            Op::ImmI(i) => Val::I(*i as u64),
            Op::ImmF(b) => Val::F(f32::from_bits(*b)),
        }
    };

    while pc < prog.insts.len() {
        steps += 1;
        if steps > step_cap {
            return Err(err("kernel exceeded step cap (suspected infinite loop)"));
        }
        match &prog.insts[pc] {
            Inst::Ret => return Ok(()),
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
                let (va, vb) = (eval(&regs, a), eval(&regs, b));
                regs[*d as usize] = ptr_int_add(va, vb, *wide, 1);
            }
            Inst::ISub { d, a, b, wide } => {
                let (va, vb) = (eval(&regs, a), eval(&regs, b));
                regs[*d as usize] = ptr_int_add(va, vb, *wide, -1);
            }
            Inst::IMul { d, a, b, wide, unsigned } => {
                let (va, vb) = (eval(&regs, a).as_u64(), eval(&regs, b).as_u64());
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
                let va = eval(&regs, a).as_i64() as i32;
                let vb = eval(&regs, b).as_i64() as i32;
                let vc = eval(&regs, c).as_i64() as i32;
                regs[*d as usize] = Val::I(va.wrapping_mul(vb).wrapping_add(vc) as u32 as u64);
            }
            Inst::Setp { d, a, b, cmp, unsigned } => {
                let r = if *unsigned {
                    let va = eval(&regs, a).as_u64() as u32;
                    let vb = eval(&regs, b).as_u64() as u32;
                    match *cmp {
                        CMP_EQ => va == vb,
                        CMP_NE => va != vb,
                        CMP_LT => va < vb,
                        CMP_LE => va <= vb,
                        CMP_GT => va > vb,
                        _ => va >= vb, // CMP_GE
                    }
                } else {
                    let va = eval(&regs, a).as_u64() as i32;
                    let vb = eval(&regs, b).as_u64() as i32;
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
                    pc = *target as usize;
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
                let v = eval(&regs, src);
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
                regs[*d as usize] = Val::F(eval(&regs, a).as_f32() + eval(&regs, b).as_f32())
            }
            Inst::FSub { d, a, b } => {
                regs[*d as usize] = Val::F(eval(&regs, a).as_f32() - eval(&regs, b).as_f32())
            }
            Inst::FMul { d, a, b } => {
                regs[*d as usize] = Val::F(eval(&regs, a).as_f32() * eval(&regs, b).as_f32())
            }
            Inst::FFma { d, a, b, c } => {
                let (fa, fb, fc) = (eval(&regs, a).as_f32(), eval(&regs, b).as_f32(), eval(&regs, c).as_f32());
                regs[*d as usize] = Val::F(fa.mul_add(fb, fc));
            }
            Inst::Cvt { d, s, kind } => {
                let v = eval(&regs, s);
                regs[*d as usize] = match *kind {
                    CVT_F32_FROM_S32 => Val::F(v.as_u64() as i32 as f32),
                    CVT_S64_FROM_S32 => Val::I(v.as_u64() as i32 as i64 as u64),
                    CVT_S32_FROM_F32 => Val::I(v.as_f32() as i32 as u32 as u64),
                    _ => v,
                };
            }
        }
        pc += 1;
    }
    Ok(())
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
        // unsupported opcode
        let bad = ".entry k(.param .u32 x) { atom.global.add.u32 %r1, [%rd1], 1; ret; }";
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
}
