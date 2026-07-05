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
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::{Container as JitContainer, Error as JitError, Guest, Image, PortMap, Runtime as JitRuntime, SpawnConfig, Stdio3, Volume};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch, Mutex};

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
