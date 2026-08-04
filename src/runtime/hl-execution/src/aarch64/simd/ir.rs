#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AesOperation {
    Encrypt,
    Decrypt,
    MixColumns,
    InverseMixColumns,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sha256Operation {
    Hash,
    HashSecond,
    ScheduleZero,
    ScheduleOne,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sha1Operation {
    Choose,
    Parity,
    Majority,
    Hash,
    ScheduleZero,
    ScheduleOne,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaneOperation {
    Multiply,
    MultiplyAccumulate { subtract: bool },
    CompareGreater { unsigned: bool },
    CompareGreaterEqual { unsigned: bool },
    CompareEqual,
    TestBits,
    Maximum { unsigned: bool },
    Minimum { unsigned: bool },
    PairAdd,
    PairMaximum { unsigned: bool },
    PairMinimum { unsigned: bool },
    HalvingAdd { unsigned: bool, rounding: bool },
    HalvingSubtract { unsigned: bool },
}
pub type SimdLaneOperation = LaneOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WideOperation {
    PairAddLong,
    AddLong,
    AddWide,
    SubtractLong,
    SubtractWide,
    AddHighNarrow {
        rounding: bool,
    },
    SubtractHighNarrow {
        rounding: bool,
    },
    MultiplyLong,
    MultiplyAccumulateLong {
        subtract: bool,
    },
    SaturatingNarrow {
        source_signed: bool,
        destination_signed: bool,
    },
    ShiftNarrow {
        amount: u8,
        rounding: bool,
        mode: NarrowMode,
    },
    ShiftLong {
        amount: u8,
    },
}
pub type SimdWideOperation = WideOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NarrowMode {
    Truncate,
    Saturate {
        source_signed: bool,
        destination_signed: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaturatingLongOperation {
    Multiply,
    Accumulate { subtract: bool },
}
pub type SimdSaturatingLongOperation = SaturatingLongOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatrixSignedness {
    Signed,
    Unsigned,
    UnsignedSigned,
}
pub type SimdMatrixSignedness = MatrixSignedness;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReduceOperation {
    Add,
    AddLong { signed: bool },
    Maximum { unsigned: bool },
    Minimum { unsigned: bool },
}
pub type SimdReduceOperation = ReduceOperation;
