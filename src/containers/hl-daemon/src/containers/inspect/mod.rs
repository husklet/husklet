//! Read / report + teardown handlers, split into cohesive submodules:
//!   - `detail`  — `containers_inspect` + `container_mounts_json` (+ inspect helpers)
//!   - `list`    — `containers_json` (`docker ps`) + filter/size/status helpers
//!   - `logs`    — `containers_logs` (+ framing / tail / timestamp helpers)
//!   - `stats`   — the stats family (`containers_stats`, `stats_sample`, `pid_metrics`, …)
//!   - `top`     — `containers_top`
//!   - `diff`    — `containers_changes` + `overlay_changes` + `discard_container_layer`
//!   - `admin`   — `containers_prune` / `containers_update` / `containers_export`
//!
//! Every previously-public name is re-exported with `pub(crate) use` so the path
//! `crate::containers::<handler>` (used by the router in main.rs and every
//! `use crate::containers::*` site) resolves exactly as it did before the split.

mod admin;
mod detail;
mod diff;
mod filter;
mod frame;
mod list;
mod logs;
mod mounts;
mod stats;
mod top;

/// HTTP adapter namespace for container collection queries and maintenance operations.
pub(crate) struct Containers;

pub(crate) use admin::*;
pub(crate) use diff::*;
#[cfg(test)]
pub(crate) use list::PsQ;
pub(crate) use logs::*;
pub(crate) use stats::*;
