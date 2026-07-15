//! Initialization: env parsing + programmatic setup.
//!
//! `init()` reads three env vars once and applies them to the global state. It is
//! idempotent and re-callable (tests reconfigure by setting env then re-calling).
//! First use of any macro auto-calls the env init via `state::ensure_init`.
//!
//! - `HL_LOG`          — comma-separated tag names, or `all`, or `off`/empty.
//! - `HL_LOG_LEVEL`    — `error|warn|info|debug|trace`.
//! - `HL_LOG_COUNTERS` — tag names / `all` / `off` (also gates timing spans).

use crate::level::Level;
use crate::state;
use crate::tag;

/// Explicit initialization. Parses the environment and applies it. Safe to call
/// repeatedly; each call re-reads the env and overwrites the masks/level. Marks the
/// auto-init `Once` done so first-macro-use does not re-parse afterward.
pub fn init() {
    init_from_env();
    state::mark_auto_init_done();
}

/// Parse a `HL_LOG`-style tag list into a mask. `all` -> ALL, `off`/empty -> 0,
/// otherwise OR of every recognized comma-separated name. Unknown names are ignored.
pub fn parse_tag_mask(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return tag::NONE;
    }
    let lower = s.to_ascii_lowercase();
    if lower == "all" {
        return tag::ALL;
    }
    if lower == "off" || lower == "none" {
        return tag::NONE;
    }
    let mut mask = 0u64;
    for part in lower.split(|c| c == ',' || c == '|' || c == ' ') {
        if let Some(bit) = tag::from_name(part) {
            mask |= bit;
        }
    }
    mask
}

/// Core env-driven configuration, shared by `init()` and the auto-init path.
pub(crate) fn init_from_env() {
    if let Ok(v) = std::env::var("HL_LOG") {
        state::set_enabled(parse_tag_mask(&v));
    }
    if let Ok(v) = std::env::var("HL_LOG_LEVEL") {
        if let Some(level) = Level::from_name(&v) {
            state::set_level(level);
        }
    }
    if let Ok(v) = std::env::var("HL_LOG_COUNTERS") {
        state::set_counters(parse_tag_mask(&v));
    }
}
