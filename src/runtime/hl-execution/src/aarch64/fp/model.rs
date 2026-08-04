#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperation {
    Add,
    Subtract,
    AbsoluteDifference,
    Multiply,
    MultiplyExtended,
    Divide,
    Minimum,
    Maximum,
    MinimumNumber,
    MaximumNumber,
}
pub type FpBinaryOperation = BinaryOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOperation {
    Move,
    Absolute,
    Negate,
    SquareRoot,
}
pub type FpUnaryOperation = UnaryOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Equal,
    GreaterEqual,
    Greater,
    LessEqual,
    Less,
}
pub type FpComparison = Comparison;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoundingMode {
    NearestEven,
    PositiveInfinity,
    NegativeInfinity,
    Zero,
    NearestAway,
    Current,
}
pub type FpRoundingMode = RoundingMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Half,
    Single,
    Double,
}
pub type FpFormat = Format;

pub const FPSR_INVALID: u32 = 1 << 0;
pub const FPSR_DIVIDE_BY_ZERO: u32 = 1 << 1;
pub const FPSR_OVERFLOW: u32 = 1 << 2;
pub const FPSR_UNDERFLOW: u32 = 1 << 3;
pub const FPSR_INEXACT: u32 = 1 << 4;
pub const FPSR_INPUT_DENORMAL: u32 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arithmetic {
    Binary(FpBinaryOperation),
    FusedMultiplyAdd,
    SquareRoot,
    RoundToIntegral {
        rounding: FpRoundingMode,
        exact: bool,
    },
    ConvertFormat {
        destination: FpFormat,
    },
    IntegerToFloat {
        signed: bool,
        width: u8,
    },
    FloatToInteger {
        signed: bool,
        width: u8,
        rounding: FpRoundingMode,
    },
    FloatToScaled {
        signed: bool,
        width: u8,
        scale: u8,
        rounding: FpRoundingMode,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: Arithmetic,
    pub format: FpFormat,
    pub left: u64,
    pub right: u64,
    pub addend: u64,
    /// Guest FPCR after the architectural writable mask has been applied.
    pub fpcr: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Result {
    /// Raw FP bits or integer result, according to the request operation.
    pub value: u64,
    /// Cumulative exception bits to OR into guest FPSR.
    pub exceptions: u32,
}

/// Consumer-owned correctly-rounded arithmetic boundary.
///
/// Implementations must be deterministic and must implement AArch64 NaN
/// selection, DN, FZ/FZ16, directed rounding, and tininess-before-rounding.
/// They must not depend on or mutate ambient host floating-point state.
pub trait ArithmeticPort {
    fn evaluate(&mut self, request: Request) -> Result;
}
impl FpFormat {
    pub const fn bits(self) -> u8 {
        match self {
            Self::Half => 16,
            Self::Single => 32,
            Self::Double => 64,
        }
    }
}
