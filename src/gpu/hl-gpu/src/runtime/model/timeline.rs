//! [`FenceTimeline`] — the per-connection timeline-fence state the runtime owns.
//!
//! A submitted [`CommandBuffer`](crate::protocol::model::command::CommandBuffer) may signal a fence to a
//! monotonically-increasing timeline value on completion; a `WaitFence` blocks until a fence reaches a
//! value. The runtime tracks the highest signaled value per fence here (stamped with the injected
//! [`Clock`](crate::runtime::port::clock::Clock)), so pacing/synchronization is testable without a GPU.
//! A signal that moves a fence backwards is a typed error, never a silent regression.

use std::collections::HashMap;

use crate::protocol::model::error::{GpuError, Result};

/// The last-signaled state of one timeline fence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FenceState {
    /// Highest timeline value signaled so far (monotonic).
    pub value: u64,
    /// Clock timestamp (ns) of the most recent signal — pacing/diagnostics.
    pub signaled_at_nanos: u64,
}

/// Per-connection fence timeline: `FenceId → last-signaled state`.
#[derive(Clone, Debug, Default)]
pub struct FenceTimeline {
    fences: HashMap<u32, FenceState>,
}

impl FenceTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-created fence at value 0 (idempotent — a re-create keeps its value).
    pub fn register(&mut self, id: u32) {
        self.fences.entry(id).or_default();
    }

    /// Forget a destroyed fence.
    pub fn retire(&mut self, id: u32) {
        self.fences.remove(&id);
    }

    /// Record a signal of fence `id` to timeline `value` at clock time `now`. A value below the current
    /// high-water mark is rejected — a timeline fence must never run backwards.
    pub fn signal(&mut self, id: u32, value: u64, now: u64) -> Result<()> {
        let st = self.fences.entry(id).or_default();
        if value < st.value {
            return Err(GpuError::Invalid("fence timeline value moved backwards"));
        }
        st.value = value;
        st.signaled_at_nanos = now;
        Ok(())
    }

    /// The highest value fence `id` has been signaled to, if it is live.
    pub fn get(&self, id: u32) -> Option<u64> {
        self.fences.get(&id).map(|s| s.value)
    }

    /// True if fence `id` has already reached `value` (a wait for it would not block).
    pub fn is_reached(&self, id: u32, value: u64) -> bool {
        self.fences
            .get(&id)
            .map(|s| s.value >= value)
            .unwrap_or(false)
    }

    /// Number of live fences.
    pub fn len(&self) -> usize {
        self.fences.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fences.is_empty()
    }
}
