//! Host-side shader translation to WGSL — the single seam that turns every shader payload the protocol
//! carries into the one language wgpu compiles.
//!
//! Two source languages feed it:
//!
//! * **kernel-IR** ([`KernelProgram`]): [`kernel_to_wgsl`] lowers the neutral compute IR (the compiled
//!   form of a driver's PTX front-end) to a WGSL compute entry point. The CPU oracle *interprets* this IR;
//!   here it becomes real WGSL that runs on the GPU. Ported verbatim (semantics-preserving) from
//!   `hl-gpu/src/ptx.rs::kernel_to_wgsl` so the executed kernel matches the oracle byte-for-byte.
//! * **SPIR-V / GLSL** graphics shaders: [`spirv_to_wgsl`] / [`glsl_to_wgsl`] run naga (spv-in / glsl-in →
//!   wgsl-out). Ported from the reference `hl-gpu-wgpu/src/shader.rs`.
//!
//! The kernel ABI the emitted compute WGSL declares: `@group(0) @binding(0)` is the flat `params` blob
//! (`array<u32>`, read), and `@binding(r+1)` is pointer region `r` (`array<u32>`/`array<atomic<u32>>`,
//! read_write) for `r in 0..num_regions`. A bind group built from the protocol descriptor maps binding 0
//! → the param buffer and binding `r+1` → the region-`r` storage buffer, exactly the layout the vecadd /
//! store-one conformance programs encode.

use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, BIT_AND, BIT_OR, CMP_EQ, CMP_GT, CMP_LE, CMP_LT, CMP_NE,
    CVT_F32_FROM_S32, CVT_S32_FROM_F32, CVT_S64_FROM_S32, SHIFT_LEFT, SR_CTAID_X, SR_CTAID_Y,
    SR_CTAID_Z, SR_NCTAID_X, SR_NCTAID_Y, SR_NTID_X, SR_NTID_Y, SR_NTID_Z, SR_TID_X, SR_TID_Y,
    SR_TID_Z, ATOM_ADD, ATOM_AND, ATOM_CAS, ATOM_EXCH, ATOM_MAX, ATOM_MIN, ATOM_OR, ATOM_XOR,
};
use hl_gpu::{GpuError, Result};

fn err(m: impl Into<String>) -> GpuError {
    // The kernel/shader lowering surfaces its failures as a typed, message-carrying error so a program
    // outside the supported subset is a clean diagnostic, never a silent wrong-shader substitution.
    GpuError::Kernel(m.into())
}

// ===================================================================================================
// SPIR-V / GLSL graphics shaders → WGSL (naga)
// ===================================================================================================

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Translate a SPIR-V word payload to WGSL via naga (spv-in → validate → wgsl-out). Returns the WGSL
/// text wgpu compiles. A payload without the SPIR-V magic is rejected (the strict ABI never falls back
/// to a built-in shader).
pub fn spirv_to_wgsl(words: &[u32]) -> Result<String> {
    if words.first().copied() != Some(SPIRV_MAGIC) {
        return Err(GpuError::Invalid("wgpu: shader payload is not SPIR-V"));
    }
    let bytes: &[u8] = bytemuck::cast_slice(words);
    let module = naga::front::spv::parse_u8_slice(bytes, &naga::front::spv::Options::default())
        .map_err(|e| err(format!("spirv-in: {e:?}")))?;
    module_to_wgsl(&module)
}

/// Translate GLSL source (the GLES path) to WGSL for `stage`. Part of the shader-translation surface
/// (the guest GLES front end forwards GLSL); not exercised by the current conformance suite.
#[allow(dead_code)]
pub fn glsl_to_wgsl(src: &str, stage: naga::ShaderStage) -> Result<String> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|e| err(format!("glsl-in: {e:?}")))?;
    module_to_wgsl(&module)
}

fn module_to_wgsl(module: &naga::Module) -> Result<String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| err(format!("validate: {e:?}")))?;
    naga::back::wgsl::write_string(module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| err(format!("wgsl-out: {e}")))
}

// ===================================================================================================
// kernel-IR → WGSL compute (ported from hl-gpu/src/ptx.rs::kernel_to_wgsl)
// ===================================================================================================

fn gty_elem_bytes(ty: u8) -> Result<u32> {
    match ty {
        gty::F32 | gty::U32 => Ok(4),
        gty::U64 => Err(err("wgsl lowering: 64-bit global load/store unsupported (elementwise subset)")),
        other => Err(err(format!("wgsl lowering: unknown global type {other}"))),
    }
}

/// Static per-register pointer-region analysis: `out[r] == Some(region)` iff register `r` holds a device
/// address into that pointer-parameter region at its most recent definition. Forward, last-write-wins.
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

/// Which pointer regions are accessed atomically → those storage buffers must be `array<atomic<u32>>`.
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

/// Lower a compiled [`KernelProgram`] to a WGSL compute shader whose entry point is `prog.entry`. Returns
/// a typed error for any instruction outside the elementwise subset this lowering supports.
pub fn kernel_to_wgsl(prog: &KernelProgram) -> Result<String> {
    let region = region_analysis(prog);
    let region_of = |addr: u16| -> Result<u32> {
        region[addr as usize].ok_or_else(|| {
            err("wgsl lowering: global access through a value with no static pointer region")
        })
    };
    let atomic = atomic_regions(prog)?;
    let shared_atomic = prog.insts.iter().any(|i| matches!(i, Inst::AtomShared { .. }));
    let coop = prog.insts.iter().any(|i| matches!(i, Inst::Bar));

    let shared_idx =
        |base: &Op, off: i64, elem: u32| format!("(({} + {}u) / {elem}u)", op_u32(base), off as u32);

    let mut s = String::new();
    s.push_str("@group(0) @binding(0) var<storage, read> params: array<u32>;\n");
    for r in 0..prog.num_regions {
        let elem_ty = if atomic[r as usize] { "atomic<u32>" } else { "u32" };
        s.push_str(&format!(
            "@group(0) @binding({}) var<storage, read_write> region{r}: array<{elem_ty}>;\n",
            r + 1
        ));
    }
    if prog.shared_bytes > 0 {
        let words = (prog.shared_bytes / 4).max(1);
        let elem_ty = if shared_atomic { "atomic<u32>" } else { "u32" };
        s.push_str(&format!("var<workgroup> shmem: array<{elem_ty}, {words}>;\n"));
    }
    if coop {
        s.push_str("var<workgroup> hl_retired_count: atomic<u32>;\n");
    }
    s.push('\n');

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

    for r in 0..prog.reg_count {
        s.push_str(&format!("    var r{r}: u32 = 0u;\n"));
    }

    if coop {
        s.push_str("    if (lidx == 0u) { atomicStore(&hl_retired_count, 0u); }\n");
        s.push_str("    workgroupBarrier();\n");
        s.push_str("    var pc: i32 = 0;\n");
        s.push_str("    var hl_retired: bool = false;\n");
        s.push_str("    loop {\n");
        s.push_str("        if (!hl_retired) {\n");
        s.push_str("            switch pc {\n");
    } else {
        s.push_str("    var pc: i32 = 0;\n");
        s.push_str("    loop {\n");
        s.push_str("        if (pc < 0) { break; }\n");
        s.push_str("        switch pc {\n");
    }
    let indent = if coop { "                " } else { "            " };
    let retire = if coop {
        "atomicAdd(&hl_retired_count, 1u); hl_retired = true; pc = -1;"
    } else {
        "pc = -1;"
    };
    for (k, inst) in prog.insts.iter().enumerate() {
        let mut body = String::new();
        let mut branched = false;
        match inst {
            Inst::Ret => {
                body.push_str(retire);
                branched = true;
            }
            Inst::Nop => {}
            Inst::Bar => {}
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
                    _ => format!("bitcast<u32>({} >> {sh})", op_i32(a)),
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
                        let cond =
                            if *neg { format!("r{p} == 0u") } else { format!("r{p} != 0u") };
                        body.push_str(&format!(
                            "if ({cond}) {{ pc = {target}; }} else {{ pc = {}; }}",
                            k + 1
                        ));
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
    s.push_str(&format!("{indent}default: {{ {retire} }}\n"));
    if coop {
        s.push_str("            }\n");
        s.push_str("        }\n");
        s.push_str("        workgroupBarrier();\n");
        s.push_str(&format!(
            "        if (atomicLoad(&hl_retired_count) == {total_threads}u) {{ break; }}\n"
        ));
        s.push_str("    }\n}\n");
    } else {
        s.push_str("        }\n    }\n}\n");
    }
    Ok(s)
}

/// Emit a WGSL atomic read-modify-write on `ptr` (an `atomic<u32>`), optionally capturing the old value.
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
        None => body.push_str(&format!("{expr};")),
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
