#![allow(unused_imports, dead_code)]
//! Daemon-wide helpers, decomposed by concern. Every item keeps its original
//! `pub(crate)` visibility and is re-exported here, so `crate::util::<name>`
//! resolves exactly as it did when this was a single `util.rs`.
//!
//! The shared import header below lives in this `mod.rs`; each sibling file does
//! `use super::*;` to inherit it (child modules can see a parent's private `use`
//! imports), so no per-file bookkeeping is needed and behavior is unchanged.
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::runtime::*;
use crate::system::*;
use crate::volumes::*;
use crate::prelude::*;
use ddjit::{Guest, PortMap, SpawnConfig, Volume};

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
