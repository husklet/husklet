//! Global runtime state + the hot-path gate.
//!
//! Process-wide logging and profiling configuration defaults to off, so a build
//! that never applies a [`crate::Config`] does no logging work
//! beyond the relaxed loads in [`Logging::enabled`].

use crate::level::Level;
use crate::tag::Tags;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};

/// Process-wide logging configuration and hot-path gate.
pub struct Logging {
    enabled: AtomicU64,
    level: AtomicU8,
}

static LOGGING: Logging = Logging {
    enabled: AtomicU64::new(0),
    level: AtomicU8::new(Level::Warn as u8),
};

impl Logging {
    #[inline(always)]
    #[must_use]
    pub fn global() -> &'static Self {
        &LOGGING
    }

    #[inline(always)]
    pub fn enabled(&self, tags: Tags, level: Level) -> bool {
        self.enabled.load(Relaxed) & tags.bits() != 0 && (level as u8) <= self.level.load(Relaxed)
    }

    pub fn enable(&self, tags: impl Into<Tags>) {
        self.enabled.fetch_or(tags.into().bits(), Relaxed);
    }

    pub fn disable(&self, tags: impl Into<Tags>) {
        self.enabled.fetch_and(!tags.into().bits(), Relaxed);
    }

    pub fn set(&self, tags: impl Into<Tags>) {
        self.enabled.store(tags.into().bits(), Relaxed);
    }

    pub fn set_level(&self, level: Level) {
        self.level.store(level as u8, Relaxed);
    }

    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Relaxed))
    }

    pub fn tags(&self) -> Tags {
        Tags::from_bits(self.enabled.load(Relaxed))
    }
}

/// Process-wide tag gate shared by counters and timing spans.
pub struct Profiling {
    enabled: AtomicU64,
}

static PROFILING: Profiling = Profiling {
    enabled: AtomicU64::new(0),
};

impl Profiling {
    #[inline(always)]
    #[must_use]
    pub fn global() -> &'static Self {
        &PROFILING
    }

    #[inline(always)]
    pub fn enabled(&self, tags: Tags) -> bool {
        self.enabled.load(Relaxed) & tags.bits() != 0
    }

    pub fn enable(&self, tags: impl Into<Tags>) {
        self.enabled.fetch_or(tags.into().bits(), Relaxed);
    }

    pub fn disable(&self, tags: impl Into<Tags>) {
        self.enabled.fetch_and(!tags.into().bits(), Relaxed);
    }

    pub fn set(&self, tags: impl Into<Tags>) {
        self.enabled.store(tags.into().bits(), Relaxed);
    }

    pub fn tags(&self) -> Tags {
        Tags::from_bits(self.enabled.load(Relaxed))
    }
}
