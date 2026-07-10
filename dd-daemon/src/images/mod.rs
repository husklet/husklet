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

/// A container image's **content** identity (`sha256:…`). Docker keys the image ID on image content, so
/// every tag alias of one rootfs shares a single ID. dd squashes each image to one rootfs dir, and a
/// `docker tag` clone copies the `rootfs` path, so keying on the rootfs makes all aliases report the SAME
/// id. (Keying on the image *name* made two tags of one rootfs look like two unrelated images — clients
/// then cache/compare/delete/display retagged content as if it were distinct.)
pub(crate) fn image_id(i: &crate::model::Image) -> String {
    format!("sha256:{}", crate::util::fake_id(&i.rootfs))
}
