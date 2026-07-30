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

        // A guard on anything but `bra` is a PREDICATED instruction. `Inst` carries a predicate only on
        // `Bra`, so the guard cannot be honoured elsewhere; dropping it would execute the instruction
        // unconditionally and return a plausible wrong result instead of the one the kernel asks for.
        if guard.is_some() && base != "bra" {
            return Err(Ptx::error(format!(
                "predicated `{opcode}` unsupported (only `@p bra` is modeled): `{stmt}`"
            )));
        }
        Ptx::reject_unmodeled_width(opcode, stmt)?;

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
                if !has("f32") && (has("hi") || ((has("s64") || has("u64")) && !has("wide"))) {
                    // `Inst::IMul` computes either the low 32 bits or the 32×32→64 `wide` product; a
                    // `.hi` or 64×64 multiply has no faithful form and must not fall through to `.lo`.
                    return Err(Ptx::error(format!(
                        "`{opcode}` unsupported (only 32-bit `.lo` and `mul.wide.[su]32` are modeled): `{stmt}`"
                    )));
                }
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
                if has("hi") || has("wide") || has("s64") || has("u64") {
                    // `Inst::IMad` is the 32-bit `.lo` form only; `.hi`/`.wide`/64-bit would silently
                    // report the low 32 bits of the wrong product.
                    return Err(Ptx::error(format!(
                        "`{opcode}` unsupported (only `mad.lo.[su]32` is modeled): `{stmt}`"
                    )));
                }
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
                if has("f32") {
                    // `Inst::Setp` compares 32-bit INTEGERS. An IEEE-754 f32 compare only coincides with
                    // a signed-integer compare of the bit patterns while both operands are non-negative;
                    // for a negative operand the ordering inverts. Rejecting is the only honest answer
                    // until the IR gains a float compare.
                    return Err(Ptx::error(format!(
                        "floating-point `{opcode}` unsupported (the IR compares integers): `{stmt}`"
                    )));
                }
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
                // Only the `.<dst>.<src>` type tokens matter; rounding/saturation modifiers (`rn`, `rzi`,
                // `sat`, …) are filtered out by name, not by first letter — `sat` would otherwise be read
                // as the destination type.
                let tys: Vec<&str> = opcode
                    .split('.')
                    .skip(1)
                    .filter(|m| Ptx::is_scalar_type(m))
                    .collect();
                let kind = Ptx::cvt_kind(tys.first().copied(), tys.get(1).copied(), stmt)?;
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

/// Opcode type-suffix rules: which widths the 32-bit kernel IR can represent, and which `cvt` type pairs
/// it actually performs.
impl Ptx {
    /// Is `token` a PTX scalar type suffix (as opposed to a rounding/saturation/space modifier)?
    fn is_scalar_type(token: &str) -> bool {
        matches!(
            token,
            "s8" | "s16"
                | "s32"
                | "s64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "b8"
                | "b16"
                | "b32"
                | "b64"
                | "f16"
                | "f32"
                | "f64"
                | "bf16"
                | "pred"
        )
    }

    /// Reject an opcode whose type suffix names a width the kernel IR cannot represent. The modeled ALU is
    /// 32-bit integer plus f32 (`Inst` has no f64/f16 form), so a wider or narrower float operation has no
    /// faithful lowering — falling through to the 32-bit form would compute on the wrong bits and hand the
    /// application a plausible wrong number.
    fn reject_unmodeled_width(opcode: &str, stmt: &str) -> Result<()> {
        const UNMODELED: &[&str] = &[
            "f64", "f16", "f16x2", "bf16", "bf16x2", "f32x2", "e4m3", "e5m2", "tf32",
        ];
        match opcode.split('.').find(|t| UNMODELED.contains(t)) {
            Some(ty) => Err(Self::error(format!(
                "`.{ty}` is outside the modeled 32-bit (s32/u32/f32) subset: `{stmt}`"
            ))),
            None => Ok(()),
        }
    }

    /// Map a `cvt.<dst>.<src>` type pair onto the `CVT_*` kind the executor performs. Only conversions the
    /// IR really carries out are accepted; an unmodeled pair (`cvt.rn.f32.u32`, `cvt.rzi.u32.f32`,
    /// `cvt.s32.s8`, …) is a typed error instead of `CVT_IDENTITY`, which would reinterpret the bits and
    /// return a wrong number. `CVT_IDENTITY` stays reserved for a same-width integer reinterpretation,
    /// where a bit-preserving move IS the conversion.
    fn cvt_kind(dst: Option<&str>, src: Option<&str>, stmt: &str) -> Result<u8> {
        let unsupported = || {
            Self::error(format!(
                "unsupported `cvt` conversion (modeled: f32<->s32, s64<-s32, same-width integer): `{stmt}`"
            ))
        };
        let (dst, src) = (dst.ok_or_else(unsupported)?, src.ok_or_else(unsupported)?);
        let width = |t: &str| match t {
            "s8" | "u8" | "b8" => 8u32,
            "s16" | "u16" | "b16" => 16,
            "s32" | "u32" | "b32" | "f32" => 32,
            "s64" | "u64" | "b64" => 64,
            _ => 0,
        };
        Ok(match (dst, src) {
            ("f32", "s32") => CVT_F32_FROM_S32,
            ("s64", "s32") => CVT_S64_FROM_S32,
            ("s32", "f32") => CVT_S32_FROM_F32,
            (d, s) if d != "f32" && s != "f32" && width(d) == width(s) && width(d) != 0 => {
                CVT_IDENTITY
            }
            _ => return Err(unsupported()),
        })
    }
}

/// Map an `atom.*`/`red.*` opcode to an `ATOM_*` code.
impl Ptx {
    fn atom_op(opcode: &str) -> Result<u8> {
        let m = |x: &str| opcode.split('.').any(|t| t == x);
        if m("inc") || m("dec") {
            // `atom.inc`/`atom.dec` wrap at the operand value; `ATOM_ADD` does not, so mapping them onto
            // it silently changes the result once the counter reaches the wrap point.
            return Err(Self::error(format!(
                "`{opcode}` unsupported (wrapping increment is not modeled)"
            )));
        }
        Ok(if m("add") {
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
