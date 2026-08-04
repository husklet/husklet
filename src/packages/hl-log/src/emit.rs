//! Line formatting + dispatch to the sink.
//!
//! [`emit`] is only ever reached after [`crate::Logging::enabled`] passed, so it
//! is off the hot path — it may allocate a small `String` and format freely. The
//! line shape is:
//!
//! ```text
//! [tag] L +<ms> module:line: message
//! ```
//!
//! where `L` is the one-char level, `+<ms>` is millis since the first log call, and
//! `message` is the caller's `format_args!`. Everything is built into one `String`
//! and handed to the sink in a single `write_line`.

use crate::level::Level;
use crate::sink;
use crate::tag::Tags;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Instant;

/// The instant of the first emit, used as the origin for relative timestamps.
static START: OnceLock<Instant> = OnceLock::new();

/// Millis since the first log line was emitted.
#[inline]
pub(crate) fn millis_since_start() -> u128 {
    START.get_or_init(Instant::now).elapsed().as_millis()
}

/// Format and dispatch one log line. Reached only when the gate passed.
///
/// Not `inline` — keeping the cold formatting path out of the caller keeps the hot
/// (disabled) call site tiny.
pub fn emit(tags: Tags, level: Level, module: &str, line: u32, args: std::fmt::Arguments) {
    let mut buf = String::with_capacity(96);
    buf.push('[');
    let _ = write!(buf, "{tags}");
    buf.push_str("] ");
    buf.push(level.short());
    // Relative timestamp + thread id, kept compact.
    let _ = write!(buf, " +{}ms t{} ", millis_since_start(), thread_id());
    buf.push_str(module);
    buf.push(':');
    let _ = write!(buf, "{}", line);
    buf.push_str(": ");
    let _ = buf.write_fmt(args);
    buf.push('\n');
    sink::Output::global().write(&buf);
}

/// A cheap, stable-per-thread numeric id. `ThreadId`'s `Debug` is `ThreadId(N)`;
/// we extract just the number for a compact `tN` field.
pub(crate) fn thread_id() -> u64 {
    thread_local! {
        static ID: u64 = derive_thread_id();
    }
    ID.with(|v| *v)
}

fn derive_thread_id() -> u64 {
    // Parse the integer out of the `ThreadId(N)` debug form. Falls back to 0.
    let dbg = format!("{:?}", std::thread::current().id());
    dbg.trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .parse()
        .unwrap_or(0)
}
