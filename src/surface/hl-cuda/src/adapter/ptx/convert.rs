//! `cvt` — the `<dst>.<src>` type pair plus the rounding modifier PTX spells into the opcode, mapped onto
//! the `CVT_*` kind the executor performs.

use super::model::Stmt;
use super::*;

/// The integer rounding a `cvt` applies to a float→int conversion. PTX names it in the opcode and the two
/// modes disagree on every fractional value, so they are separate `CVT_*` kinds.
enum Rounding {
    /// `.rni` — to nearest, ties to even (2.5 → 2, 3.5 → 4).
    NearestEven,
    /// `.rzi` — toward zero (2.5 → 2, 3.5 → 3).
    Zero,
    /// `.rn` — the float rounding an int→float conversion uses.
    Nearest,
    /// `.rmi`/`.rpi` (floor/ceil), `.rz`/`.rm`/`.rp`, or none.
    Other,
}

impl Rounding {
    fn of(stmt: &Stmt) -> Self {
        if stmt.has("rni") {
            Self::NearestEven
        } else if stmt.has("rzi") {
            Self::Zero
        } else if stmt.has("rn") {
            Self::Nearest
        } else {
            Self::Other
        }
    }
}

impl Stmt<'_> {
    pub(super) fn cvt(&self, interner: &mut Interner, sregs: &mut SRegAlloc) -> Result<Inst> {
        let d = self.reg(0, interner)?;
        let s = self.op(1, interner, self.is_f32(), sregs)?;
        // Only the `.<dst>.<src>` type tokens name the conversion; rounding/saturation modifiers (`rn`,
        // `rzi`, `sat`, …) are filtered out by name, not by first letter — `sat` would otherwise be read
        // as the destination type.
        let tys: Vec<&str> = self
            .opcode
            .split('.')
            .skip(1)
            .filter(|m| Ptx::is_scalar_type(m))
            .collect();
        let kind = self.cvt_kind(tys.first().copied(), tys.get(1).copied())?;
        Ok(Inst::Cvt { d, s, kind })
    }

    /// Map a `cvt.<rounding>.<dst>.<src>` onto its `CVT_*` kind. A conversion the IR does not carry out is a
    /// typed error rather than `CVT_IDENTITY`, which would reinterpret the bits and return a wrong number;
    /// `CVT_IDENTITY` stays reserved for a same-width integer reinterpretation, where a bit-preserving move
    /// IS the conversion.
    fn cvt_kind(&self, dst: Option<&str>, src: Option<&str>) -> Result<u8> {
        let unsupported = || {
            self.error(format!(
                "unsupported conversion (modeled: f32<->s32/u32 with `.rn`/`.rni`/`.rzi`, `s64<-s32`, \
                 same-width integer): `{}`",
                self.text
            ))
        };
        let (dst, src) = (dst.ok_or_else(unsupported)?, src.ok_or_else(unsupported)?);
        let float_to_int = src == "f32" && dst != "f32";
        // `cvt.sat` clamps to the destination range. For a float→int conversion that clamping is already
        // the defined behaviour, so the modifier is redundant; on any other pair (`cvt.sat.f32.f32` clamps
        // to [0,1]) it changes the result and has no modeled form.
        if self.has("sat") && !float_to_int {
            return Err(unsupported());
        }
        // `.ftz` flushes a denormal input to zero, which the executor's conversions do not do.
        if self.has("ftz") {
            return Err(unsupported());
        }
        let rounding = Rounding::of(self);
        let width = |t: &str| match t {
            "s8" | "u8" | "b8" => 8u32,
            "s16" | "u16" | "b16" => 16,
            "s32" | "u32" | "b32" | "f32" => 32,
            "s64" | "u64" | "b64" => 64,
            _ => 0,
        };
        Ok(match (dst, src, rounding) {
            ("f32", "s32", Rounding::Nearest) => CVT_F32_FROM_S32,
            ("f32", "u32", Rounding::Nearest) => CVT_F32_FROM_U32,
            ("s32", "f32", Rounding::Zero) => CVT_S32_FROM_F32,
            ("u32", "f32", Rounding::Zero) => CVT_U32_FROM_F32,
            ("s32", "f32", Rounding::NearestEven) => CVT_S32_FROM_F32_RNI,
            ("u32", "f32", Rounding::NearestEven) => CVT_U32_FROM_F32_RNI,
            ("s64", "s32", _) => CVT_S64_FROM_S32,
            (d, s, _) if d != "f32" && s != "f32" && width(d) == width(s) && width(d) != 0 => {
                CVT_IDENTITY
            }
            _ => return Err(unsupported()),
        })
    }
}

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
}
