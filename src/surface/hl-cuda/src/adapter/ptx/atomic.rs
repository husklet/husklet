//! `atom` / `red` — the atomic read-modify-write forms the IR performs, all on 32-bit words.

use super::model::Stmt;
use super::operand::{parse_mem, parse_shared_mem};
use super::*;

impl Stmt<'_> {
    pub(super) fn atom(
        &self,
        shared: &std::collections::HashMap<String, u32>,
        interner: &mut Interner,
        sregs: &mut SRegAlloc,
    ) -> Result<Inst> {
        let op = self.atom_op()?;
        let unsigned = self.is_unsigned() || self.has("b32") || self.has("b64");
        if self.is_f32() || self.has("f64") {
            return Err(self.error(format!(
                "floating-point atomics unsupported (WGSL has no f32 atomics): `{}`",
                self.text
            )));
        }
        // `red` has no destination register; `atom` returns the previous value in one.
        let mut i = 0;
        let d = if self.base() == "atom" {
            let r = self.reg(i, interner)?;
            i += 1;
            Some(r)
        } else {
            None
        };
        let addr_tok = self.tok(i)?.to_string();
        i += 1;
        let (cmp, val) = if op == ATOM_CAS {
            (
                self.op(i, interner, false, sregs)?,
                self.op(i + 1, interner, false, sregs)?,
            )
        } else {
            (Op::ImmI(0), self.op(i, interner, false, sregs)?)
        };
        if self.has("shared") {
            let (base, off) = parse_shared_mem(&addr_tok, shared, interner, sregs)?;
            return Ok(Inst::AtomShared {
                d,
                base,
                off,
                op,
                cmp,
                val,
                unsigned,
            });
        }
        let (addr, off) = parse_mem(&addr_tok, interner, sregs)?;
        Ok(Inst::AtomGlobal {
            d,
            addr,
            off,
            op,
            cmp,
            val,
            unsigned,
        })
    }

    fn atom_op(&self) -> Result<u8> {
        if self.has("inc") || self.has("dec") {
            // `atom.inc`/`atom.dec` wrap at the operand value; `ATOM_ADD` does not, so mapping them onto
            // it silently changes the result once the counter reaches the wrap point.
            return Err(self.error("unsupported (wrapping increment is not modeled)"));
        }
        Ok(if self.has("add") {
            ATOM_ADD
        } else if self.has("min") {
            ATOM_MIN
        } else if self.has("max") {
            ATOM_MAX
        } else if self.has("and") {
            ATOM_AND
        } else if self.has("or") {
            ATOM_OR
        } else if self.has("xor") {
            ATOM_XOR
        } else if self.has("exch") {
            ATOM_EXCH
        } else if self.has("cas") {
            ATOM_CAS
        } else {
            return Err(self.error("unsupported atomic operation"));
        })
    }
}
