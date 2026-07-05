#![allow(unused_imports, dead_code)]
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::system::*;
use crate::util::*;
use crate::volumes::*;
use crate::prelude::*;
use ddjit::{Container as JitContainer, Error as JitError, Guest, Image, PortMap, Runtime as JitRuntime, SpawnConfig, Stdio3, Volume};

/// Cap on the retained `docker logs` replay buffer (per container/exec). A chatty or long-lived guest
/// would otherwise grow `live.log_chunks` without bound and OOM the daemon. When a new chunk pushes the
/// buffer over this, the oldest chunks are dropped from the front — standard log-rotation behavior, so
/// `docker logs` shows the most-recent ≤ 8 MiB of output.
const LOG_CHUNKS_CAP_BYTES: usize = 8 * 1024 * 1024;

mod health;
mod restart;
mod spawn;

pub(crate) use health::*;
pub(crate) use restart::*;
pub(crate) use spawn::*;
