//! Daemon-wide helpers, decomposed by concern. Every item keeps its original
//! `pub(crate)` visibility and is re-exported here, so `crate::util::<name>`
//! resolves exactly as it did when this was a single `util.rs`.
//!
//! The shared import header below lives in this `mod.rs`; each sibling file does
//! `use super::*;` to inherit it (child modules can see a parent's private `use`
//! imports), so no per-file bookkeeping is needed and behavior is unchanged.
pub(crate) use crate::api::ErrorMessage;
use crate::images::*;
use crate::model::*;
use crate::prelude::*;
use hl_jit::Guest;

mod discover;
mod fmt;
mod fsgen;
mod http;
mod ids;
mod paths;
mod state;

pub(crate) use discover::*;
pub(crate) use fmt::*;
pub(crate) use fsgen::*;
pub(crate) use http::*;
pub(crate) use ids::*;
pub(crate) use paths::*;
pub(crate) use state::*;

pub(crate) const API_VERSION: &str = "1.43";
