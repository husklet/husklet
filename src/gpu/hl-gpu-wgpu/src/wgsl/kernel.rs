use super::*;

// ===================================================================================================
// kernel-IR → WGSL compute (ported from hl-gpu/src/ptx.rs::kernel_to_wgsl)
// ===================================================================================================

struct GlobalType(u8);

impl GlobalType {
    fn bytes(self) -> Result<u32> {
        match self.0 {
            gty::F32 | gty::U32 => Ok(4),
            gty::U64 => Err(Diagnostic::kernel(
                "wgsl lowering: 64-bit global load/store unsupported (elementwise subset)",
            )),
            other => Err(Diagnostic::kernel(format!(
                "wgsl lowering: unknown global type {other}"
            ))),
        }
    }
}

/// Static per-register pointer-region analysis: `out[r] == Some(region)` iff register `r` holds a device
/// address into that pointer-parameter region at its most recent definition. Forward, last-write-wins.
struct Analysis<'a> {
    program: &'a KernelProgram,
    regions: Vec<Option<u32>>,
}

impl<'a> Analysis<'a> {
    fn new(program: &'a KernelProgram) -> Self {
        let mut reg: Vec<Option<u32>> = vec![None; program.reg_count as usize];
        let of = |reg: &Vec<Option<u32>>, op: &Op| -> Option<u32> {
            match op {
                Op::Reg(r) => reg[*r as usize],
                _ => None,
            }
        };
        for inst in &program.insts {
            match inst {
                Inst::LdParam { d, param } => {
                    let p = &program.params[*param as usize];
                    reg[*d as usize] = if p.is_ptr { Some(p.region) } else { None };
                }
                Inst::MovReg { d, s } => reg[*d as usize] = reg[*s as usize],
                Inst::Cvta { d, s } => reg[*d as usize] = reg[*s as usize],
                Inst::IAdd { d, a, b, .. } | Inst::ISub { d, a, b, .. } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b));
                }
                Inst::IMad { d, a, b, c } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b)).or_else(|| of(&reg, c));
                }
                Inst::IMul { d, a, b, .. } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b))
                }
                Inst::MovImmI { d, .. }
                | Inst::MovImmF { d, .. }
                | Inst::MovSReg { d, .. }
                | Inst::LdGlobal { d, .. }
                | Inst::LdShared { d, .. }
                | Inst::Setp { d, .. }
                | Inst::Shift { d, .. }
                | Inst::BitOp { d, .. }
                | Inst::FAdd { d, .. }
                | Inst::FSub { d, .. }
                | Inst::FMul { d, .. }
                | Inst::FFma { d, .. }
                | Inst::Cvt { d, .. } => reg[*d as usize] = None,
                Inst::AtomGlobal { d, .. } | Inst::AtomShared { d, .. } => {
                    if let Some(dr) = d {
                        reg[*dr as usize] = None;
                    }
                }
                Inst::StGlobal { .. }
                | Inst::StShared { .. }
                | Inst::Bra { .. }
                | Inst::Bar
                | Inst::Ret
                | Inst::Nop => {}
            }
        }
        Self {
            program,
            regions: reg,
        }
    }

    /// Which pointer regions are accessed atomically → those storage buffers must be `array<atomic<u32>>`.
    fn atomic_regions(&self) -> Result<Vec<bool>> {
        let mut atomic = vec![false; self.program.num_regions as usize];
        for inst in &self.program.insts {
            if let Inst::AtomGlobal { addr, .. } = inst {
                let r = self.regions[*addr as usize].ok_or_else(|| {
                    Diagnostic::kernel(
                        "wgsl lowering: atomic through a value with no static pointer region",
                    )
                })?;
                atomic[r as usize] = true;
            }
        }
        Ok(atomic)
    }
}

/// A special-register id → the WGSL builtin/constant expression that yields its value.
struct SpecialRegister(u8);

impl SpecialRegister {
    fn expression(self, block: [u32; 3]) -> String {
        match self.0 {
            SR_TID_X => "lid.x".into(),
            SR_TID_Y => "lid.y".into(),
            SR_TID_Z => "lid.z".into(),
            SR_NTID_X => format!("{}u", block[0]),
            SR_NTID_Y => format!("{}u", block[1]),
            SR_NTID_Z => format!("{}u", block[2]),
            SR_CTAID_X => "wid.x".into(),
            SR_CTAID_Y => "wid.y".into(),
            SR_CTAID_Z => "wid.z".into(),
            SR_NCTAID_X => "nwg.x".into(),
            SR_NCTAID_Y => "nwg.y".into(),
            _ => "nwg.z".into(), // SR_NCTAID_Z
        }
    }
}

struct Operand<'a>(&'a Op);

impl Operand<'_> {
    fn u32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("r{r}"),
            Op::ImmI(i) => format!("{}u", *i as u32),
            Op::ImmF(b) => format!("{b}u"),
        }
    }

    fn i32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("bitcast<i32>(r{r})"),
            Op::ImmI(i) => format!("i32({})", *i as i32),
            Op::ImmF(b) => format!("bitcast<i32>({b}u)"),
        }
    }

    fn f32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("bitcast<f32>(r{r})"),
            Op::ImmF(b) => format!("bitcast<f32>({b}u)"),
            Op::ImmI(i) => format!("f32({i})"),
        }
    }
}

mod emit;
pub use emit::Kernel;
