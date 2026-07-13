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
pub(crate) use hl_images::{
    config_exposed_ports, config_labels, config_stop_signal, config_strs, config_volumes,
    default_shell, image_ref, layer_short, ref_tag, repo_tag, safe_name,
};

/// A container image's **content** identity — a real `sha256:` digest over the image's defining content
/// (its rootfs identity + OCI config: arch/cmd/entrypoint/env/workdir/user/sorted-labels). Docker keys
/// the image ID on image content, so every tag alias of one image reports the SAME id (dd squashes each
/// image to one rootfs and a `docker tag` clone copies the `rootfs` path + config, so aliases agree),
/// while two distinct rootfs/configs differ. This is the SAME id used by list, inspect, distribution and
/// pull, and it is a stable 64-hex sha256 shape (not the old tiled FNV placeholder).
pub(crate) fn image_id(i: &crate::model::Image) -> String {
    // Sort labels so HashMap iteration order can't leak into the digest (reproducible id).
    let mut lbl: Vec<(&String, &String)> = i.labels.iter().collect();
    lbl.sort();
    let labels_str = lbl
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let manifest = format!(
        "rootfs:{}\narch:{}\ncmd:{}\nentrypoint:{}\nenv:{}\nworkdir:{}\nuser:{}\nlabels:\n{}",
        i.rootfs,
        i.arch.arch(),
        i.cmd.join("\u{1}"),
        i.entrypoint.join("\u{1}"),
        i.env.join("\u{1}"),
        i.workdir,
        i.user,
        labels_str,
    );
    format!("sha256:{}", hl_images::build::sha256_hex(manifest.as_bytes()))
}
