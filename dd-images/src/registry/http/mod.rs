//! Thin `curl` wrappers plus small subprocess / header / base64 tools. Headers are captured to a temp
//! file (`-D`); the body goes to stdout (or a tar). The shelling-out is confined here — everything above
//! is ordinary typed code.

use super::*;

mod archive;
mod curl;
mod util;
mod verbs;

pub(super) use archive::*;
pub(super) use curl::*;
pub(super) use util::*;
pub(super) use verbs::*;
