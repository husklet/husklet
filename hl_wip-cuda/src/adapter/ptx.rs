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

fn err(m: impl Into<String>) -> GpuError {
    GpuError::Kernel(m.into())
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
        if toks.is_empty() || toks[0] != ".param" {
            return Err(err(format!("bad .param decl: `{s}`")));
        }
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

/// Allocates a real register for each distinct special register that appears in an OPERAND position
/// (e.g. `mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x` — legal PTX). A `mov %r, %sreg` lowers straight to
/// [`Inst::MovSReg`]; an operand-position special register instead needs a register to read from, so its
/// value is materialized once by a [`Inst::MovSReg`] PRELUDE emitted at the very top of the instruction
/// stream (special registers are thread/block-invariant, so a single leading read is always correct) and
/// operand references then read that register. This routes operand-position special registers through the
/// EXACT same runtime resolution as the `mov` form — instead of the old footgun of silently interning
/// `%ntid.x` as a fresh, zero-valued virtual register (→ every thread computing index 0).
#[derive(Default)]
struct SRegAlloc {
    /// `(sreg code, assigned register)` in first-use order — also the prelude emission order.
    regs: Vec<(u8, u16)>,
}
impl SRegAlloc {
    /// The register holding special register `sreg`, allocating it (and scheduling its prelude
    /// `MovSReg`) on first use. The synthetic interner key starts with `$`, which a real `%`-register
    /// (stripped of its `%`) never can — so it cannot collide with a virtual register name.
    fn reg_for(&mut self, sreg: u8, interner: &mut Interner) -> u16 {
        if let Some(&(_, r)) = self.regs.iter().find(|(s, _)| *s == sreg) {
            return r;
        }
        let r = interner.get(&format!("$sreg${sreg}"));
        self.regs.push((sreg, r));
        r
    }
    /// The leading `MovSReg` block that materializes every operand-position special register.
    fn prelude(&self) -> Vec<Inst> {
        self.regs.iter().map(|&(sreg, d)| Inst::MovSReg { d, sreg }).collect()
    }
}

/// Classify a `%`-prefixed OPERAND token:
/// - `Ok(Some(sr))` — a recognized special register (resolve it via [`SRegAlloc`]);
/// - `Ok(None)`     — a plain virtual register (intern it normally);
/// - `Err(..)`      — a special-register-SHAPED token we do not recognize. A namespaced `%ns.field`
///   token (`%tid.w`, `%bogus.x`) or a known dotless special register we do not model (`%laneid`,
///   `%warpid`, …) is REJECTED rather than silently interned as a fresh zero register — that silent path
///   is the wrong-result footgun this guards against. A plain virtual register never contains a `.`.
fn classify_reg_operand(tok: &str) -> Result<Option<u8>> {
    if let Some(sr) = parse_sreg(tok) {
        return Ok(Some(sr));
    }
    let name = strip_reg(tok);
    let base = name.split('.').next().unwrap_or(name);
    if name.contains('.') || is_special_reg_base(base) {
        return Err(err(format!(
            "unknown/unsupported special register `%{name}` used as operand \
             (modeled: %tid/%ntid/%ctaid/%nctaid with a .x/.y/.z component)"
        )));
    }
    Ok(None)
}

/// Dotless special-register names PTX defines that we do NOT model. Recognized here ONLY so that an
/// operand use errors honestly instead of silently interning a fresh zero register. The dimension/index
/// roots (`tid`/`ntid`/`ctaid`/`nctaid`) are deliberately NOT listed: they are only ever special
/// registers WITH a `.x/.y/.z` component (handled by the dot rule in [`classify_reg_operand`]), and a
/// bare `%tid`/`%ntid`/… is a perfectly ordinary virtual-register name real kernels use.
fn is_special_reg_base(base: &str) -> bool {
    matches!(
        base,
        "laneid" | "warpid" | "nwarpid" | "warpsize" | "gridid" | "smid" | "nsmid"
            | "lanemask_eq" | "lanemask_le" | "lanemask_lt" | "lanemask_ge" | "lanemask_gt"
            | "clock" | "clock64" | "clock_hi" | "globaltimer"
    )
}

/// Resolve a `%`-register operand token to a register index: a recognized special register routes to its
/// [`SRegAlloc`]-assigned register (materialized by the prelude); a plain virtual register is interned;
/// an unrecognized special-register-shaped token errors (via [`classify_reg_operand`]).
fn resolve_reg(tok: &str, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<u16> {
    match classify_reg_operand(tok)? {
        Some(sr) => Ok(sregs.reg_for(sr, interner)),
        None => Ok(interner.get(strip_reg(tok))),
    }
}

/// Strip PTX comments (`//` line, `/* … */` block), replacing each with a space to keep token bounds.
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

fn parse_body(body: &str, params: &[(String, u32)], interner: &mut Interner) -> Result<RawFn> {
    let flat = strip_comments(body).replace('\n', " ").replace('\r', " ");
    let raw_stmts: Vec<&str> = flat.split(';').collect();

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
        // A `.shared` state-space variable declaration — either bare (`.shared …`) or, as nvcc actually
        // emits it, behind linkage qualifiers (`.extern .shared …`, `.visible .shared …`). It is a
        // directive line (starts with `.`) with a standalone `.shared` token; an instruction that touches
        // shared memory (`ld.shared.f32`, `st.shared.b32`) carries `.shared` inside its opcode token, never
        // as a bare token, so it is not misrouted here. Routing the qualified form matters: `parse_shared_decl`
        // is what rejects the dynamic (`extern`, unsized `name[]`) form — skipping it would silently model a
        // dynamic-shared kernel as having zero shared memory (wrong results / a deferred OOB) instead of an
        // honest `GpuError::Kernel`.
        if s.starts_with('.') {
            if s.split_whitespace().any(|t| t == ".shared") {
                parse_shared_decl(&s, &mut shared_syms, &mut shared_cursor)?;
            }
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

    let mut sregs = SRegAlloc::default();
    let mut insts = Vec::with_capacity(stmts.len());
    let mut ld_param = Vec::new();
    for s in &stmts {
        let inst = parse_inst(s, &param_idx, &labels, &shared_syms, interner, &mut sregs)?;
        if let Inst::LdParam { d, param } = inst {
            ld_param.push((d, param));
        }
        insts.push(inst);
    }

    // Materialize operand-position special registers: emit their `MovSReg` prelude at the top of the
    // stream, then shift every branch target past it. Branch targets are instruction indices (== statement
    // ordinals here), so they move by the prelude length; `ld_param` seeds are register indices, unaffected.
    let prelude = sregs.prelude();
    if !prelude.is_empty() {
        let n = prelude.len() as u32;
        for inst in &mut insts {
            if let Inst::Bra { target, .. } = inst {
                *target += n;
            }
        }
        let mut all = prelude;
        all.extend(insts);
        insts = all;
    }

    Ok(RawFn { insts, reg_count: interner.count(), ld_param, shared_bytes })
}

/// Parse a `.shared` declaration, reserving space + recording the symbol's base byte offset. Dynamic
/// `extern` shared (`name[]`, sized at launch) is rejected — out of the statically-sized subset.
fn parse_shared_decl(
    s: &str,
    syms: &mut std::collections::HashMap<String, u32>,
    cursor: &mut u32,
) -> Result<()> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let ty = toks
        .iter()
        .skip(1)
        .find(|t| t.starts_with('.') && *t != &".align" && *t != &".shared")
        .copied();
    let name_tok = *toks.last().ok_or_else(|| err(format!("bad .shared decl: `{s}`")))?;
    let elem = ty.map(type_width).transpose()?.unwrap_or(1);
    let (name, count) = if let Some(lb) = name_tok.find('[') {
        let name = &name_tok[..lb];
        let inner = name_tok[lb + 1..].trim_end_matches([']', ';']).trim();
        if inner.is_empty() {
            return Err(err(format!("dynamic (extern) .shared unsupported: `{s}`")));
        }
        let count: u32 =
            parse_imm_i(inner).map_err(|_| err(format!("bad .shared array size: `{s}`")))? as u32;
        (name, count)
    } else {
        (name_tok.trim_end_matches(';'), 1u32)
    };
    let name: String =
        name.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
    if name.is_empty() {
        return Err(err(format!(".shared decl missing name: `{s}`")));
    }
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
    sregs: &mut SRegAlloc,
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
        "bar" | "barrier" => Inst::Bar,
        "membar" | "fence" => Inst::Nop,
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
            if is_reg(src) {
                // A `%`-source is a special register (→ MovSReg) or a plain register (→ MovReg); an
                // unrecognized special-register-shaped token errors instead of silently reading a zero reg.
                match classify_reg_operand(src)? {
                    Some(sr) => Inst::MovSReg { d, sreg: sr },
                    None => Inst::MovReg { d, s: reg(interner, src) },
                }
            } else if let Some(&off) = shared_syms.get(src.trim()) {
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
                let (addr, off) = parse_mem(&ops[1], interner, sregs)?;
                Inst::LdGlobal { d, addr, off, ty: gtype(opcode)? }
            } else if has("shared") {
                let (base, off) = parse_shared_mem(&ops[1], shared_syms, interner, sregs)?;
                Inst::LdShared { d, base, off, ty: gtype(opcode)? }
            } else {
                return Err(err(format!("unsupported ld space: `{stmt}`")));
            }
        }
        "st" => {
            need!(2);
            if has("global") {
                let (addr, off) = parse_mem(&ops[0], interner, sregs)?;
                let src = parse_op(&ops[1], interner, has("f32"), sregs)?;
                Inst::StGlobal { addr, off, src, ty: gtype(opcode)? }
            } else if has("shared") {
                let (base, off) = parse_shared_mem(&ops[0], shared_syms, interner, sregs)?;
                let src = parse_op(&ops[1], interner, has("f32"), sregs)?;
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
                Inst::FAdd { d, a: parse_op(&ops[1], interner, true, sregs)?, b: parse_op(&ops[2], interner, true, sregs)? }
            } else {
                Inst::IAdd {
                    d,
                    a: parse_op(&ops[1], interner, false, sregs)?,
                    b: parse_op(&ops[2], interner, false, sregs)?,
                    wide: has("s64") || has("u64") || has("b64"),
                }
            }
        }
        "sub" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            if has("f32") {
                Inst::FSub { d, a: parse_op(&ops[1], interner, true, sregs)?, b: parse_op(&ops[2], interner, true, sregs)? }
            } else {
                Inst::ISub {
                    d,
                    a: parse_op(&ops[1], interner, false, sregs)?,
                    b: parse_op(&ops[2], interner, false, sregs)?,
                    wide: has("s64") || has("u64") || has("b64"),
                }
            }
        }
        "mul" => {
            need!(3);
            let d = reg(interner, &ops[0]);
            if has("f32") {
                Inst::FMul { d, a: parse_op(&ops[1], interner, true, sregs)?, b: parse_op(&ops[2], interner, true, sregs)? }
            } else {
                Inst::IMul {
                    d,
                    a: parse_op(&ops[1], interner, false, sregs)?,
                    b: parse_op(&ops[2], interner, false, sregs)?,
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
                a: parse_op(&ops[1], interner, false, sregs)?,
                b: parse_op(&ops[2], interner, false, sregs)?,
                c: parse_op(&ops[3], interner, false, sregs)?,
            }
        }
        "fma" => {
            need!(4);
            let d = reg(interner, &ops[0]);
            Inst::FFma {
                d,
                a: parse_op(&ops[1], interner, true, sregs)?,
                b: parse_op(&ops[2], interner, true, sregs)?,
                c: parse_op(&ops[3], interner, true, sregs)?,
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
                a: parse_op(&ops[1], interner, false, sregs)?,
                b: parse_op(&ops[2], interner, false, sregs)?,
                cmp,
                unsigned: has("u32") || has("u64") || has("u16") || has("u8"),
            }
        }
        "cvt" => {
            need!(2);
            let d = reg(interner, &ops[0]);
            let s = parse_op(&ops[1], interner, has("f32"), sregs)?;
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
                a: parse_op(&ops[1], interner, false, sregs)?,
                b: parse_op(&ops[2], interner, false, sregs)?,
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
                a: parse_op(&ops[1], interner, false, sregs)?,
                b: parse_op(&ops[2], interner, false, sregs)?,
                op,
            }
        }
        "atom" | "red" => {
            let has_dest = base == "atom";
            let op = atom_op(opcode)?;
            let unsigned = has("u32") || has("u64") || has("u16") || has("b32") || has("b64");
            if opcode.contains("f32") || opcode.contains("f64") {
                return Err(err(format!("floating-point atomics unsupported (WGSL has no f32 atomics): `{stmt}`")));
            }
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
                let cmp = parse_op(&ops[i], interner, false, sregs)?;
                let val = parse_op(&ops[i + 1], interner, false, sregs)?;
                (cmp, val)
            } else {
                (Op::ImmI(0), parse_op(&ops[i], interner, false, sregs)?)
            };
            if has("shared") {
                let (b, off) = parse_shared_mem(addr_tok, shared_syms, interner, sregs)?;
                Inst::AtomShared { d, base: b, off, op, cmp, val, unsigned }
            } else if has("global") || !has("shared") {
                let (addr, off) = parse_mem(addr_tok, interner, sregs)?;
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

/// Parse a shared-memory address operand `[base(+off)]` (`base` = register or `.shared` symbol).
fn parse_shared_mem(
    tok: &str,
    shared_syms: &std::collections::HashMap<String, u32>,
    interner: &mut Interner,
    sregs: &mut SRegAlloc,
) -> Result<(Op, i64)> {
    let inner = tok.trim().trim_start_matches('[').trim_end_matches(']').trim();
    let (base_tok, off) = if let Some(pos) = inner.find(['+', '-']) {
        let (b, o) = inner.split_at(pos);
        (b.trim(), parse_imm_i(o.trim())?)
    } else {
        (inner, 0)
    };
    if is_reg(base_tok) {
        Ok((Op::Reg(resolve_reg(base_tok, interner, sregs)?), off))
    } else if let Some(&sym) = shared_syms.get(base_tok) {
        Ok((Op::ImmI(sym as i64), off))
    } else {
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

/// Split an operand list on commas (no commas occur inside `[a+b]` for our subset).
fn split_operands(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
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
fn parse_mem(tok: &str, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<(u16, i64)> {
    let inner = tok.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if let Some(pos) = inner.find(['+', '-']) {
        let (r, o) = inner.split_at(pos);
        let off = parse_imm_i(o.trim())?;
        Ok((resolve_reg(r.trim(), interner, sregs)?, off))
    } else {
        Ok((resolve_reg(inner, interner, sregs)?, 0))
    }
}

fn parse_op(tok: &str, interner: &mut Interner, want_float: bool, sregs: &mut SRegAlloc) -> Result<Op> {
    let t = tok.trim();
    if is_reg(t) {
        Ok(Op::Reg(resolve_reg(t, interner, sregs)?))
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
        t.parse::<f32>().map(|f| f.to_bits()).map_err(|_| err(format!("bad float immediate `{t}`")))
    }
}
