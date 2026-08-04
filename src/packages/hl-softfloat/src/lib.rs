//! Deterministic, host-floating-point-independent IEEE-754 binary arithmetic.

#![forbid(unsafe_code)]

mod arithmetic;
mod bits;
mod conversion;
mod fused;
mod selection;
mod value;

pub use arithmetic::Environment;
pub(crate) use value::{Class, Operand};
pub use value::{Comparison, ExceptionFlags, Format, NaNMode, Result, RoundingMode, TininessMode, Value};

#[cfg(test)]
#[path = "test.rs"]
mod tests;
