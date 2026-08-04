mod address;
mod instruction;

pub use address::*;
pub use instruction::Aarch64Instruction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ir {
    pub word: u32,
    pub wide: bool,
    pub instruction: Aarch64Instruction,
}
pub type Aarch64Ir = Ir;
