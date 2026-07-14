//! hl_wip-integration — the Linux-phase CAPSTONE.
//!
//! This crate carries NO library code of its own: it exists only to host the two integration tests in
//! `tests/`, which compose the REAL driver + runtime crates end-to-end. See `tests/plug.rs` (the
//! `engine.add(Cuda::new())` injection seam) and `tests/lower.rs` (all three drivers lowering onto one
//! shared runtime + CPU executor). The empty lib target just gives the package something to link.
