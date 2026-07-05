//! Docker/OCI model DTOs — the flat wire contract a container inherits/reports, split per
//! domain. Grouped into siblings (image/health/container/mount/network) but re-exported flat
//! via `pub(crate) use <sib>::*` so `crate::model::*` resolves every type UNCHANGED.
use super::*;

mod container;
mod health;
mod image;
mod mount;
mod network;

pub(crate) use container::*;
pub(crate) use health::*;
pub(crate) use image::*;
pub(crate) use mount::*;
pub(crate) use network::*;
