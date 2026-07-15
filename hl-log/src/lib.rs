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
//!    [`enabled`]: one relaxed atomic load + AND + a predicted-not-taken branch. With
//!    `HL_LOG` unset the enabled mask is 0, so a live call site costs ~a couple ns and
//!    NEVER evaluates its arguments (`format_args!` is inside the `if`).
//!
//! ## Quick start
//! ```
//! use hl_log::tag;
//! hl_log::init(); // optional; first macro use auto-inits from env
//! hl_log::enable(tag::GPU | tag::WGPU);
//! hl_log::set_level(hl_log::Level::Debug);
//!
//! let (id, n) = (7u32, 4096usize);
//! hl_log::hl_info!(tag::GPU, "submit frame {} ({} bytes)", id, n);
//! hl_log::hl_count!(tag::GPU, "frames");
//! { let _s = hl_log::hl_span!(tag::WGPU, "readback"); /* timed work */ }
//! ```
//!
//! ## Environment
//! - `HL_LOG` — comma-separated tag names, or `all`, or `off`/empty. Sets the enabled mask.
//! - `HL_LOG_LEVEL` — `error|warn|info|debug|trace`. Minimum level (default `warn`).
//! - `HL_LOG_COUNTERS` — tag names / `all` / `off`. Enables counters + timing per tag.

// ---- modules (one purpose each) -------------------------------------------------
pub mod counters;
mod emit;
mod init;
mod level;
mod macros;
mod shard;
pub mod sink;
mod state;
pub mod tag;
pub mod timing;

// ---- re-exports: the flat public API -------------------------------------------

pub use emit::emit;
pub use init::{init, parse_tag_mask};
pub use level::Level;
pub use sink::{reset_sink, set_sink, Sink, StderrSink};
pub use state::{
    counters_enabled, counters_mask, disable, disable_counters, enable, enable_counters, enabled,
    enabled_mask, level, set_counters, set_enabled, set_level,
};

// Counter / timing surface, re-exported at the crate root for ergonomics.
pub use counters::{counters_dump, counters_reset, counters_snapshot};
pub use timing::{timing_dump, timing_reset, timing_snapshot, Span, TimingStat};

/// Auto-init hook called by the macros on first use. Public + hidden because macro
/// expansions in other crates must be able to name it. Warm cost: one relaxed load.
#[doc(hidden)]
#[inline]
pub fn ensure_init() {
    state::ensure_init();
}

/// Flush all profiling to the active sink: counters table followed by timing table.
/// Safe to call anytime — snapshots under brief per-shard locks, then writes with NO
/// lock held. Does not reset; call [`counters_reset`]/[`timing_reset`] for that.
pub fn flush() {
    counters::counters_dump();
    timing::timing_dump();
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
