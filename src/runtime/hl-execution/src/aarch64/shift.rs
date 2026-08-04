#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

pub type Aarch64Shift = Shift;
