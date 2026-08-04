mod decode;
mod immediate;
mod interpreter;
mod model;
mod operation;

pub(crate) use decode::Aarch64FpDecoder;
pub(crate) use immediate::ImmediateEncoding;
pub use interpreter::Aarch64FpExecutor;
pub use model::{
    Arithmetic as FpArithmetic, ArithmeticPort as FpArithmeticPort, FPSR_DIVIDE_BY_ZERO, FPSR_INEXACT,
    FPSR_INPUT_DENORMAL, FPSR_INVALID, FPSR_OVERFLOW, FPSR_UNDERFLOW, FpBinaryOperation, FpComparison, FpFormat,
    FpRoundingMode, FpUnaryOperation, Request as FpRequest, Result as FpResult,
};

#[cfg(test)]
mod test;
