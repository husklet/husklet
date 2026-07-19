//! Pure `docker ps` helpers: `--size` accounting (`container_sizes`), `--filter` matching
//! (`ps_match`), and the humanized Status column (`human_status`). Side-effect-free container→value
//! transforms (bar the on-disk `du` walk in `container_sizes`) split out from the async list handler
//! in `list.rs`, which pulls them back in via `use super::filter::*`.
//!
//! Split by concern:
//!   - `predicate` — `ps_match` (the `docker ps --filter` predicate)
//!   - `status`    — `human_status` (the Status-column string renderer)
//!   - `sizes`     — `container_sizes` (the SizeRw/SizeRootFs disk walk)
use super::super::*;

mod predicate;
mod sizes;
mod status;
pub(crate) use predicate::*;

/// Shared test fixture: a minimal running `nginx` container. Lives here (not in a single submodule)
/// because all three submodules' test suites build off it; reachable from each via `use super::*`.
#[cfg(test)]
pub(super) fn ctr() -> Container {
    Container {
        id: "abc123def456".into(),
        image: "nginx".into(),
        status: "running".into(),
        ..Default::default()
    }
}
