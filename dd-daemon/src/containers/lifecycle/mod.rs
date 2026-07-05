#![allow(unused_imports, dead_code)]
//! Container lifecycle / control handlers, decomposed by concern. Every handler
//! was moved verbatim from the former single-file `lifecycle.rs`; behavior is
//! unchanged (pure file reshaping). Submodules:
//!   - `create` — `POST /containers/create` + create-body/host-config DTOs,
//!     published-port assembly (`publish_str`/`publish_str_alloc`) and anonymous-
//!     volume seeding (populateVolumes).
//!   - `run`    — `start`/`stop`/`kill`/`restart`/`pause`/`unpause` run-state control.
//!   - `manage` — `rename`/`wait`/`delete` (`docker rm`).
//!
//! Shared request helpers (parse_bind, parse_signal, do_stop, q_truthy) still live
//! in the parent `containers` module and are pulled in via `use super::super::*`.
//! Each submodule is re-exported with `pub(crate) use <sib>::*` so the public path
//! `crate::containers::<handler>` (router in main.rs + every `use crate::containers::*`
//! site) resolves exactly as before.
mod create;
mod manage;
mod run;

pub(crate) use create::*;
pub(crate) use manage::*;
pub(crate) use run::*;
