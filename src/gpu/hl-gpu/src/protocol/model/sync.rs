//! Neutral cross-session synchronization identities and wait results.

/// Process-global authenticated synchronization identity. Serial values are monotonic and never reused;
/// authenticity makes a guest-local opaque-fd carrier unforgeable by guessing the serial alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SyncExportId {
    serial: u64,
    authenticity: u128,
}

impl SyncExportId {
    pub fn from_parts(serial: u64, authenticity: u128) -> Self {
        Self {
            serial,
            authenticity,
        }
    }

    pub fn serial(self) -> u64 {
        self.serial
    }

    pub fn authenticity(self) -> u128 {
        self.authenticity
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineWait {
    Reached,
    Timeout,
}
