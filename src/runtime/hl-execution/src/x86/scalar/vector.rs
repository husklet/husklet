use crate::EffectiveAddress;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorSource {
    Register(u8),
    Memory(EffectiveAddress),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorOperation {
    And,
    AndNot,
    Or,
    Xor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorComparison {
    Equal,
    SignedGreater,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatArithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    SquareRoot,
    Minimum,
    Maximum,
    Reciprocal,
    ReciprocalSquareRoot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatWidth {
    Single,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorArithmetic {
    Add,
    Subtract,
    UnsignedMinimum,
    UnsignedMaximum,
    SignedMinimum,
    SignedMaximum,
    UnsignedMultiplyEvenDwords,
    SignedMultiplyEvenDwords,
    MultiplyLowDwords,
    MultiplyLowWords,
    MultiplyHighWords { signed: bool },
    MultiplyAddWords,
    SumAbsoluteDifferences,
    AddUnsignedSaturating,
    Average,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ssse3Operation {
    Horizontal { subtract: bool, saturating: bool },
    Sign,
    RoundedMultiply,
    MultiplyAdd,
    Absolute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorShuffleMode {
    Dwords,
    LowWords,
    HighWords,
    PackedSingle,
    PackedDouble,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorShiftKind {
    LogicalRight,
    ArithmeticRight,
    Left,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorPackKind {
    SignedBytes,
    UnsignedBytes,
    SignedWords,
    UnsignedWords,
}
