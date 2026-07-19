//! Image *building* — the runtime-agnostic half of `docker build`. This module owns everything that is
//! not "run a build step in a container": Dockerfile lexing/parsing ([`dockerfile`]) and the classic
//! build **layer cache** ([`cache`], snapshotting/restoring rootfs trees + step config). Executing a
//! `RUN` (which needs a runtime) is left to the caller, so `hl-images` keeps its no-runtime-dependency
//! contract: the daemon drives the build loop, calling these primitives and handing each `RUN` to
//! `hl-jit` itself.

pub mod cache;
pub mod dockerfile;

pub use cache::{BuildCache, CacheId};
pub use dockerfile::{Command, Dockerfile, Instruction};
