//! Crate-local test-support harness — the product-neutral engine-test model, guest provisioning,
//! result normalization and perf reruns. This was the external
//! `dd-tests` dev-dependency; it has been dissolved into its sole engine consumer (this crate), since
//! the harness only ever measures the dd-jit-darwin engine.
//!
//! It is a *test-support* module tree, not a library: every integration test / example that needs it
//! includes it at its crate root via `#[path = "support/mod.rs"] mod support;` (mirroring how the
//! `engine_matrix` case registry is shared via `#[path]`). Referenced from the included modules as
//! `crate::support::…`.
//!
//! Internally the harness couples to the host-neutral `ddjit` API (`hl_jit::Guest` / `hl_jit::available`
//! / `hl_jit::SpawnConfig`); that resolves crate-locally because `dd-jit-darwin` dev-deps `dd-jit`
//! (imported as `ddjit`).

mod harness;
pub use harness::*;
