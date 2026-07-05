//! Image *building* — the runtime-agnostic half of `docker build`. This module owns everything that is
//! not "run a build step in a container": Dockerfile lexing/parsing ([`dockerfile`]) and the classic
//! build **layer cache** ([`cache`], snapshotting/restoring rootfs trees + step config). Executing a
//! `RUN` (which needs a runtime) is left to the caller, so `dd-images` keeps its no-runtime-dependency
//! contract: the daemon drives the build loop, calling these primitives and handing each `RUN` to
//! `dd-jit` itself.

pub mod cache;
pub mod dockerfile;

pub use cache::{cache_id, is_fs_inst, path_digest, rootfs_digest, sha256_hex, BuildCache};
pub use dockerfile::{parse_dockerfile, parse_env, parse_exec_form, parse_labels, substitute_args};
