//! Memory access: `ld.param`, `ld`/`st` in `.global` and `.shared`, and `cvta`.

use super::model::{Stmt, Symbols};
use super::operand::{parse_mem, parse_shared_mem};
use super::*;

impl Stmt<'_> {
    pub(super) fn ld(
        &self,
        syms: &Symbols,
        interner: &mut Interner,
        sregs: &mut SRegAlloc,
    ) -> Result<Inst> {
        let d = self.reg(0, interner)?;
        let addr_tok = self.tok(1)?.to_string();
        if self.has("param") {
            let name = Ptx::strip_mem_name(&addr_tok);
            let param = *syms
                .params
                .get(name)
                .ok_or_else(|| self.error(format!("unknown param `{name}`")))?;
            return Ok(Inst::LdParam { d, param });
        }
        if self.has("global") {
            let (addr, off) = parse_mem(&addr_tok, interner, sregs)?;
            return Ok(Inst::LdGlobal {
                d,
                addr,
                off,
                ty: Ptx::gtype(self.opcode)?,
            });
        }
        if self.has("shared") {
            let (base, off) = parse_shared_mem(&addr_tok, syms.shared, interner, sregs)?;
            return Ok(Inst::LdShared {
                d,
                base,
                off,
                ty: Ptx::gtype(self.opcode)?,
            });
        }
        Err(self.error(format!("unsupported state space: `{}`", self.text)))
    }

    pub(super) fn st(
        &self,
        shared: &std::collections::HashMap<String, u32>,
        interner: &mut Interner,
        sregs: &mut SRegAlloc,
    ) -> Result<Inst> {
        let addr_tok = self.tok(0)?.to_string();
        let float = self.is_f32();
        if self.has("global") {
            let (addr, off) = parse_mem(&addr_tok, interner, sregs)?;
            return Ok(Inst::StGlobal {
                addr,
                off,
                src: self.op(1, interner, float, sregs)?,
                ty: Ptx::gtype(self.opcode)?,
            });
        }
        if self.has("shared") {
            let (base, off) = parse_shared_mem(&addr_tok, shared, interner, sregs)?;
            return Ok(Inst::StShared {
                base,
                off,
                src: self.op(1, interner, float, sregs)?,
                ty: Ptx::gtype(self.opcode)?,
            });
        }
        Err(self.error(format!("unsupported state space: `{}`", self.text)))
    }

    pub(super) fn cvta(&self, interner: &mut Interner) -> Result<Inst> {
        Ok(Inst::Cvta {
            d: self.reg(0, interner)?,
            s: self.reg(1, interner)?,
        })
    }
}
