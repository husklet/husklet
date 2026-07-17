//! Crate-internal prelude: the third-party + std leaf imports shared by nearly every
//! handler module. Glob-import this (`use crate::prelude::*;`) to collapse the otherwise
//! duplicated ~18-line import header. Only names that never collide with crate business
//! types live here — notably `hl_jit::{Guest, PortMap, SpawnConfig, Volume}` is deliberately
//! kept per-file (its `Volume` can shadow-ambiguate against `crate::volumes::*`).
pub(crate) use axum::body::Body;
pub(crate) use axum::extract::{Path, Query, Request, State};
pub(crate) use axum::http::StatusCode;
pub(crate) use axum::response::{IntoResponse, Response};
pub(crate) use axum::Json;
pub(crate) use hyper_util::rt::TokioIo;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::{json, Value};
pub(crate) use std::collections::HashMap;
pub(crate) use std::os::fd::RawFd;
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::Arc;
pub(crate) use std::time::{SystemTime, UNIX_EPOCH};
pub(crate) use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub(crate) use tokio::sync::{broadcast, mpsc, watch, Mutex};
