//! CUDA **streams** — the ordered submission queues `cuStreamCreate`/`cuStreamSynchronize` name.
//!
//! The current lowering is synchronous (every submit completes before the driver call returns), so a
//! stream carries no reordering yet; the table exists to (a) validate a `CUstream` handle the guest
//! passes back, and (b) give [`crate::service::synchronize`] a real object to barrier on. The default
//! stream is id [`StreamTable::DEFAULT`] and is always present. Async ordering across streams is a
//! deferred hardening pass (it needs the executor's timeline fences fully wired).

use std::collections::HashSet;

/// A CUDA stream handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Stream(pub u32);

/// The per-context stream table: the live non-default stream ids + a monotonic id counter. The default
/// stream ([`StreamTable::DEFAULT`]) is implicit and always valid.
#[derive(Debug)]
pub struct StreamTable {
    live: HashSet<u32>,
    next_id: u32,
}

impl Default for StreamTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTable {
    /// The implicit default stream (CUDA's stream `0` / `NULL` stream).
    pub const DEFAULT: Stream = Stream(0);

    pub fn new() -> Self {
        // ids start at 1 so the default stream (0) is never minted as an explicit stream.
        Self {
            live: HashSet::new(),
            next_id: 1,
        }
    }

    /// `cuStreamCreate` — mint and register a new stream.
    pub fn create(&mut self) -> Stream {
        let id = self.next_id;
        self.next_id += 1;
        self.live.insert(id);
        Stream(id)
    }

    /// Is `s` a usable stream handle (the default stream, or a live created one)?
    pub fn is_valid(&self, s: Stream) -> bool {
        s == Self::DEFAULT || self.live.contains(&s.0)
    }

    /// `cuStreamDestroy` — returns `false` if the handle was never live / already destroyed / the
    /// (non-destroyable) default stream.
    pub fn destroy(&mut self, s: Stream) -> bool {
        s != Self::DEFAULT && self.live.remove(&s.0)
    }
}
