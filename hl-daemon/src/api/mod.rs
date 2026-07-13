//! Typed Docker Engine API **response** DTOs.
//!
//! These `#[derive(Serialize)]` structs replace hand-rolled inline `serde_json::json!({…})` response
//! builders for the small, self-contained handlers (`/version`, `/info`, `/system/df`, `/auth`,
//! networks, volumes, events). They serialize to the EXACT same JSON shape the inline builders
//! produced — clients (docker CLI / bollard) are strict about keys, so the field renames below are
//! load-bearing. Most keys are a plain PascalCase of the snake_case field name (handled by
//! `#[serde(rename_all = "PascalCase")]`); the few that aren't carry an explicit `#[serde(rename)]`
//! (e.g. `ID`, `OSType`, `NCPU`, `MinAPIVersion`, `EndpointID`, `IPv4Address`, `EnableIPv6`, `IPAM`).
//!
//! The DTOs are split into per-domain sibling files by their Docker endpoint area; every type is
//! glob-re-exported below so `crate::api::X` resolves unchanged for every handler.

mod build;
mod container;
mod error;
mod event;
mod exec;
mod image;
mod network;
mod system;
mod version;
mod volume;

pub(crate) use build::*;
pub(crate) use container::*;
pub(crate) use error::*;
pub(crate) use event::*;
pub(crate) use exec::*;
pub(crate) use image::*;
pub(crate) use network::*;
pub(crate) use system::*;
pub(crate) use version::*;
pub(crate) use volume::*;
