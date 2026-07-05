#![allow(unused_imports, dead_code)]
//! `docker build` orchestration — a minimal Dockerfile builder, decomposed by concern:
//!   - `handler` — the `POST /build` axum handler (`images_build`): request/`--build-arg`/`--target`
//!                 parsing, the multi-stage step-loop driver, the build layer-cache chain, and final
//!                 image registration + progress/response streaming.
//!   - `steps`   — the per-instruction executors extracted from the loop (RUN via the JIT, COPY/ADD)
//!                 plus the cache-descriptor helper.
//!   - `prune`   — `build_prune` (`docker builder prune`) and `commit_container` (`docker commit`).
//!
//! Not BuildKit: we copy the base image's rootfs, run each RUN in the JIT (writes persist in the new
//! rootfs), COPY from the build context, track ENV/WORKDIR/CMD/ENTRYPOINT, and register the result as an
//! image. Reuses pull (base must be local), the JIT spawn, the archive path-mapper, and image registration.
//!
//! Every previously-public name stays reachable as `crate::build::…` via the glob re-exports below, so
//! the router in main.rs and every `use crate::build::*` site keep resolving unchanged.
use crate::archive::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::runtime::*;
use crate::system::*;
use crate::util::*;
use crate::volumes::*;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::{Guest, PortMap, SpawnConfig, Volume};
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

mod handler;
mod prune;
mod steps;

pub(crate) use handler::*;
pub(crate) use prune::*;
pub(crate) use steps::*;

// Dockerfile parsing (`parse_dockerfile`/`substitute_args`/`parse_exec_form`/`parse_labels`) and the
// content-addressed build **layer cache** (`BuildCache`, `cache_id`, `is_fs_inst`, the rootfs/path
// digests + `sha256_hex`) now live in `dd_images::build` — runtime-agnostic building primitives. This
// module keeps only the daemon-side orchestration: driving the step loop, running each `RUN` in the JIT,
// and registering the result as a daemon `Image`. Re-export the parsing helpers so `crate::build::*`
// call sites keep resolving.
pub(crate) use dd_images::build::{
    cache_id, is_fs_inst, parse_dockerfile, parse_exec_form, parse_labels, path_digest,
    rootfs_digest, sha256_hex, substitute_args, BuildCache,
};

pub(crate) fn build_stream(lines: Vec<String>) -> Response {
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        lines.join("\n") + "\n",
    )
        .into_response()
}

pub(crate) fn build_err(mut lines: Vec<String>, msg: String) -> Response {
    lines.push(json!({"errorDetail": {"message": msg.clone()}, "error": msg}).to_string());
    build_stream(lines)
}
