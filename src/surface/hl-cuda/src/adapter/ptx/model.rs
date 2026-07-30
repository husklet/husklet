use super::operand::parse_op;
use super::*;

/// The module-scope names a statement can refer to: kernel parameters, branch labels, and `.shared`
/// variables with their resolved byte offsets.
pub(super) struct Symbols<'a> {
    pub(super) params: &'a std::collections::HashMap<&'a str, u16>,
    pub(super) labels: &'a std::collections::HashMap<String, u32>,
    pub(super) shared: &'a std::collections::HashMap<String, u32>,
}

/// One PTX instruction statement, split into the opcode (with every `.` modifier) and its comma-separated
/// operands. Each instruction family lowers from this; the family lowerings live in sibling modules.
pub(super) struct Stmt<'a> {
    pub(super) text: &'a str,
    pub(super) opcode: &'a str,
    pub(super) ops: Vec<String>,
}

impl<'a> Stmt<'a> {
    pub(super) fn new(text: &'a str, opcode: &'a str, ops: Vec<String>) -> Self {
        Self { text, opcode, ops }
    }

    /// The opcode without its modifiers (`mad.lo.s32` → `mad`).
    pub(super) fn base(&self) -> &'a str {
        self.opcode.split('.').next().unwrap_or(self.opcode)
    }

    /// Does the opcode carry `modifier` as a whole `.`-separated token? Token equality matters: `lt` must
    /// not match the distinct PTX comparison `ltu`, nor `sat` the destination type.
    pub(super) fn has(&self, modifier: &str) -> bool {
        self.opcode.split('.').any(|t| t == modifier)
    }

    /// Is this a `.f32`-typed instruction (so an immediate operand is a float encoding)?
    pub(super) fn is_f32(&self) -> bool {
        self.has("f32")
    }

    /// Is the type suffix an unsigned integer width, i.e. does the operation zero-extend?
    pub(super) fn is_unsigned(&self) -> bool {
        ["u8", "u16", "u32", "u64"].iter().any(|t| self.has(t))
    }

    pub(super) fn tok(&self, index: usize) -> Result<&str> {
        self.ops.get(index).map(String::as_str).ok_or_else(|| {
            self.error(format!(
                "expects at least {} operands: `{}`",
                index + 1,
                self.text
            ))
        })
    }

    pub(super) fn reg(&self, index: usize, interner: &mut Interner) -> Result<u16> {
        Ok(interner.get(Ptx::strip_reg(self.tok(index)?)))
    }

    pub(super) fn op(
        &self,
        index: usize,
        interner: &mut Interner,
        float: bool,
        sregs: &mut SRegAlloc,
    ) -> Result<Op> {
        let tok = self.tok(index)?.to_string();
        parse_op(&tok, interner, float, sregs)
    }

    pub(super) fn error(&self, message: impl std::fmt::Display) -> GpuError {
        Ptx::error(format!("`{}`: {message}", self.opcode))
    }
}

#[derive(Default)]
pub(super) struct Interner {
    map: std::collections::HashMap<String, u16>,
    /// Set once the kernel names more than `u16::MAX` distinct registers (see [`Interner::get`]).
    overflowed: bool,
}
impl Interner {
    /// Register indices are `u16` in the IR. A kernel that names more than `u16::MAX` distinct registers
    /// cannot be represented: truncating `len()` would alias two registers onto one index (wrong results)
    /// and make `count()` smaller than an already-issued index (an out-of-range register-table access).
    /// `overflowed` records that so [`Ptx::parse_body`] can fail loudly instead.
    pub(super) fn get(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.map.get(name) {
            return i;
        }
        if self.map.len() >= u16::MAX as usize {
            self.overflowed = true;
            return 0;
        }
        let i = self.map.len() as u16;
        self.map.insert(name.to_string(), i);
        i
    }
    pub(super) fn count(&self) -> u16 {
        self.map.len() as u16
    }
    pub(super) fn is_overflowed(&self) -> bool {
        self.overflowed
    }
}

/// Allocates a real register for each distinct special register that appears in an OPERAND position
/// (e.g. `mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x` — legal PTX). A `mov %r, %sreg` lowers straight to
/// [`Inst::MovSReg`]; an operand-position special register instead needs a register to read from, so its
/// value is materialized once by a [`Inst::MovSReg`] PRELUDE emitted at the very top of the instruction
/// stream (special registers are thread/block-invariant, so a single leading read is always correct) and
/// operand references then read that register. This routes operand-position special registers through the
/// EXACT same runtime resolution as the `mov` form — instead of the old footgun of silently interning
/// `%ntid.x` as a fresh, zero-valued virtual register (→ every thread computing index 0).
#[derive(Default)]
pub(super) struct SRegAlloc {
    /// `(sreg code, assigned register)` in first-use order — also the prelude emission order.
    regs: Vec<(u8, u16)>,
}
impl SRegAlloc {
    /// The register holding special register `sreg`, allocating it (and scheduling its prelude
    /// `MovSReg`) on first use. The synthetic interner key starts with `$`, which a real `%`-register
    /// (stripped of its `%`) never can — so it cannot collide with a virtual register name.
    pub(super) fn reg_for(&mut self, sreg: u8, interner: &mut Interner) -> u16 {
        if let Some(&(_, r)) = self.regs.iter().find(|(s, _)| *s == sreg) {
            return r;
        }
        let r = interner.get(&format!("$sreg${sreg}"));
        self.regs.push((sreg, r));
        r
    }
    /// The leading `MovSReg` block that materializes every operand-position special register.
    pub(super) fn prelude(&self) -> Vec<Inst> {
        self.regs
            .iter()
            .map(|&(sreg, d)| Inst::MovSReg { d, sreg })
            .collect()
    }
}
