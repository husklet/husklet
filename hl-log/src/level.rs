//! Log severity levels.
//!
//! Levels are ordered by numeric severity: `Error = 1` (most severe) through
//! `Trace = 5` (least severe). The global `MIN_LEVEL` gate lets a message through
//! when `(level as u8) <= MIN_LEVEL`, i.e. a `MIN_LEVEL` of `Info (3)` admits
//! Error/Warn/Info but suppresses Debug/Trace.

/// A log severity level. The `u8` repr is the wire value stored in `MIN_LEVEL`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    /// Single-character tag used in emitted lines: `E W I D T`.
    #[inline]
    pub const fn short(self) -> char {
        match self {
            Level::Error => 'E',
            Level::Warn => 'W',
            Level::Info => 'I',
            Level::Debug => 'D',
            Level::Trace => 'T',
        }
    }

    /// Lowercase name, as accepted by `HL_LOG_LEVEL`.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    /// Parse a level from its lowercase name (case-insensitive). Returns `None`
    /// for unknown strings.
    pub fn from_name(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "err" | "1" => Some(Level::Error),
            "warn" | "warning" | "2" => Some(Level::Warn),
            "info" | "3" => Some(Level::Info),
            "debug" | "dbg" | "4" => Some(Level::Debug),
            "trace" | "5" => Some(Level::Trace),
            _ => None,
        }
    }

    /// Reconstruct a `Level` from its stored `u8`. Out-of-range values clamp to
    /// the nearest valid level (0 -> Error, >5 -> Trace).
    #[inline]
    pub const fn from_u8(v: u8) -> Level {
        match v {
            0 | 1 => Level::Error,
            2 => Level::Warn,
            3 => Level::Info,
            4 => Level::Debug,
            _ => Level::Trace,
        }
    }
}
