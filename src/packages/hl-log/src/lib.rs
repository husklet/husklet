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
//!     logging: tag::SYSCALL | tag::FS,
//!     level: hl_log::Level::Debug,
//!     profiling: tag::SYSCALL.into(),
//! }.apply();
//!
//! let (id, n) = (7u32, 4096usize);
//! hl_log::hl_info!(tag::SYSCALL, "dispatch {} ({} bytes)", id, n);
//! hl_log::hl_count!(tag::SYSCALL, "calls");
//! { let _s = hl_log::hl_span!(tag::FS, "resolve"); /* timed work */ }
//! ```
//!
//! Environment and CLI translation belongs to the application composition root.
//!
//! ## Conventions
//!
//! `println!`/`eprintln!`/`dbg!` are for tests, benchmarks, build scripts, and deliberate
//! program output — not diagnostics.
//!
//! - `Error`: the operation failed and it is not ordinary guest business. A guest-visible
//!   errno is not an error; it is the `Debug` outcome of its call.
//! - `Warn`: degraded, retried, backpressured, or refused by policy.
//! - `Info`: a lifecycle transition an operator wants in a normal session.
//! - `Debug`: one line per domain-boundary crossing, with operands and outcome.
//! - `Trace`: per-syscall, per-instruction, per-packet. Off in normal operation.
//!
//! Tag by the domain that owns the boundary, not the caller: a filesystem open reached
//! from the syscall router is [`tag::FS`]. See [`tag`] for what each one owns.
//!
//! Message shape is `subject verb key=value`, lowercase, `snake_case` keys, `{:#x}` for
//! guest addresses. Prefer [`hl_span!`] over logging a scope's start and end, and
//! [`hl_event!`] when the consumer is a machine.
//!
//! Never log from a signal, VEH, fault-entry, or fork-critical callback: the macros
//! format, may allocate, and may unwind. Record into a preallocated ring and report from
//! the owning coordinator once the callback has returned.

// ---- modules (one purpose each) -------------------------------------------------
mod channel;
mod config;
mod counters;
mod emit;
mod environment;
mod event;
mod level;
mod macros;
mod shard;
pub mod sink;
mod state;
pub mod tag;
mod timing;

// ---- re-exports: the flat public API -------------------------------------------

pub use channel::{Channel, Publish, ReceiveError, Receiver};
pub use config::Config;
pub use counters::Counters;
pub use emit::emit;
pub use environment::{ConfigurationWarning, EnvironmentConfig, LOG_LEVEL, LOG_TAGS, PROFILE_TAGS};
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
