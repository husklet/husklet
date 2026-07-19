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
use crate::images::*;
use crate::model::*;
use crate::prelude::*;
use crate::registry::Credentials;
use crate::util::*;
use hl_jit::Guest;

mod handler;
mod prune;
mod steps;

pub(crate) use handler::*;
pub(crate) use prune::*;
pub(in crate::build) use steps::*;

// Dockerfile parsing (`parse_dockerfile`/`substitute_args`/`parse_exec_form`/`parse_labels`) and the
// content-addressed build **layer cache** (`BuildCache`, `CacheId`, and `is_fs_inst`) and content digest
// values now live in `hl_images` — runtime-agnostic building primitives. This
// module keeps only the daemon-side orchestration: driving the step loop, running each `RUN` in the JIT,
// and registering the result as a daemon `Image`. Re-export the parsing helpers so `crate::build::*`
// call sites keep resolving.
pub(crate) use hl_images::build::{
    BuildCache, CacheId, Command as DockerCommand, Dockerfile, Instruction,
};
pub(crate) use hl_images::Sha256Digest;

// Pure `docker build` argument helpers (build-arg decode, store-dir sanitization), unit-tested apart
// from the stateful `images_build` step loop.
mod parse;
pub(crate) use parse::*;

pub(crate) struct BuildResponse;
impl BuildResponse {
    pub(crate) fn stream(lines: Vec<String>) -> Response {
        (
            StatusCode::OK,
            [("Content-Type", "application/json")],
            lines.join("\n") + "\n",
        )
            .into_response()
    }

    pub(crate) fn error(mut lines: Vec<String>, msg: String) -> Response {
        lines.push(json!({"errorDetail": {"message": msg.clone()}, "error": msg}).to_string());
        Self::stream(lines)
    }
}
