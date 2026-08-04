mod bit_decode;
mod bit_operation;
mod compare_exchange;
mod cpuid;
mod decode;
mod double_shift;
mod eager;
mod extension;
mod flags;
mod fxsave;
mod increment;
mod integer;
mod interpreter;
mod lane_transfer;
mod mmx;
mod mxcsr_control;
mod real;
mod scalar;
mod staged;
mod state;
mod string;
mod vector;
mod vex;
mod x87;

pub use cpuid::{CpuidRegisters, GuestFeaturePolicy, HostCapabilities, XgetbvError};
pub use decode::{
    ByteRegister, DecodeError, DecodedInstruction, EffectiveAddress, Encoding, FetchError, InstructionFetch,
    LegacyPrefixes, Rex, Segment, X86Decoder,
};
pub use flags::{Arithmetic, Flag, FlagState, FlagUpdate, IntegerWidth};
pub use integer::{BitAction, BitPlan, BitScan, Division, DivisionError, Multiplication};
pub use interpreter::ScalarInterpreter;
pub use scalar::ir::{
    AluOperation, BitIsolation, BitScanOperation, BranchCondition, ControlFlag, FmaForm, FmaOperation,
    Instruction as ScalarInstruction, Ir as ScalarIr, MmxCount, MmxOperation, Operand as ScalarOperand,
    Register as ScalarRegister, RepeatCondition, ShiftCount, ShiftOperation, StringInstruction, StringOperation,
    UnaryOperation, VariableShift, VexImmediateShift, VexOperation, Width as ScalarWidth, X87StackOperation,
};
pub(crate) use scalar::ir::{X86AesOperation, X86ShaOperation};
pub use scalar::vector::{
    FloatArithmetic, FloatWidth, Ssse3Operation, VectorArithmetic, VectorComparison, VectorOperation, VectorPackKind,
    VectorShiftKind, VectorShuffleMode, VectorSource,
};
pub use scalar::{Decoder as X86ScalarDecoder, Error as ScalarIrError};
pub use staged::Staged;
pub use state::{CpuState, ExecutionExit, ExtendedClass, ExtendedReal};
pub use vector::{Decode as VectorDecode, Lane as VectorLane, Memory as VectorMemory, Transfer as VectorTransfer};

#[cfg(test)]
mod interpreter_test;
