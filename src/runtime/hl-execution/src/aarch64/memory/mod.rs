mod decode;
mod interpreter;
mod structure;
mod vector;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Width {
    Byte,
    Half,
    Word,
    Double,
}

impl Width {
    pub const fn bytes(self) -> u8 {
        match self {
            Self::Byte => 1,
            Self::Half => 2,
            Self::Word => 4,
            Self::Double => 8,
        }
    }
}

pub(crate) use decode::Aarch64MemoryDecoder;
pub(crate) use interpreter::Aarch64MemoryInterpreter;

#[cfg(test)]
mod test;
