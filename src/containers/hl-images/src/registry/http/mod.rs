//! Thin `curl` wrappers plus small subprocess / header / base64 tools. Headers are captured to a temp
//! file (`-D`); the body goes to stdout (or a tar). The shelling-out is confined here — everything above
//! is ordinary typed code.

use super::*;

mod curl;
mod verbs;

pub(super) use curl::*;
pub(super) use verbs::*;
