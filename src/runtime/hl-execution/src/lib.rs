//! Safe CPU execution contracts and validated immutable execution artifacts.

mod aarch64;
mod artifact;
pub(crate) use aarch64::*;
mod digest;
mod execution;
mod identity;
mod operand_memory;
mod persistence;
mod projection;
mod relocation;
mod retained_cache;
mod trace_register;
mod x86;

pub use aarch64::atomic::{
    Generation as MappingGeneration, Load as ExclusiveLoad, Memory as ExclusiveMemory, Operation as AtomicOperation,
    Order as MemoryOrder, Reservation as ExclusiveReservation, Value as AtomicValue,
};
pub use aarch64::coordinate::Port as PcCoordinatePort;
pub use aarch64::crc32::CrcPolynomial;
pub use aarch64::decode::{Aarch64DecodeError, Aarch64Decoder};
pub use aarch64::fp::Aarch64FpExecutor;
pub use aarch64::fp::{
    FPSR_DIVIDE_BY_ZERO, FPSR_INEXACT, FPSR_INPUT_DENORMAL, FPSR_INVALID, FPSR_OVERFLOW, FPSR_UNDERFLOW, FpArithmetic,
    FpArithmeticPort, FpBinaryOperation, FpComparison, FpFormat, FpRequest, FpResult, FpRoundingMode, FpUnaryOperation,
};
pub use aarch64::integer::{
    BitfieldOperation, BranchCondition as Aarch64BranchCondition, CompareOperand, DivideOperation, LogicalOperation,
    MoveWideOperation, MultiplyOperation, RegisterExtension,
};
pub use aarch64::ir::{
    Aarch64Instruction, Aarch64Ir, IndexExtension, LoadExtension, MemoryAddress, SimdCopy, SimdLogic, SimdPermute,
    SimdShift, SimdUnary, Writeback,
};
pub use aarch64::memory::Width as MemoryWidth;
pub use aarch64::shift::Aarch64Shift;
pub use aarch64::simd::{
    AesOperation, NarrowMode, Sha1Operation, Sha256Operation, SimdLaneOperation, SimdMatrixSignedness,
    SimdReduceOperation, SimdSaturatingLongOperation, SimdWideOperation,
};
pub use aarch64::softfloat::Aarch64SoftFloat;
pub use aarch64::state::{Aarch64CpuState, Nzcv};
pub use aarch64::system::{Barrier as BarrierKind, Port as GuestSystemPort, Register as SystemRegister};
pub use aarch64::{Aarch64ExecutionExit, interpreter::Aarch64Interpreter};
pub use artifact::{ArenaRequest, CodeArtifact, CodePublisher, Publication, PublicationError, ValidatedCodeArtifact};
pub use digest::{ArtifactDigest, DIGEST_SEED};
pub use execution::{
    ArchitecturalCounter, BlockIdentity, CacheObservation, DispatchDecision, DispatchError, EXECUTION_SNAPSHOT_VERSION,
    ExecutionCpuSnapshot, ExecutionFault, ExecutionInstructionMemory, ExecutionMachine, ExecutionSnapshot,
    ExecutionStateError, InstructionEpoch, StepOutcome, SynchronousTrap, TranslationEmission, TranslationRequest,
    TrapSignal, TrapState,
};
pub use identity::{CacheIdentity, FileIdentity};
pub use operand_memory::{AccessKind, FaultAccess, GuestOperandMemory, MemoryFault};
pub use persistence::{
    AARCH64_CACHE_ABI, ArtifactCursor, ArtifactName, ArtifactStore, CacheCompatibility, CacheEnvelope,
    PersistenceError, X86_64_CACHE_ABI,
};
pub use projection::{MemoryProjection, ProjectionControl, ProjectionTransition};
pub use relocation::{Materialization, RelocationError, RelocationRecord, RelocationTable};
pub use retained_cache::{CacheExpectations, RetainedCache, RetainedCacheError};
pub use trace_register::{
    Aarch64Prstatus, StoppedRegisterImage, StoppedRegisters, TRACE_REGISTER_VERSION, TraceRegisterError,
    TraceSafepointPort, X86Prstatus,
};
pub use x86::{
    AluOperation, Arithmetic, BitAction, BitPlan, BitScan, BitScanOperation, BranchCondition, ByteRegister,
    ControlFlag, CpuState, CpuidRegisters, DecodeError, DecodedInstruction, Division, DivisionError, EffectiveAddress,
    Encoding, ExecutionExit, ExtendedClass, ExtendedReal, FetchError, Flag, FlagState, FlagUpdate, FloatArithmetic,
    FloatWidth, GuestFeaturePolicy, HostCapabilities, InstructionFetch, IntegerWidth, LegacyPrefixes, MmxCount,
    MmxOperation, Multiplication, RepeatCondition, Rex, ScalarInstruction, ScalarInterpreter, ScalarIr, ScalarIrError,
    ScalarOperand, ScalarRegister, ScalarWidth, Segment, ShiftCount, ShiftOperation, Staged, StringInstruction,
    StringOperation, UnaryOperation, VectorArithmetic, VectorComparison, VectorDecode, VectorLane, VectorMemory,
    VectorOperation, VectorPackKind, VectorShiftKind, VectorShuffleMode, VectorSource, VectorTransfer, X86Decoder,
    X86ScalarDecoder, X87StackOperation, XgetbvError,
};

#[cfg(test)]
mod test;
