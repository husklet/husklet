//! `setp` — the integer compare ([`Inst::Setp`]) and the IEEE-754 f32 compare ([`Inst::FSetp`]).

use super::model::Stmt;
use super::*;

/// A PTX `setp` comparison operator. The PTX ISA defines three families for floating point and they differ
/// exactly at NaN:
/// * `Ordered` — `eq`/`ne`/`lt`/`le`/`gt`/`ge`: FALSE if either operand is NaN.
/// * `Unordered` — `equ`/`neu`/`ltu`/`leu`/`gtu`/`geu`: TRUE if either operand is NaN. A compiler emits
///   this family for a negated source comparison such as `!(x < y)`.
/// * `Order` — `num`/`nan`: not a comparison at all but a test of whether the operands are numbers.
enum Family {
    Ordered(u8),
    Unordered(u8),
    Order,
}

impl Family {
    fn of(stmt: &Stmt) -> Option<Self> {
        const CMPS: [(&str, u8); 6] = [
            ("eq", CMP_EQ),
            ("ne", CMP_NE),
            ("lt", CMP_LT),
            ("le", CMP_LE),
            ("gt", CMP_GT),
            ("ge", CMP_GE),
        ];
        if stmt.has("num") || stmt.has("nan") {
            return Some(Self::Order);
        }
        for (name, cmp) in CMPS {
            if stmt.has(name) {
                return Some(Self::Ordered(cmp));
            }
            if stmt.has(&format!("{name}u")) {
                return Some(Self::Unordered(cmp));
            }
        }
        None
    }
}

impl Stmt<'_> {
    pub(super) fn setp(&self, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<Inst> {
        // `setp.<cmp>.<op>.<type> p|q, a, b, c` fuses a second predicate operand into the result. `Inst`
        // has no such form, so dropping the `.and`/`.or`/`.xor` term would report the bare comparison.
        if self.has("and") || self.has("or") || self.has("xor") || self.tok(0)?.contains('|') {
            return Err(self.error(format!(
                "fused predicate operand unsupported (only the plain 3-operand `setp` is modeled): `{}`",
                self.text
            )));
        }
        let family = Family::of(self)
            .ok_or_else(|| self.error(format!("unsupported comparison: `{}`", self.text)))?;
        let d = self.reg(0, interner)?;
        let float = self.is_f32();
        let a = self.op(1, interner, float, sregs)?;
        let b = self.op(2, interner, float, sregs)?;
        match (float, family) {
            (_, Family::Order) => Err(self.error(format!(
                "`setp.num`/`setp.nan` unsupported: they test whether the operands are numbers rather \
                 than comparing them, and the IR's f32 compare carries only a comparison: `{}`",
                self.text
            ))),
            (true, Family::Ordered(cmp)) => Ok(Inst::FSetp {
                d,
                a,
                b,
                cmp,
                ordered: true,
            }),
            (true, Family::Unordered(cmp)) => Ok(Inst::FSetp {
                d,
                a,
                b,
                cmp,
                ordered: false,
            }),
            (false, Family::Ordered(cmp)) => Ok(Inst::Setp {
                d,
                a,
                b,
                cmp,
                unsigned: self.is_unsigned(),
            }),
            // The unordered family exists only for floating point; on an integer type it is not PTX.
            (false, Family::Unordered(_)) => Err(self.error(format!(
                "the unordered comparison family applies to floating point only: `{}`",
                self.text
            ))),
        }
    }
}
