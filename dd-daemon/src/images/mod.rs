//! Docker image HTTP handlers + helpers, decomposed by concern:
//! - `query`    — read/report handlers (list / history / search / prune / inspect / distribution).
//! - `pull`     — `POST /images/create` (pull/import dispatch) + registry pull/refresh/config helpers.
//! - `tags`     — tag / rmi / rescan / register (in-memory store mutations).
//! - `transfer` — push / save / load / import (archive + registry transfer).
//!
//! Every previously-public name stays reachable as `crate::images::…` via the glob re-exports below,
//! so the router and sibling modules (`use crate::images::*`) keep resolving unchanged.

mod pull;
mod query;
mod tags;
mod transfer;

pub(crate) use pull::*;
pub(crate) use query::*;
pub(crate) use tags::*;
pub(crate) use transfer::*;

// Image ref / store-name / OCI-config / repo-tag / default-command helpers live in dd-images (usable
// standalone, runtime-agnostic); re-export so existing `crate::images::*` call sites keep resolving.
pub(crate) use dd_images::{
    config_exposed_ports, config_labels, config_stop_signal, config_strs, config_volumes,
    default_shell, image_ref, layer_short, ref_tag, repo_tag, safe_name,
};
