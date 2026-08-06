//! Owns the C native execution engine and the CPU ABI layout generated beside it.

/// Generated native CPU block-entry layout; `cpu/README.md` documents regeneration.
#[path = "cpu/rust/layout.rs"]
#[allow(non_camel_case_types)]
pub mod cpu;
