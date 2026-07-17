//! `/containers` DTOs — `top`, `stats`, `Mounts[]`, `inspect`, `ps` list rows, create ack, and the
//! published-port shapes.
//!
//! Split into per-endpoint sibling files (`list`/`inspect`/`stats`/`admin`); every type is
//! glob-re-exported below so `crate::api::X` resolves unchanged for every handler.

mod admin;
mod inspect;
mod list;
mod stats;

pub(crate) use admin::*;
pub(crate) use inspect::*;
pub(crate) use list::*;
pub(crate) use stats::*;
