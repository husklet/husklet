//! # hl-log
//!
//! Foundational **tag-based logging + counters + timing** for the whole app. std-only,
//! zero external dependencies, and designed around two independent "off = free" axes:
//!
//! 1. **Compile-time (build profile).** In `--release` the verbose levels
//!    (`hl_warn!`/`hl_info!`/`hl_debug!`/`hl_trace!`) and all profiling
//!    (`hl_count!`/`hl_add!`/`hl_span!`) expand to `{}` and produce no code — the
//!    branches, the `format_args!`, and the argument expressions are all removed. Only
//!    `hl_error!` (the one level you always want in release) and `hl_log!` (runtime
//!    level) survive. Opt back into full logging in release with the `release-verbose`
//!    feature; turn EVERYTHING off (including error) with the `disabled` feature.
//!
//! 2. **Runtime (the gate).** In debug builds every macro is present but fronted by
//!    [`Logging::enabled`]: one relaxed atomic load + AND + a predicted-not-taken branch. With
//!    the default configuration the enabled mask is 0, so a live call site costs ~a couple ns and
//!    NEVER evaluates its arguments (`format_args!` is inside the `if`).
//!
//! ## Quick start
//! ```
//! use hl_log::tag;
//! hl_log::Config {
//!     logging: tag::GPU | tag::WGPU,
//!     level: hl_log::Level::Debug,
//!     profiling: tag::GPU.into(),
//! }.apply();
//!
//! let (id, n) = (7u32, 4096usize);
//! hl_log::hl_info!(tag::GPU, "submit frame {} ({} bytes)", id, n);
//! hl_log::hl_count!(tag::GPU, "frames");
//! { let _s = hl_log::hl_span!(tag::WGPU, "readback"); /* timed work */ }
//! ```
//!
//! Environment and CLI translation belongs to the application composition root.

// ---- modules (one purpose each) -------------------------------------------------
mod config;
mod counters;
mod emit;
mod event;
mod level;
mod macros;
mod shard;
pub mod sink;
mod state;
pub mod tag;
mod timing;

// ---- re-exports: the flat public API -------------------------------------------

pub use config::Config;
pub use counters::Counters;
pub use emit::emit;
pub use event::{emit_event, emit_verdict, emit_verdict_with, Value};
pub use level::Level;
pub use sink::{DiscardSink, Events, Output, Sink, StderrSink};
pub use state::{Logging, Profiling};
pub use tag::{Tag, TagList, Tags};
pub use timing::{Span, TimingStat, Timings};

/// Flush all profiling to the active sink: counters table followed by timing table.
/// Safe to call anytime — snapshots under brief per-shard locks, then writes with NO
/// lock held. Does not reset the collections.
pub fn flush() {
    Counters::global().dump();
    Timings::global().dump();
}

/// Whether verbose logging (`hl_warn!`..`hl_trace!`) is compiled into THIS build.
/// `true` in debug or with `release-verbose`; `false` in a default release build or
/// with `disabled`.
pub const VERBOSE_COMPILED: bool = cfg!(all(
    not(feature = "disabled"),
    any(debug_assertions, feature = "release-verbose")
));

/// Whether ANY logging (at least `hl_error!`) is compiled into THIS build. `false`
/// only with the `disabled` feature.
pub const LOG_COMPILED: bool = cfg!(not(feature = "disabled"));
