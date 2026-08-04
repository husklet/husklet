//! Seccomp policy and classic-BPF evaluation.

mod policy;
mod vm;

pub use policy::*;
pub use vm::*;

#[cfg(test)]
mod test;
