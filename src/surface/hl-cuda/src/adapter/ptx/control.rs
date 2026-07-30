//! Control flow and memory ordering: `bra`, `membar`/`fence`.

use super::instruction::Guard;
use super::model::Stmt;
use super::*;

impl Stmt<'_> {
    pub(super) fn bra(
        &self,
        labels: &std::collections::HashMap<String, u32>,
        pred: Guard,
    ) -> Result<Inst> {
        let tok = self.tok(0)?;
        let target = *labels
            .get(Ptx::strip_label(tok))
            .ok_or_else(|| self.error(format!("unknown branch label `{tok}`")))?;
        Ok(Inst::Bra { target, pred })
    }

    /// `membar.{cta,gl,sys}` and `fence.<order>.{cta,gpu,sys}` → [`Inst::Fence`]. The scope is load-bearing
    /// once an executor runs a block's threads concurrently, so an unrecognized or absent scope is refused
    /// rather than widened or dropped: `fence.sc.cluster` orders a cluster of blocks, which sits between
    /// `CTA` and `DEVICE` and has no `mem_scope` constant.
    pub(super) fn fence(&self) -> Result<Inst> {
        let scope = if self.has("cta") {
            mem_scope::CTA
        } else if self.has("gl") || self.has("gpu") || self.has("device") {
            mem_scope::DEVICE
        } else if self.has("sys") {
            mem_scope::SYSTEM
        } else {
            return Err(self.error(format!(
                "unsupported memory-fence scope (modeled: `cta`, `gl`/`gpu`, `sys`): `{}`",
                self.text
            )));
        };
        Ok(Inst::Fence { scope })
    }
}
