use super::operand::{parse_mem, parse_op, parse_shared_mem};
use super::*;

impl Ptx {
    pub(super) fn parse_inst(
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
            let (pred_tok, tail) = Ptx::split_first(after);
            let (neg, pname) = if let Some(p) = pred_tok.strip_prefix('!') {
                (true, p)
            } else {
                (false, pred_tok)
            };
            guard = Some((interner.get(Ptx::strip_reg(pname)), neg));
            rest = tail.trim();
        }

        let (opcode, args_str) = Ptx::split_first(rest);
        let ops: Vec<String> = Ptx::split_operands(args_str);

        let base = opcode.split('.').next().unwrap_or(opcode);
        let has = |m: &str| opcode.split('.').any(|x| x == m);

        let reg = |i: &mut Interner, tok: &str| -> u16 { i.get(Ptx::strip_reg(tok)) };

        macro_rules! need {
            ($n:expr) => {
                if ops.len() < $n {
                    return Err(Ptx::error(format!(
                        "`{opcode}` expects {} operands: `{stmt}`",
                        $n
                    )));
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
                    .get(Ptx::strip_label(&ops[0]))
                    .ok_or_else(|| Ptx::error(format!("unknown branch label `{}`", ops[0])))?;
                Inst::Bra {
                    target,
                    pred: guard,
                }
            }
            "mov" => {
                need!(2);
                let d = reg(interner, &ops[0]);
                let src = &ops[1];
                if Ptx::is_reg(src) {
                    // A `%`-source is a special register (→ MovSReg) or a plain register (→ MovReg); an
                    // unrecognized special-register-shaped token errors instead of silently reading a zero reg.
                    match Ptx::classify_reg_operand(src)? {
                        Some(sr) => Inst::MovSReg { d, sreg: sr },
                        None => Inst::MovReg {
                            d,
                            s: reg(interner, src),
                        },
                    }
                } else if let Some(&off) = shared_syms.get(src.trim()) {
                    Inst::MovImmI { d, imm: off as u64 }
                } else if has("f32") {
                    Inst::MovImmF {
                        d,
                        bits: Ptx::parse_imm_f(src)?,
                    }
                } else {
                    Inst::MovImmI {
                        d,
                        imm: Ptx::parse_imm_i(src)? as u64,
                    }
                }
            }
            "ld" => {
                need!(2);
                let d = reg(interner, &ops[0]);
                if has("param") {
                    let name = Ptx::strip_mem_name(&ops[1]);
                    let p = *params
                        .get(name)
                        .ok_or_else(|| Ptx::error(format!("unknown param `{name}`")))?;
                    Inst::LdParam { d, param: p }
                } else if has("global") {
                    let (addr, off) = parse_mem(&ops[1], interner, sregs)?;
                    Inst::LdGlobal {
                        d,
                        addr,
                        off,
                        ty: Ptx::gtype(opcode)?,
                    }
                } else if has("shared") {
                    let (base, off) = parse_shared_mem(&ops[1], shared_syms, interner, sregs)?;
                    Inst::LdShared {
                        d,
                        base,
                        off,
                        ty: Ptx::gtype(opcode)?,
                    }
                } else {
                    return Err(Ptx::error(format!("unsupported ld space: `{stmt}`")));
                }
            }
            "st" => {
                need!(2);
                if has("global") {
                    let (addr, off) = parse_mem(&ops[0], interner, sregs)?;
                    let src = parse_op(&ops[1], interner, has("f32"), sregs)?;
                    Inst::StGlobal {
                        addr,
                        off,
                        src,
                        ty: Ptx::gtype(opcode)?,
                    }
                } else if has("shared") {
                    let (base, off) = parse_shared_mem(&ops[0], shared_syms, interner, sregs)?;
                    let src = parse_op(&ops[1], interner, has("f32"), sregs)?;
                    Inst::StShared {
                        base,
                        off,
                        src,
                        ty: Ptx::gtype(opcode)?,
                    }
                } else {
                    return Err(Ptx::error(format!("unsupported st space: `{stmt}`")));
                }
            }
            "cvta" => {
                need!(2);
                Inst::Cvta {
                    d: reg(interner, &ops[0]),
                    s: reg(interner, &ops[1]),
                }
            }
            "add" => {
                need!(3);
                let d = reg(interner, &ops[0]);
                if has("f32") {
                    Inst::FAdd {
                        d,
                        a: parse_op(&ops[1], interner, true, sregs)?,
                        b: parse_op(&ops[2], interner, true, sregs)?,
                    }
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
                    Inst::FSub {
                        d,
                        a: parse_op(&ops[1], interner, true, sregs)?,
                        b: parse_op(&ops[2], interner, true, sregs)?,
                    }
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
                    Inst::FMul {
                        d,
                        a: parse_op(&ops[1], interner, true, sregs)?,
                        b: parse_op(&ops[2], interner, true, sregs)?,
                    }
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
                    return Err(Ptx::error(format!("unsupported setp cmp: `{stmt}`")));
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
                    (Some(&d), Some(&s)) if d.starts_with('f') && s.starts_with('s') => {
                        CVT_F32_FROM_S32
                    }
                    (Some(&d), Some(&s)) if d == "s64" && s.starts_with('s') => CVT_S64_FROM_S32,
                    (Some(&d), Some(&s)) if d.starts_with('s') && s.starts_with('f') => {
                        CVT_S32_FROM_F32
                    }
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
                    dir: if base == "shl" {
                        SHIFT_LEFT
                    } else {
                        SHIFT_RIGHT
                    },
                    unsigned: has("u32")
                        || has("u64")
                        || has("u16")
                        || has("b32")
                        || has("b64")
                        || has("b16"),
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
                let op = Ptx::atom_op(opcode)?;
                let unsigned = has("u32") || has("u64") || has("u16") || has("b32") || has("b64");
                if opcode.contains("f32") || opcode.contains("f64") {
                    return Err(Ptx::error(format!(
                        "floating-point atomics unsupported (WGSL has no f32 atomics): `{stmt}`"
                    )));
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
                    Inst::AtomShared {
                        d,
                        base: b,
                        off,
                        op,
                        cmp,
                        val,
                        unsigned,
                    }
                } else if has("global") || !has("shared") {
                    let (addr, off) = parse_mem(addr_tok, interner, sregs)?;
                    Inst::AtomGlobal {
                        d,
                        addr,
                        off,
                        op,
                        cmp,
                        val,
                        unsigned,
                    }
                } else {
                    return Err(Ptx::error(format!("unsupported atom space: `{stmt}`")));
                }
            }
            other => {
                return Err(Ptx::error(format!(
                    "unsupported opcode `{other}`: `{stmt}`"
                )))
            }
        };
        Ok(inst)
    }
}

/// Map an `atom.*`/`red.*` opcode to an `ATOM_*` code.
impl Ptx {
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
            return Err(Self::error(format!("unsupported atomic op: `{opcode}`")));
        })
    }
}
