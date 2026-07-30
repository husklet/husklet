use super::*;

/// Lower a compiled [`KernelProgram`] to a WGSL compute shader whose entry point is `prog.entry`. Returns
/// a typed error for any instruction outside the elementwise subset this lowering supports.
pub struct Kernel;

impl Kernel {
    pub fn translate(prog: &KernelProgram) -> Result<String> {
        let analysis = Analysis::new(prog);
        let region = &analysis.regions;
        let region_of = |addr: u16| -> Result<u32> {
            region[addr as usize].ok_or_else(|| {
                Diagnostic::kernel(
                    "wgsl lowering: global access through a value with no static pointer region",
                )
            })
        };
        let atomic = analysis.atomic_regions()?;
        let shared_atomic = prog
            .insts
            .iter()
            .any(|i| matches!(i, Inst::AtomShared { .. }));
        let coop = prog.insts.iter().any(|i| matches!(i, Inst::Bar));

        let shared_idx = |base: &Op, off: i64, elem: u32| {
            format!("(({} + {}u) / {elem}u)", Operand(base).u32(), off as u32)
        };

        let mut s = String::new();
        s.push_str("@group(0) @binding(0) var<storage, read> params: array<u32>;\n");
        for r in 0..prog.num_regions {
            let elem_ty = if atomic[r as usize] {
                "atomic<u32>"
            } else {
                "u32"
            };
            s.push_str(&format!(
                "@group(0) @binding({}) var<storage, read_write> region{r}: array<{elem_ty}>;\n",
                r + 1
            ));
        }
        if prog.shared_bytes > 0 {
            let words = (prog.shared_bytes / 4).max(1);
            let elem_ty = if shared_atomic { "atomic<u32>" } else { "u32" };
            s.push_str(&format!(
                "var<workgroup> shmem: array<{elem_ty}, {words}>;\n"
            ));
        }
        if coop {
            s.push_str("var<workgroup> hl_retired_count: atomic<u32>;\n");
        }
        s.push('\n');

        let [bx, by, bz] = prog.block;
        let total_threads = bx.max(1) * by.max(1) * bz.max(1);
        s.push_str(&format!(
            "@compute @workgroup_size({}, {}, {})\n",
            bx.max(1),
            by.max(1),
            bz.max(1)
        ));
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
        let indent = if coop {
            "                "
        } else {
            "            "
        };
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
                // A fence orders memory without a rendezvous. This executor runs a block's threads
                // concurrently, so the ordering is load-bearing and must be emitted, not discarded.
                Inst::Fence { scope } => body.push_str(match *scope {
                    mem_scope::CTA => "workgroupBarrier();",
                    _ => "storageBarrier();", // DEVICE and SYSTEM
                }),
                Inst::MovImmI { d, imm } => body.push_str(&format!("r{d} = {}u;", *imm as u32)),
                Inst::MovImmF { d, bits } => body.push_str(&format!("r{d} = {bits}u;")),
                Inst::MovReg { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
                Inst::MovSReg { d, sreg } => body.push_str(&format!(
                    "r{d} = {};",
                    SpecialRegister(*sreg).expression(prog.block)
                )),
                Inst::LdParam { d, param } => {
                    let p = &prog.params[*param as usize];
                    if p.is_ptr {
                        body.push_str(&format!("r{d} = 0u;"));
                    } else {
                        body.push_str(&format!("r{d} = params[{}u];", p.offset / 4));
                    }
                }
                Inst::Cvta { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
                Inst::IAdd { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} + {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::ISub { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} - {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::IMul { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} * {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::IMad { d, a, b, c } => body.push_str(&format!(
                    "r{d} = {} * {} + {};",
                    Operand(a).u32(),
                    Operand(b).u32(),
                    Operand(c).u32()
                )),
                Inst::Shift {
                    d,
                    a,
                    b,
                    dir,
                    unsigned,
                } => {
                    let sh = format!("({} & 31u)", Operand(b).u32());
                    let e = match *dir {
                        SHIFT_LEFT => format!("{} << {sh}", Operand(a).u32()),
                        _ if *unsigned => format!("{} >> {sh}", Operand(a).u32()),
                        _ => format!("bitcast<u32>({} >> {sh})", Operand(a).i32()),
                    };
                    body.push_str(&format!("r{d} = {e};"));
                }
                Inst::BitOp { d, a, b, op } => {
                    let sym = match *op {
                        BIT_AND => "&",
                        BIT_OR => "|",
                        _ => "^",
                    };
                    body.push_str(&format!(
                        "r{d} = {} {sym} {};",
                        Operand(a).u32(),
                        Operand(b).u32()
                    ));
                }
                Inst::Setp {
                    d,
                    a,
                    b,
                    cmp,
                    unsigned,
                } => {
                    let sym = match *cmp {
                        CMP_EQ => "==",
                        CMP_NE => "!=",
                        CMP_LT => "<",
                        CMP_LE => "<=",
                        CMP_GT => ">",
                        _ => ">=", // CMP_GE
                    };
                    let cond = if *unsigned {
                        format!("{} {sym} {}", Operand(a).u32(), Operand(b).u32())
                    } else {
                        format!("{} {sym} {}", Operand(a).i32(), Operand(b).i32())
                    };
                    body.push_str(&format!("r{d} = select(0u, 1u, {cond});"));
                }
                Inst::FSetp {
                    d,
                    a,
                    b,
                    cmp,
                    ordered,
                } => {
                    let sym = match *cmp {
                        CMP_EQ => "==",
                        CMP_NE => "!=",
                        CMP_LT => "<",
                        CMP_LE => "<=",
                        CMP_GT => ">",
                        _ => ">=", // CMP_GE
                    };
                    let (x, y) = (Operand(a).f32(), Operand(b).f32());
                    // `isNan` is not in WGSL; `v != v` is the portable NaN test.
                    let unordered = format!("(({x}) != ({x}) || ({y}) != ({y}))");
                    let strict = format!("({x}) {sym} ({y})");
                    let cond = if *ordered {
                        // WGSL float `!=` is TRUE at NaN; the ordered family must be false there. The
                        // other five are already false at NaN.
                        if *cmp == CMP_NE {
                            format!("(!{unordered} && ({strict}))")
                        } else {
                            format!("({strict})")
                        }
                    } else {
                        format!("({unordered} || ({strict}))")
                    };
                    body.push_str(&format!("r{d} = select(0u, 1u, {cond});"));
                }
                Inst::Bra { target, pred } => {
                    match pred {
                        None => body.push_str(&format!("pc = {target};")),
                        Some((p, neg)) => {
                            let cond = if *neg {
                                format!("r{p} == 0u")
                            } else {
                                format!("r{p} != 0u")
                            };
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
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                    if atomic[r as usize] {
                        body.push_str(&format!("r{d} = atomicLoad(&region{r}[{idx}]);"));
                    } else {
                        body.push_str(&format!("r{d} = region{r}[{idx}];"));
                    }
                }
                Inst::StGlobal { addr, off, src, ty } => {
                    let r = region_of(*addr)?;
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                    if atomic[r as usize] {
                        body.push_str(&format!(
                            "atomicStore(&region{r}[{idx}], {});",
                            Operand(src).u32()
                        ));
                    } else {
                        body.push_str(&format!("region{r}[{idx}] = {};", Operand(src).u32()));
                    }
                }
                Inst::LdShared { d, base, off, ty } => {
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = shared_idx(base, *off, elem);
                    if shared_atomic {
                        body.push_str(&format!("r{d} = atomicLoad(&shmem[{idx}]);"));
                    } else {
                        body.push_str(&format!("r{d} = shmem[{idx}];"));
                    }
                }
                Inst::StShared { base, off, src, ty } => {
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = shared_idx(base, *off, elem);
                    if shared_atomic {
                        body.push_str(&format!(
                            "atomicStore(&shmem[{idx}], {});",
                            Operand(src).u32()
                        ));
                    } else {
                        body.push_str(&format!("shmem[{idx}] = {};", Operand(src).u32()));
                    }
                }
                Inst::AtomGlobal {
                    d,
                    addr,
                    off,
                    op,
                    cmp,
                    val,
                    unsigned,
                } => {
                    let r = region_of(*addr)?;
                    let idx = format!("(r{addr} + {}u) / 4u", *off as u32);
                    let ptr = format!("&region{r}[{idx}]");
                    emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
                }
                Inst::AtomShared {
                    d,
                    base,
                    off,
                    op,
                    cmp,
                    val,
                    unsigned,
                } => {
                    if !shared_atomic {
                        return Err(Diagnostic::kernel(
                            "wgsl lowering: atom.shared requires an atomic shared array",
                        ));
                    }
                    let idx = shared_idx(base, *off, 4);
                    let ptr = format!("&shmem[{idx}]");
                    emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
                }
                Inst::FAdd { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} + {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FSub { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} - {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FMul { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} * {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FFma { d, a, b, c } => body.push_str(&format!(
                    "r{d} = bitcast<u32>(fma({}, {}, {}));",
                    Operand(a).f32(),
                    Operand(b).f32(),
                    Operand(c).f32()
                )),
                Inst::Cvt { d, s: src, kind } => {
                    // WGSL `round` is ties-to-even, which is exactly PTX `rni`. Bare `i32()`/`u32()`
                    // truncate toward zero, which is `rzi`. An unknown kind is refused: falling back
                    // to a bit-preserving move is a reinterpret, not a conversion.
                    let e = match *kind {
                        CVT_F32_FROM_S32 => format!("bitcast<u32>(f32({}))", Operand(src).i32()),
                        CVT_F32_FROM_U32 => format!("bitcast<u32>(f32({}))", Operand(src).u32()),
                        CVT_S64_FROM_S32 | CVT_IDENTITY => Operand(src).u32(),
                        CVT_S32_FROM_F32 => format!("bitcast<u32>(i32({}))", Operand(src).f32()),
                        CVT_U32_FROM_F32 => format!("u32({})", Operand(src).f32()),
                        CVT_S32_FROM_F32_RNI => {
                            format!("bitcast<u32>(i32(round({})))", Operand(src).f32())
                        }
                        CVT_U32_FROM_F32_RNI => format!("u32(round({}))", Operand(src).f32()),
                        other => {
                            return Err(Diagnostic::kernel(format!(
                                "wgsl lowering: unknown cvt kind {other}"
                            )))
                        }
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
        ATOM_ADD => dst(body, &format!("atomicAdd({ptr}, {})", Operand(val).u32())),
        ATOM_AND => dst(body, &format!("atomicAnd({ptr}, {})", Operand(val).u32())),
        ATOM_OR => dst(body, &format!("atomicOr({ptr}, {})", Operand(val).u32())),
        ATOM_XOR => dst(body, &format!("atomicXor({ptr}, {})", Operand(val).u32())),
        ATOM_EXCH => dst(
            body,
            &format!("atomicExchange({ptr}, {})", Operand(val).u32()),
        ),
        ATOM_MIN if unsigned => dst(body, &format!("atomicMin({ptr}, {})", Operand(val).u32())),
        ATOM_MAX if unsigned => dst(body, &format!("atomicMax({ptr}, {})", Operand(val).u32())),
        ATOM_MIN | ATOM_MAX => {
            return Err(Diagnostic::kernel(
                "wgsl lowering: signed atomic min/max unsupported (atomic word is u32)",
            ))
        }
        ATOM_CAS => {
            let (c, v) = (Operand(cmp).u32(), Operand(val).u32());
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
        _ => return Err(Diagnostic::kernel("wgsl lowering: unknown atomic op")),
    }
    Ok(())
}
