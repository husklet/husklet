use super::vector::{
    FloatArithmetic, FloatWidth, Ssse3Operation, VectorArithmetic, VectorComparison, VectorOperation, VectorPackKind,
    VectorShiftKind, VectorShuffleMode, VectorSource,
};
use crate::{ByteRegister, EffectiveAddress};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Width {
    Byte,
    Word,
    Dword,
    Qword,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Register {
    General(u8),
    Byte(ByteRegister),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operand {
    Register(Register),
    Memory(EffectiveAddress),
    Immediate(i64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X87StackOperation {
    Load,
    Exchange,
    Store,
    StorePop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MmxOperation {
    And,
    AndNot,
    Or,
    Xor,
    Add(u8),
    Subtract(u8),
    AddUnsigned(u8),
    SubtractUnsigned(u8),
    AddSigned(u8),
    SubtractSigned(u8),
    Equal(u8),
    Greater(u8),
    Extrema { lane: u8, signed: bool, minimum: bool },
    Average(u8),
    Unpack { lane: u8, high: bool },
    Pack(VectorPackKind),
    MultiplyLow,
    MultiplyHigh,
    UnsignedMultiplyHigh,
    MultiplyAdd,
    UnsignedMultiplyDword,
    SumAbsoluteDifferences,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MmxCount {
    Immediate(u8),
    Source(VectorSource),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AluOperation {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
    Compare,
    Test,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VexOperation {
    And,
    AndNot,
    Or,
    Xor,
    AddSingle,
    AddDouble,
    MultiplySingle,
    MultiplyDouble,
    DotSingle,
    DotDouble,
    CarrylessMultiply,
    MultipleSad,
    Permute128,
    AddByte,
    AddWord,
    AddDword,
    SubtractByte,
    SubtractWord,
    SubtractDword,
    SubtractQword,
    Saturating {
        subtract: bool,
        unsigned: bool,
        word: bool,
    },
    Extrema {
        maximum: bool,
        unsigned: bool,
        bytes: u8,
    },
    Average {
        word: bool,
    },
    MultiplyAddWords,
    MultiplyAddBytes,
    Horizontal {
        subtract: bool,
        saturating: bool,
        dword: bool,
    },
    Sign {
        bytes: u8,
    },
    Absolute {
        bytes: u8,
    },
    MultiplyHighRoundWord,
    HorizontalMinimumWord,
    SumAbsoluteDifferences,
    MultiplyLowDword,
    MultiplyLowWord,
    MultiplyHighWordSigned,
    MultiplyHighWordUnsigned,
    MultiplyDwordSigned,
    MultiplyDwordUnsigned,
    BlendWord,
    BlendDword,
    BlendQword,
    PackSignedWordByte,
    PackSignedDwordWord,
    PackUnsignedWordByte,
    PackUnsignedDwordWord,
    PermuteDword,
    PermuteQword,
    PermuteLaneDword {
        variable: bool,
    },
    PermuteLaneQword {
        variable: bool,
    },
    ShuffleByte,
    ShiftLeftVariableDword,
    ShiftLeftVariableQword,
    ShiftRightVariableDword,
    ShiftRightVariableQword,
    ShiftArithmeticVariableDword,
    UnpackLowByte,
    UnpackLowWord,
    UnpackLowDword,
    UnpackLowQword,
    UnpackHighByte,
    UnpackHighWord,
    UnpackHighDword,
    UnpackHighQword,
    BroadcastByte,
    BroadcastWord,
    BroadcastDword,
    BroadcastQword,
    Broadcast128,
    DuplicateDouble,
    DuplicateLowSingle,
    DuplicateHighSingle,
    Insert128,
    WidenSignedDword,
    AddQword,
    /// Packed integer compare; `lane` is the element width in bytes.
    Compare {
        comparison: VectorComparison,
        lane: u8,
    },
    ShiftRightBytes,
    ShuffleDword,
    ShuffleSingle,
    ShuffleDouble,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VexImmediateShift {
    LogicalRight,
    ArithmeticRight,
    LogicalLeft,
    ByteRight,
    ByteLeft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FmaForm {
    Form132,
    Form213,
    Form231,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FmaOperation {
    Add,
    Subtract,
    NegativeAdd,
    NegativeSubtract,
    AddSubtract,
    SubtractAdd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86AesOperation {
    Encrypt,
    EncryptLast,
    Decrypt,
    DecryptLast,
    InverseMix,
    KeyAssist(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86ShaOperation {
    Sha1Next,
    Sha1Message1,
    Sha1Message2,
    Sha1Rounds4(u8),
    Sha256Rounds2,
    Sha256Message1,
    Sha256Message2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitIsolation {
    Reset,
    Mask,
    Isolate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VariableShift {
    Left,
    LogicalRight,
    ArithmeticRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BranchCondition(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StringOperation {
    Move,
    Store,
    Load,
    Compare,
    Scan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepeatCondition {
    None,
    Count,
    WhileEqual,
    WhileNotEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShiftOperation {
    RotateLeft,
    RotateRight,
    CarryLeft,
    CarryRight,
    Left,
    Right,
    ArithmeticRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShiftCount {
    One,
    Immediate(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOperation {
    Not,
    Negate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFlag {
    ComplementCarry,
    ClearCarry,
    SetCarry,
    ClearDirection,
    SetDirection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitScanOperation {
    Forward,
    Reverse,
    TrailingZeroCount,
    LeadingZeroCount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StringInstruction {
    pub operation: StringOperation,
    pub repeat: RepeatCondition,
    pub address_32: bool,
    pub source_segment: Option<crate::Segment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    Move {
        destination: Operand,
        source: Operand,
    },
    EndianMove {
        register: Register,
        address: EffectiveAddress,
        store: bool,
    },
    Crc32c {
        destination: Register,
        source: Operand,
        source_width: Width,
    },
    ByteSwap {
        register: Register,
    },
    Exchange {
        destination: Operand,
        source: Register,
    },
    AccumulatorExchange {
        source: Register,
    },
    ExchangeAdd {
        destination: Operand,
        source: Register,
        locked: bool,
    },
    CompareExchange {
        destination: Operand,
        source: Register,
        locked: bool,
    },
    WideCompareExchange {
        address: EffectiveAddress,
        wide: bool,
        locked: bool,
    },
    VectorMove {
        vector: u8,
        scalar: Operand,
        to_vector: bool,
    },
    VectorUnpack {
        destination: u8,
        source: VectorSource,
        lane: u8,
        high: bool,
    },
    VectorStore {
        source: u8,
        destination: VectorSource,
    },
    VectorLoad {
        destination: u8,
        source: VectorSource,
    },
    VectorScalarMove {
        destination: u8,
        operand: VectorSource,
        store: bool,
        format: FloatWidth,
    },
    VectorBitwise {
        operation: VectorOperation,
        destination: u8,
        source: VectorSource,
    },
    VectorTransport {
        vector: u8,
        operand: VectorSource,
        store: bool,
        aligned: bool,
    },
    VexVectorTransport {
        vector: u8,
        operand: VectorSource,
        store: bool,
        wide: bool,
    },
    VexScalarMerge {
        destination: u8,
        first: u8,
        second: VectorSource,
        double: bool,
    },
    VexScalarLoad {
        destination: u8,
        source: VectorSource,
        double: bool,
    },
    VexScalarMultiply {
        destination: u8,
        first: u8,
        second: VectorSource,
    },
    VexInsertSingle {
        destination: u8,
        first: u8,
        second: VectorSource,
        control: u8,
    },
    VexFloatArithmetic {
        operation: FloatArithmetic,
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        scalar: bool,
        wide: bool,
    },
    VexFma {
        operation: FmaOperation,
        form: FmaForm,
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        scalar: bool,
        wide: bool,
    },
    VexHalfWiden {
        destination: u8,
        source: VectorSource,
        wide: bool,
    },
    VexPackedDoubleConvert {
        destination: u8,
        source: VectorSource,
        from_integer: bool,
        truncate: bool,
        wide: bool,
    },
    VexHalfNarrow {
        source: u8,
        destination: VectorSource,
        wide: bool,
        control: u8,
    },
    VexBinary {
        operation: VexOperation,
        destination: u8,
        first: u8,
        second: VectorSource,
        wide: bool,
        immediate: u8,
    },
    VexImmediateShift {
        operation: VexImmediateShift,
        destination: u8,
        source: u8,
        lane: u8,
        wide: bool,
        count: u8,
    },
    VexScalarCountShift {
        operation: VexImmediateShift,
        destination: u8,
        source: u8,
        count: VectorSource,
        lane: u8,
        wide: bool,
    },
    VexGather {
        destination: u8,
        mask: u8,
        index: u8,
        address: crate::EffectiveAddress,
        element: u8,
        index_bytes: u8,
        wide: bool,
    },
    VexExtract128 {
        source: u8,
        destination: VectorSource,
        high: bool,
    },
    VexVectorToGeneral {
        source: u8,
        destination: Register,
        wide: bool,
    },
    VexGeneralToVector {
        destination: u8,
        source: Operand,
        wide: bool,
    },
    VexQword {
        vector: u8,
        operand: VectorSource,
        store: bool,
    },
    VexHalfMove {
        destination: u8,
        first: u8,
        second: VectorSource,
        high: bool,
    },
    VexHalfStore {
        source: u8,
        address: crate::EffectiveAddress,
        high: bool,
    },
    VexMask {
        destination: Register,
        source: u8,
        lane: u8,
        wide: bool,
    },
    VexDwordToSingle {
        destination: u8,
        source: VectorSource,
        wide: bool,
        to_integer: bool,
        truncate: bool,
    },
    VexFloatWidth {
        destination: u8,
        first: u8,
        source: VectorSource,
        destination_format: FloatWidth,
        packed: bool,
        wide: bool,
    },
    VexCompare {
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        scalar: bool,
        wide: bool,
        predicate: u8,
    },
    VexRound {
        destination: u8,
        first: u8,
        source: VectorSource,
        format: FloatWidth,
        scalar: bool,
        wide: bool,
        control: u8,
    },
    VexBlend {
        destination: u8,
        first: u8,
        second: VectorSource,
        mask: u8,
        lane: u8,
        wide: bool,
    },
    RotateRightNoFlags {
        destination: Register,
        source: Operand,
        count: u8,
    },
    IsolateBit {
        operation: BitIsolation,
        destination: Register,
        source: Operand,
    },
    AndNotGeneral {
        destination: Register,
        first: Register,
        second: Operand,
    },
    ZeroHighBits {
        destination: Register,
        source: Operand,
        index: Register,
    },
    VariableShift {
        operation: VariableShift,
        destination: Register,
        source: Operand,
        count: Register,
    },
    MultiplyExtended {
        high: Register,
        low: Register,
        source: Operand,
    },
    TransferBits {
        destination: Register,
        source: Register,
        mask: Operand,
        deposit: bool,
    },
    VexZeroUpper,
    VexZeroAll,
    VectorByteShift {
        vector: u8,
        left: bool,
        count: u8,
    },
    VectorLaneShift {
        vector: u8,
        lane: u8,
        kind: VectorShiftKind,
        count: u8,
    },
    VectorVariableShift {
        vector: u8,
        count: VectorSource,
        lane: u8,
        kind: VectorShiftKind,
    },
    PackedString {
        left: u8,
        right: VectorSource,
        control: u8,
        explicit: bool,
        mask: bool,
        wide_lengths: bool,
    },
    Ssse3 {
        operation: Ssse3Operation,
        lane: u8,
        destination: u8,
        source: VectorSource,
    },
    VectorAlign {
        destination: u8,
        source: VectorSource,
        count: u8,
    },
    CarrylessMultiply {
        destination: u8,
        source: VectorSource,
        control: u8,
    },
    VectorPack {
        destination: u8,
        source: VectorSource,
        kind: VectorPackKind,
    },
    Increment {
        operand: Operand,
        decrement: bool,
        locked: bool,
    },
    DoubleShift {
        destination: Operand,
        source: Register,
        right: bool,
        count: ShiftCount,
    },
    X87Control {
        address: EffectiveAddress,
        load: bool,
    },
    X87Extended {
        address: EffectiveAddress,
        load: bool,
    },
    X87Float {
        address: EffectiveAddress,
        format: FloatWidth,
        store: bool,
        pop: bool,
    },
    X87Compare {
        source: u8,
        ordered: bool,
        pop: bool,
    },
    X87ConditionalMove {
        source: u8,
        condition: u8,
        negate: bool,
    },
    X87Stack {
        source: u8,
        operation: X87StackOperation,
    },
    X87Initialize,
    X87Status,
    X87StatusStore {
        address: EffectiveAddress,
    },
    X87Constant {
        constant: u8,
    },
    X87Environment {
        address: EffectiveAddress,
        load: bool,
    },
    X87Arithmetic {
        address: Option<EffectiveAddress>,
        source: u8,
        operation: u8,
        destination_source: bool,
        pop: bool,
        format: FloatWidth,
        integer_bytes: u8,
    },
    X87StatusCompare {
        address: Option<EffectiveAddress>,
        source: u8,
        pop: u8,
        format: FloatWidth,
        ordered: bool,
    },
    X87Integer {
        address: EffectiveAddress,
        bytes: u8,
        load: bool,
        pop: bool,
        truncate: bool,
    },
    X87Unary {
        operation: u8,
        source: u8,
    },
    X87Save {
        address: EffectiveAddress,
        load: bool,
    },
    MxcsrControl {
        address: EffectiveAddress,
        load: bool,
    },
    Fxsave {
        address: EffectiveAddress,
    },
    Fxrstor {
        address: EffectiveAddress,
    },
    ConvertFloatInteger {
        destination: Register,
        source: VectorSource,
        wide: bool,
        format: FloatWidth,
        truncate: bool,
    },
    ConvertIntegerFloat {
        destination: u8,
        source: Operand,
        wide: bool,
        format: FloatWidth,
        merge: Option<u8>,
    },
    ConvertFloatWidth {
        destination: u8,
        source: VectorSource,
        destination_format: FloatWidth,
        packed: bool,
    },
    ConvertPackedSingle {
        destination: u8,
        source: VectorSource,
        to_integer: bool,
        truncate: bool,
    },
    ConvertPackedDouble {
        destination: u8,
        source: VectorSource,
        from_integer: bool,
        truncate: bool,
    },
    VectorFloatArithmetic {
        operation: FloatArithmetic,
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
    },
    VectorPairArithmetic {
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        subtract: bool,
        alternating: bool,
    },
    VexPairArithmetic {
        destination: u8,
        first: u8,
        second: VectorSource,
        format: FloatWidth,
        subtract: bool,
        alternating: bool,
        wide: bool,
    },
    VexVectorTest {
        left: u8,
        right: VectorSource,
        lane: u8,
        wide: bool,
    },
    VexMaskedMemory {
        vector: u8,
        mask: u8,
        address: crate::EffectiveAddress,
        lane: u8,
        store: bool,
        wide: bool,
    },
    VectorFloatCompare {
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
        predicate: u8,
    },
    VectorScalarCompare {
        left: u8,
        right: VectorSource,
        format: FloatWidth,
        signaling_only: bool,
    },
    VectorInteger {
        operation: VectorArithmetic,
        destination: u8,
        source: VectorSource,
        lane: u8,
    },
    VectorShuffle {
        mode: VectorShuffleMode,
        destination: u8,
        source: VectorSource,
        selectors: u8,
    },
    VectorByteShuffle {
        destination: u8,
        control: VectorSource,
    },
    VectorCompare {
        comparison: VectorComparison,
        destination: u8,
        source: VectorSource,
        lane: u8,
    },
    VectorTest {
        left: u8,
        right: VectorSource,
    },
    Aes {
        operation: X86AesOperation,
        destination: u8,
        source: VectorSource,
    },
    VexAes {
        operation: X86AesOperation,
        destination: u8,
        first: u8,
        second: VectorSource,
    },
    Sha {
        operation: X86ShaOperation,
        destination: u8,
        source: VectorSource,
    },
    VectorExtend {
        destination: u8,
        source: VectorSource,
        source_lane: u8,
        destination_lane: u8,
        signed: bool,
    },
    VectorBlend {
        destination: u8,
        source: VectorSource,
        lane: u8,
        selectors: u16,
        implicit: bool,
    },
    VectorHorizontalMinimum {
        destination: u8,
        source: VectorSource,
    },
    VectorSad {
        destination: u8,
        source: VectorSource,
        control: u8,
    },
    VectorDot {
        destination: u8,
        source: VectorSource,
        control: u8,
        format: FloatWidth,
    },
    VectorRound {
        destination: u8,
        source: VectorSource,
        format: FloatWidth,
        packed: bool,
        control: u8,
    },
    VectorLaneInsert {
        destination: u8,
        source: Operand,
        bytes: u8,
        lane: u8,
    },
    VectorLaneExtract {
        source: u8,
        destination: Operand,
        bytes: u8,
        lane: u8,
    },
    VectorInsertSingle {
        destination: u8,
        source: VectorSource,
        control: u8,
    },
    VectorMask {
        destination: Register,
        source: u8,
        lane: u8,
    },
    VectorMaskedStore {
        source: u8,
        mask: u8,
        mmx: bool,
        address: EffectiveAddress,
    },
    VectorInsertWord {
        destination: u8,
        source: Operand,
        lane: u8,
    },
    VectorHalf {
        vector: u8,
        source: VectorSource,
        store: bool,
        high: bool,
    },
    VectorDuplicate {
        destination: u8,
        source: VectorSource,
        lane: u8,
        high: bool,
    },
    MmxScalar {
        register: u8,
        operand: Operand,
        store: bool,
    },
    MmxTransport {
        register: u8,
        operand: VectorSource,
        store: bool,
    },
    MmxVector {
        mmx: u8,
        vector: u8,
        to_vector: bool,
    },
    MmxExtractWord {
        source: u8,
        destination: Register,
        lane: u8,
    },
    MmxMask {
        destination: Register,
        source: u8,
    },
    MmxInsertWord {
        destination: u8,
        source: Operand,
        lane: u8,
    },
    MmxConvertToFloat {
        destination: u8,
        source: VectorSource,
        double: bool,
    },
    MmxConvertFromFloat {
        destination: u8,
        source: VectorSource,
        double: bool,
        truncate: bool,
    },
    MmxPacked {
        operation: MmxOperation,
        destination: u8,
        source: VectorSource,
    },
    MmxShift {
        kind: VectorShiftKind,
        lane: u8,
        destination: u8,
        count: MmxCount,
    },
    MmxEmpty,
    BitScan {
        operation: BitScanOperation,
        destination: Register,
        source: Operand,
    },
    PopulationCount {
        destination: Register,
        source: Operand,
    },
    Xlat {
        address_32: bool,
        segment: Option<crate::Segment>,
    },
    BitOperation {
        action: crate::BitAction,
        destination: Operand,
        index: Operand,
        locked: bool,
    },
    AccumulatorSignExtend {
        source_width: Width,
    },
    AccumulatorHighExtend,
    MoveSignExtend {
        destination: Register,
        source: Operand,
        source_width: Width,
    },
    MoveZeroExtend {
        destination: Register,
        source: Operand,
        source_width: Width,
    },
    Lea {
        destination: Register,
        address: EffectiveAddress,
    },
    Push {
        source: Operand,
    },
    Pop {
        destination: Operand,
    },
    PushFlags,
    PopFlags,
    ReadSelector {
        destination: Operand,
        value: u16,
    },
    WriteSelector,
    Iret,
    FlagControl(ControlFlag),
    Alu {
        operation: AluOperation,
        destination: Operand,
        source: Operand,
        locked: bool,
    },
    Shift {
        operation: ShiftOperation,
        destination: Operand,
        count: ShiftCount,
    },
    Unary {
        operation: UnaryOperation,
        operand: Operand,
    },
    Multiply {
        signed: bool,
        source: Operand,
    },
    TruncatedMultiply {
        destination: Register,
        source: Operand,
        multiplier: Option<i64>,
    },
    Divide {
        signed: bool,
        divisor: Operand,
    },
    Jump {
        target: u64,
    },
    JumpIndirect {
        target: Operand,
    },
    JumpConditional {
        condition: BranchCondition,
        target: u64,
    },
    CountBranch {
        target: u64,
        address_32: bool,
        decrement: bool,
        zero: Option<bool>,
    },
    ConditionalMove {
        condition: BranchCondition,
        destination: Register,
        source: Operand,
    },
    SetConditional {
        condition: BranchCondition,
        destination: Operand,
    },
    FlagsFromAh,
    AhFromFlags,
    Call {
        target: u64,
    },
    CallIndirect {
        target: Operand,
    },
    Return {
        pop_bytes: u16,
    },
    Leave {
        address_32: bool,
    },
    String(StringInstruction),
    Nop,
    Undefined,
    Breakpoint,
    Cpuid,
    TimestampCounter {
        auxiliary: bool,
    },
    Syscall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ir {
    pub length: u8,
    pub width: Width,
    pub instruction: Instruction,
}
