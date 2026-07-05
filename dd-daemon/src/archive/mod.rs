use crate::model::*;
use crate::util::*;
use crate::prelude::*;

// ===================== docker cp — the /archive tar endpoints =====================
// `docker cp` tars a path out of (GET) / into (PUT) the container filesystem; HEAD returns a path-stat the
// CLI uses to pick file-vs-dir semantics. We operate on the container's rootfs (the overlay upper) -- the
// common case -- except paths under a bind, which `archive_host_path` redirects to the host bind source.

mod handlers;
mod overlay;

pub(crate) use {handlers::*, overlay::*};
