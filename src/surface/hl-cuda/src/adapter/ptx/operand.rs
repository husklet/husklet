use super::register::resolve_reg;
use super::*;

/// Parse a shared-memory address operand `[base(+off)]` (`base` = register or `.shared` symbol).
pub(super) fn parse_shared_mem(
    tok: &str,
    shared_syms: &std::collections::HashMap<String, u32>,
    interner: &mut Interner,
    sregs: &mut SRegAlloc,
) -> Result<(Op, i64)> {
    let inner = tok
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    let (base_tok, off) = if let Some(pos) = inner.find(['+', '-']) {
        let (b, o) = inner.split_at(pos);
        (b.trim(), Ptx::parse_imm_i(o.trim())?)
    } else {
        (inner, 0)
    };
    if Ptx::is_reg(base_tok) {
        Ok((Op::Reg(resolve_reg(base_tok, interner, sregs)?), off))
    } else if let Some(&sym) = shared_syms.get(base_tok) {
        Ok((Op::ImmI(sym as i64), off))
    } else {
        Ok((Op::ImmI(Ptx::parse_imm_i(base_tok)?), off))
    }
}

// ---- small parser helpers ----

impl Ptx {
    pub(super) fn split_first(s: &str) -> (&str, &str) {
        let s = s.trim_start();
        match s.find(char::is_whitespace) {
            Some(i) => (&s[..i], s[i..].trim_start()),
            None => (s, ""),
        }
    }

    /// Split an operand list on commas (no commas occur inside `[a+b]` for our subset).
    pub(super) fn split_operands(s: &str) -> Vec<String> {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    }

    pub(super) fn is_reg(tok: &str) -> bool {
        tok.starts_with('%')
    }
    pub(super) fn strip_reg(tok: &str) -> &str {
        tok.trim().trim_start_matches('%')
    }
    pub(super) fn strip_label(tok: &str) -> &str {
        tok.trim()
    }
    pub(super) fn strip_mem_name(tok: &str) -> &str {
        tok.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim()
    }

    pub(super) fn parse_sreg(tok: &str) -> Option<u8> {
        Some(match Self::strip_reg(tok) {
            "tid.x" => SR_TID_X,
            "tid.y" => SR_TID_Y,
            "tid.z" => SR_TID_Z,
            "ntid.x" => SR_NTID_X,
            "ntid.y" => SR_NTID_Y,
            "ntid.z" => SR_NTID_Z,
            "ctaid.x" => SR_CTAID_X,
            "ctaid.y" => SR_CTAID_Y,
            "ctaid.z" => SR_CTAID_Z,
            "nctaid.x" => SR_NCTAID_X,
            "nctaid.y" => SR_NCTAID_Y,
            "nctaid.z" => SR_NCTAID_Z,
            _ => return None,
        })
    }

    pub(super) fn gtype(opcode: &str) -> Result<u8> {
        if opcode.contains("f32") {
            Ok(gty::F32)
        } else if opcode.contains("u64") || opcode.contains("s64") || opcode.contains("b64") {
            Ok(gty::U64)
        } else if opcode.contains("u32") || opcode.contains("s32") || opcode.contains("b32") {
            Ok(gty::U32)
        } else {
            Err(Self::error(format!(
                "unsupported global access type: `{opcode}`"
            )))
        }
    }
}

/// `[%reg]`, `[%reg+off]`, or `[%reg-off]` → (reg, byte offset).
pub(super) fn parse_mem(
    tok: &str,
    interner: &mut Interner,
    sregs: &mut SRegAlloc,
) -> Result<(u16, i64)> {
    let inner = tok
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if let Some(pos) = inner.find(['+', '-']) {
        let (r, o) = inner.split_at(pos);
        let off = Ptx::parse_imm_i(o.trim())?;
        Ok((resolve_reg(r.trim(), interner, sregs)?, off))
    } else {
        Ok((resolve_reg(inner, interner, sregs)?, 0))
    }
}

pub(super) fn parse_op(
    tok: &str,
    interner: &mut Interner,
    want_float: bool,
    sregs: &mut SRegAlloc,
) -> Result<Op> {
    let t = tok.trim();
    if Ptx::is_reg(t) {
        Ok(Op::Reg(resolve_reg(t, interner, sregs)?))
    } else if want_float {
        Ok(Op::ImmF(Ptx::parse_imm_f(t)?))
    } else {
        Ok(Op::ImmI(Ptx::parse_imm_i(t)?))
    }
}

impl Ptx {
    pub(super) fn parse_imm_i(t: &str) -> Result<i64> {
        let t = t.trim();
        if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            i64::from_str_radix(h, 16).map_err(|_| Self::error(format!("bad hex immediate `{t}`")))
        } else {
            t.parse::<i64>()
                .map_err(|_| Self::error(format!("bad integer immediate `{t}`")))
        }
    }

    /// PTX float immediates are `0f<hex bits>` (single) — the exact 32-bit encoding. Also accept decimals.
    pub(super) fn parse_imm_f(t: &str) -> Result<u32> {
        let t = t.trim();
        if let Some(h) = t.strip_prefix("0f").or_else(|| t.strip_prefix("0F")) {
            u32::from_str_radix(h, 16)
                .map_err(|_| Self::error(format!("bad float immediate `{t}`")))
        } else {
            t.parse::<f32>()
                .map(|f| f.to_bits())
                .map_err(|_| Self::error(format!("bad float immediate `{t}`")))
        }
    }
}
