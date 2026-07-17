//! Pure request/spec parsers shared across the container handlers: bind mounts, stop signals,
//! query truthiness, and the published-port grammar (`parse_publish` + the two JSON shapers).
//! These are side-effect-free string→value transforms with no async/`App` dependency, split out
//! from `mod.rs` so the async handlers (`do_stop`) and these parsers live apart. Re-exported by
//! `mod.rs` via `pub(crate) use parse::*`, so `crate::containers::<fn>` resolves unchanged.
//!
//! Split by concern:
//!   - `config` — `parse_bind`, `parse_signal`, `q_truthy` (container-config request bits)
//!   - `ports`  — `parse_publish`, `ports_json`, `ports_map_json` (published-port parsing)
use super::*;

mod config;
mod ports;
pub(crate) use config::*;
pub(crate) use ports::*;
