use super::*;

/// Classify a `%`-prefixed OPERAND token:
/// - `Ok(Some(sr))` — a recognized special register (resolve it via [`SRegAlloc`]);
/// - `Ok(None)`     — a plain virtual register (intern it normally);
/// - `Err(..)`      — a special-register-SHAPED token we do not recognize. A namespaced `%ns.field`
///   token (`%tid.w`, `%bogus.x`) or a known dotless special register we do not model (`%laneid`,
///   `%warpid`, …) is REJECTED rather than silently interned as a fresh zero register — that silent path
///   is the wrong-result footgun this guards against. A plain virtual register never contains a `.`.
impl Ptx {
    pub(super) fn classify_reg_operand(tok: &str) -> Result<Option<u8>> {
        if let Some(sr) = Self::parse_sreg(tok) {
            return Ok(Some(sr));
        }
        let name = Self::strip_reg(tok);
        let base = name.split('.').next().unwrap_or(name);
        if name.contains('.') || Self::is_special_reg_base(base) {
            return Err(Self::error(format!(
                "unknown/unsupported special register `%{name}` used as operand \
             (modeled: %tid/%ntid/%ctaid/%nctaid with a .x/.y/.z component)"
            )));
        }
        Ok(None)
    }

    /// Dotless special-register names PTX defines that we do NOT model. Recognized here ONLY so that an
    /// operand use errors honestly instead of silently interning a fresh zero register. The dimension/index
    /// roots (`tid`/`ntid`/`ctaid`/`nctaid`) are deliberately NOT listed: they are only ever special
    /// registers WITH a `.x/.y/.z` component (handled by the dot rule in [`classify_reg_operand`]), and a
    /// bare `%tid`/`%ntid`/… is a perfectly ordinary virtual-register name real kernels use.
    pub(super) fn is_special_reg_base(base: &str) -> bool {
        matches!(
            base,
            "laneid"
                | "warpid"
                | "nwarpid"
                | "warpsize"
                | "gridid"
                | "smid"
                | "nsmid"
                | "lanemask_eq"
                | "lanemask_le"
                | "lanemask_lt"
                | "lanemask_ge"
                | "lanemask_gt"
                | "clock"
                | "clock64"
                | "clock_hi"
                | "globaltimer"
        )
    }
}

/// Resolve a `%`-register operand token to a register index: a recognized special register routes to its
/// [`SRegAlloc`]-assigned register (materialized by the prelude); a plain virtual register is interned;
/// an unrecognized special-register-shaped token errors (via [`classify_reg_operand`]).
pub(super) fn resolve_reg(
    tok: &str,
    interner: &mut Interner,
    sregs: &mut SRegAlloc,
) -> Result<u16> {
    match Ptx::classify_reg_operand(tok)? {
        Some(sr) => Ok(sregs.reg_for(sr, interner)),
        None => Ok(interner.get(Ptx::strip_reg(tok))),
    }
}

/// Strip PTX comments (`//` line, `/* … */` block), replacing each with a space to keep token bounds.
impl Ptx {
    pub(super) fn strip_comments(src: &str) -> String {
        let b = src.as_bytes();
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                out.push(' ');
            } else {
                out.push(b[i] as char);
                i += 1;
            }
        }
        out
    }
}
