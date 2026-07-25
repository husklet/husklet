//! `VkEvent` + `VkSemaphore` (binary/timeline) + `VkQueryPool` records, and the deferred device-op enum.
//!
//! Ported from `hl-shim-vk/src/reg.rs` (`EventRec`, `SemaphoreRec`, `QueryResult`, `QueryPoolRec`,
//! `DeferredOp`), themselves mirroring MoltenVK's `MVKEvent` / `MVKSemaphore` / `MVKTimelineSemaphore` /
//! `MVKQueryPool`. Pure owned value types: no `Cmd` is built here. The [`crate::service`] layer drives
//! them — host ops mutate directly, device ops (`vkCmd*`) are recorded into a command buffer as a
//! [`DeferredOp`] and applied at `vkQueueSubmit` completion (the host replay is synchronous, so a
//! device-set event / ended query is observably resolved once the submit returns).

use crate::{VkEvent, VkQueryPool};

/// A `VkEvent` — a guest-side boolean, created unsignaled. Host ops (`vkSetEvent`/`vkResetEvent`/
/// `vkGetEventStatus`) mutate/poll it directly; device ops (`vkCmdSetEvent`/`vkCmdResetEvent`) resolve
/// at submit completion. Mirrors `MVKEvent`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EventRec {
    pub signaled: bool,
}

/// A `VkSemaphore` — binary (present/acquire sync, bookkeeping-only for the synchronous executor) or
/// timeline (`VK_KHR_timeline_semaphore`, a monotonically increasing `counter` the host signals/waits/
/// polls). Mirrors `MVKSemaphore` / `MVKTimelineSemaphore`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SemaphoreRec {
    pub timeline: bool,
    /// The timeline counter (0 for a binary semaphore, or a timeline's initial value).
    pub counter: u64,
}

impl SemaphoreRec {
    /// A binary semaphore (the classic present/acquire kind).
    pub fn binary() -> Self {
        Self {
            timeline: false,
            counter: 0,
        }
    }
    /// A timeline semaphore starting at `initial` (`VkSemaphoreTypeCreateInfo::initialValue`).
    pub fn timeline(initial: u64) -> Self {
        Self {
            timeline: true,
            counter: initial,
        }
    }
}

/// One query slot's result. `available` gates `vkGetQueryPoolResults` (unavailable → `VK_NOT_READY`
/// unless `WAIT`/`PARTIAL`); `value` is the (bounded) synchronous result. Mirrors `MVKQueryPool` slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct QueryResult {
    pub available: bool,
    pub value: u64,
}

/// A `VkQueryPool` — a fixed array of typed query slots. Occlusion / pipeline-statistics results are a
/// bounded synchronous model (no real GPU sample counts → a conservative `0`); timestamps are a
/// host-monotonic serial. Availability + the read/copy machinery are real. Mirrors `MVKQueryPool`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QueryPoolRec {
    /// `VkQueryType` (raw): 0 = OCCLUSION, 1 = PIPELINE_STATISTICS, 2 = TIMESTAMP.
    pub query_type: i32,
    pub count: u32,
    pub results: Vec<QueryResult>,
}

impl QueryPoolRec {
    /// A pool of `count` unavailable/zero slots of `query_type`.
    pub fn new(query_type: i32, count: u32) -> Self {
        Self {
            query_type,
            count,
            results: vec![QueryResult::default(); count as usize],
        }
    }
}

/// A device-side event/query op recorded into a command buffer and applied at `vkQueueSubmit`
/// completion (the host replay is synchronous). Kept OUT of the `Vec<Enc>` encoder so the shipped
/// `Cmd::Submit` byte stream for an existing draw/dispatch is byte-for-byte unchanged. Ported from
/// `hl-shim-vk`'s `DeferredOp`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeferredOp {
    /// `vkCmdResetQueryPool` — clear `[first, first+count)` to unavailable/zero.
    QueryReset {
        pool: VkQueryPool,
        first: u32,
        count: u32,
    },
    /// `vkCmdEndQuery` (occlusion / pipeline-statistics) — mark the slot available with a bounded value.
    QueryEnd {
        pool: VkQueryPool,
        query: u32,
        value: u64,
    },
    /// `vkCmdWriteTimestamp` — mark the slot available with a host-monotonic timestamp serial.
    QueryTimestamp { pool: VkQueryPool, query: u32 },
    /// `vkCmdSetEvent` / `vkCmdResetEvent` — set/clear an event on completion.
    Event { event: VkEvent, set: bool },
    /// `vkCmdCopyQueryPoolResults` — on completion write the pool's `[first, first+count)` results into
    /// the destination buffer (an IR `WriteBuffer`) with the requested element size, stride and flags.
    CopyResults {
        pool: VkQueryPool,
        first: u32,
        count: u32,
        dst_ir: u32,
        dst_offset: u64,
        dst_size: u64,
        stride: u64,
        /// `VK_QUERY_RESULT_64_BIT`.
        wide: bool,
        /// `VK_QUERY_RESULT_WITH_AVAILABILITY_BIT`.
        with_availability: bool,
    },
}
