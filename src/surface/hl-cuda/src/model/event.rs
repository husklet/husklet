//! CUDA **events** — the lightweight markers `cuEventCreate` / `cuEventRecord` / `cuStreamWaitEvent` /
//! `cuEventSynchronize` / `cuEventQuery` name to express cross-stream ordering.
//!
//! With the current lowering every submit completes before the driver call returns (the executor is
//! synchronous), so an event is COMPLETE the instant it is recorded, and a `cuStreamWaitEvent` is a
//! validated ordering point whose dependency has already been satisfied. The table still models the event
//! object faithfully:
//!   * a handle is validated on every use — a bogus / already-destroyed handle is a hard error
//!     (the `CUDA_ERROR_INVALID_VALUE`/`INVALID_HANDLE` analogue), never a silent success, and
//!   * an event tracks whether it has been *recorded*, so `cuEventQuery` answers truthfully
//!     (a never-recorded event is trivially complete in CUDA; a recorded one is complete here because
//!     the work it captured has already run).
//!
//! Once async streams land (a deferred hardening pass, like the fence-backed stream ordering) the same
//! object model carries real record→wait dependencies; the only change is that `record`/`wait` would
//! emit timeline-fence signal/wait `Cmd`s instead of relying on synchronous completion.

use std::collections::HashSet;

/// A CUDA event handle (`CUevent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Event(pub u32);

/// The per-context event table: live event ids + the subset that have been recorded, plus a monotonic id
/// counter. Ids start at 1 so a zero handle is always invalid.
#[derive(Debug)]
pub struct EventTable {
    live: HashSet<u32>,
    recorded: HashSet<u32>,
    next_id: u32,
}

impl Default for EventTable {
    fn default() -> Self {
        Self::new()
    }
}

impl EventTable {
    pub fn new() -> Self {
        Self {
            live: HashSet::new(),
            recorded: HashSet::new(),
            next_id: 1,
        }
    }

    /// `cuEventCreate` — mint and register a fresh (un-recorded) event.
    pub fn create(&mut self) -> Event {
        let id = self.next_id;
        self.next_id += 1;
        self.live.insert(id);
        Event(id)
    }

    /// Is `e` a live event handle?
    pub fn is_valid(&self, e: Event) -> bool {
        self.live.contains(&e.0)
    }

    /// Mark a live event as recorded (`cuEventRecord`). Returns `false` for an unknown handle.
    pub fn mark_recorded(&mut self, e: Event) -> bool {
        if self.live.contains(&e.0) {
            self.recorded.insert(e.0);
            true
        } else {
            false
        }
    }

    /// Has `e` been recorded? In the synchronous model a recorded event is also *complete*.
    pub fn is_recorded(&self, e: Event) -> bool {
        self.recorded.contains(&e.0)
    }

    /// `cuEventDestroy` — drop the event. Returns `false` if the handle was never live / already destroyed.
    pub fn destroy(&mut self, e: Event) -> bool {
        self.recorded.remove(&e.0);
        self.live.remove(&e.0)
    }
}
