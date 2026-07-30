//! The modeled ALU: `mov`, `add`/`sub`/`mul`/`mad`/`fma`, `shl`/`shr`, `and`/`or`/`xor`.

use super::model::Stmt;
use super::*;

impl Stmt<'_> {
    pub(super) fn mov(
        &self,
        shared: &std::collections::HashMap<String, u32>,
        interner: &mut Interner,
    ) -> Result<Inst> {
        let d = self.reg(0, interner)?;
        let src = self.tok(1)?.to_string();
        if Ptx::is_reg(&src) {
            // A `%`-source is a special register (→ MovSReg) or a plain register (→ MovReg); an
            // unrecognized special-register-shaped token errors instead of silently reading a zero reg.
            return Ok(match Ptx::classify_reg_operand(&src)? {
                Some(sreg) => Inst::MovSReg { d, sreg },
                None => Inst::MovReg {
                    d,
                    s: interner.get(Ptx::strip_reg(&src)),
                },
            });
        }
        if let Some(&off) = shared.get(src.trim()) {
            return Ok(Inst::MovImmI { d, imm: off as u64 });
        }
        Ok(if self.is_f32() {
            Inst::MovImmF {
                d,
                bits: Ptx::parse_imm_f(&src)?,
            }
        } else {
            Inst::MovImmI {
                d,
                imm: Ptx::parse_imm_i(&src)? as u64,
            }
        })
    }

    pub(super) fn arith(&self, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<Inst> {
        let float = self.is_f32();
        let wide_type = self.has("s64") || self.has("u64") || self.has("b64");
        let d = self.reg(0, interner)?;
        let operand = |index: usize, interner: &mut Interner, sregs: &mut SRegAlloc| {
            self.op(index, interner, float, sregs)
        };
        // `mad.rn.f32` rounds the product before the add, so it is NOT `fma.rn.f32`; the IR has only the
        // fused form. Lowering it onto the integer `mad` (what an unmatched `.f32` used to do) adds the
        // operands' bit patterns.
        if float && self.base() == "mad" {
            return Err(self.error(format!(
                "unsupported (the IR carries the FUSED `fma.rn.f32` only): `{}`",
                self.text
            )));
        }
        match self.base() {
            "add" if float => Ok(Inst::FAdd {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
            }),
            "add" => Ok(Inst::IAdd {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
                wide: wide_type,
            }),
            "sub" if float => Ok(Inst::FSub {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
            }),
            "sub" => Ok(Inst::ISub {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
                wide: wide_type,
            }),
            "mul" if float => Ok(Inst::FMul {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
            }),
            "mul" => {
                // `Inst::IMul` computes either the low 32 bits or the 32×32→64 `wide` product; a `.hi` or
                // 64×64 multiply has no faithful form and must not fall through to `.lo`.
                if self.has("hi") || (wide_type && !self.has("wide")) {
                    return Err(self.error(format!(
                        "unsupported (only 32-bit `.lo` and `mul.wide.[su]32` are modeled): `{}`",
                        self.text
                    )));
                }
                Ok(Inst::IMul {
                    d,
                    a: operand(1, interner, sregs)?,
                    b: operand(2, interner, sregs)?,
                    wide: self.has("wide"),
                    unsigned: self.has("u32") || self.has("u16") || self.has("u8"),
                })
            }
            "fma" => Ok(Inst::FFma {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
                c: operand(3, interner, sregs)?,
            }),
            // `Inst::IMad` is the 32-bit `.lo` form only; `.hi`/`.wide`/64-bit would silently report the
            // low 32 bits of the wrong product.
            _ if self.has("hi") || self.has("wide") || wide_type => Err(self.error(format!(
                "unsupported (only `mad.lo.[su]32` is modeled): `{}`",
                self.text
            ))),
            _ => Ok(Inst::IMad {
                d,
                a: operand(1, interner, sregs)?,
                b: operand(2, interner, sregs)?,
                c: operand(3, interner, sregs)?,
            }),
        }
    }

    pub(super) fn shift(&self, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<Inst> {
        Ok(Inst::Shift {
            d: self.reg(0, interner)?,
            a: self.op(1, interner, false, sregs)?,
            b: self.op(2, interner, false, sregs)?,
            dir: if self.base() == "shl" {
                SHIFT_LEFT
            } else {
                SHIFT_RIGHT
            },
            unsigned: self.is_unsigned() || self.has("b32") || self.has("b64") || self.has("b16"),
        })
    }

    pub(super) fn bitop(&self, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<Inst> {
        Ok(Inst::BitOp {
            d: self.reg(0, interner)?,
            a: self.op(1, interner, false, sregs)?,
            b: self.op(2, interner, false, sregs)?,
            op: match self.base() {
                "and" => BIT_AND,
                "or" => BIT_OR,
                _ => BIT_XOR,
            },
        })
    }
}
