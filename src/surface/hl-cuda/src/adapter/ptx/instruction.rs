//! Opcode dispatch: the predicate guard, the type-width gate every family shares, and the routing of a
//! [`Stmt`] to the family module that owns its lowering.

use super::model::{Stmt, Symbols};
use super::*;

/// A `@%p` / `@!%p` guard: the predicate register and whether it is negated.
pub(super) type Guard = Option<(u16, bool)>;

impl Ptx {
    pub(super) fn parse_inst(
        stmt: &str,
        syms: &Symbols,
        interner: &mut Interner,
        sregs: &mut SRegAlloc,
    ) -> Result<Inst> {
        // optional predicate guard: `@%p1` or `@!%p1`
        let mut rest = stmt.trim();
        let mut guard: Guard = None;
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
        let stmt = Stmt::new(stmt, opcode, Ptx::split_operands(args_str));
        let base = stmt.base();

        // A guard on anything but `bra` is a PREDICATED instruction. `Inst` carries a predicate only on
        // `Bra`, so the guard cannot be honoured elsewhere; dropping it would execute the instruction
        // unconditionally and return a plausible wrong result instead of the one the kernel asks for.
        if guard.is_some() && base != "bra" {
            return Err(stmt.error(format!(
                "predicated instruction unsupported (only `@p bra` is modeled): `{}`",
                stmt.text
            )));
        }
        Ptx::reject_unmodeled_width(opcode, stmt.text)?;

        match base {
            "ret" => Ok(Inst::Ret),
            "bar" | "barrier" => Ok(Inst::Bar),
            "membar" | "fence" => stmt.fence(),
            "bra" => stmt.bra(syms.labels, guard),
            "mov" => stmt.mov(syms.shared, interner),
            "ld" => stmt.ld(syms, interner, sregs),
            "st" => stmt.st(syms.shared, interner, sregs),
            "cvta" => stmt.cvta(interner),
            "add" | "sub" | "mul" | "mad" | "fma" => stmt.arith(interner, sregs),
            "setp" => stmt.setp(interner, sregs),
            "cvt" => stmt.cvt(interner, sregs),
            "shl" | "shr" => stmt.shift(interner, sregs),
            "and" | "or" | "xor" => stmt.bitop(interner, sregs),
            "atom" | "red" => stmt.atom(syms.shared, interner, sregs),
            other => Err(Ptx::error(format!(
                "unsupported opcode `{other}`: `{}`",
                stmt.text
            ))),
        }
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
}
