#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadExtension {
    Zero,
    SignTo32,
    SignTo64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexExtension {
    Unsigned32,
    Unsigned64,
    Signed32,
    Signed64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Writeback {
    None,
    PreIndex,
    PostIndex,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdLogic {
    And,
    Orr,
    ExclusiveOr,
    BitClear,
    BitSelect,
    BitInsertTrue,
    BitInsertFalse,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdCopy {
    DuplicateElement { source_lane: u8 },
    DuplicateGeneral,
    InsertElement { source_lane: u8 },
    InsertGeneral,
    MoveUnsigned,
    MoveSigned,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdPermute {
    UnzipLow,
    UnzipHigh,
    TransposeLow,
    TransposeHigh,
    ZipLow,
    ZipHigh,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdUnary {
    Reverse { container_bytes: u8 },
    CountLeadingSign,
    CountLeadingZero,
    PopulationCount,
    Not,
    ReverseBits,
    CompareGreaterZero,
    CompareGreaterEqualZero,
    CompareEqualZero,
    CompareLessEqualZero,
    CompareLessZero,
    Absolute,
    Negate,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdShift {
    Left,
    Insert {
        left: bool,
    },
    Right {
        signed: bool,
        rounding: bool,
        accumulating: bool,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryAddress {
    PostRegister {
        base: u8,
        index: u8,
    },
    Base {
        register: u8,
        displacement: i64,
        writeback: Writeback,
    },
    Register {
        base: u8,
        index: u8,
        extension: IndexExtension,
        shift: u8,
    },
    Literal {
        displacement: i64,
    },
}
