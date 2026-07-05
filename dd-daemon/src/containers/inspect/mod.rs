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
mod list;
mod logs;
mod stats;
mod top;

pub(crate) use admin::*;
pub(crate) use detail::*;
pub(crate) use diff::*;
pub(crate) use list::*;
pub(crate) use logs::*;
pub(crate) use stats::*;
pub(crate) use top::*;
