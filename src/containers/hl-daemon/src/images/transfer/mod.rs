//! Archive + registry transfer: `docker push` / `docker save` / `docker load` / `docker import`.
//!
//! hl's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus a `hl-manifest.json` sidecar recording the image identity (name + run
//! config). `docker save` produces it, `docker load` consumes it; `docker import` instead takes a
//! bare rootfs tar (no manifest) whose files land directly in a new image's rootfs.
//!
//! One file per operation; each sibling reaches this header (and its siblings' public names) via
//! `use super::*`:
//! - `save`   — `docker save`   (GET /images/get, tar out).
//! - `load`   — `docker load`   (POST /images/load, hl-save archive in).
//! - `import` — `docker import` (POST /images/create with fromSrc, bare rootfs tar in).
//! - `push`   — `docker push`   (POST /images/:name/push, registry upload).
use super::*;
use crate::api::*;
use crate::model::*;
use crate::prelude::*;
use crate::util::*;

mod import;
mod load;
mod push;
mod save;

pub(crate) use import::*;
pub(crate) use load::*;
pub(crate) use push::*;
#[cfg(test)]
pub(crate) use save::SaveQ;
