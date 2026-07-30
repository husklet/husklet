//! Timeline fences. Ordinary submissions remain asynchronous, but a command buffer carrying a signal waits
//! for its preceding queue work before raising this host-side high-water mark. A later wait succeeds iff the
//! completion value was already signalled. No cross-process GPU semaphore is claimed (capabilities advertise
//! `supports_timeline_fences: false`).

use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Operations on one timeline fence stored in session resources.
pub struct Fence;
pub struct State {
    scheduled: u64,
    completed: Arc<AtomicU64>,
}

impl Fence {
    pub fn state() -> State {
        State {
            scheduled: 0,
            completed: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn schedule(res: &mut SessionResources, id: u32, value: u64) -> Result<Arc<AtomicU64>> {
        let state = res
            .fences
            .get_mut(id)?
            .downcast_mut::<State>()
            .ok_or(GpuError::Invalid("wgpu: fence native type mismatch"))?;
        state.scheduled = state.scheduled.max(value);
        Ok(state.completed.clone())
    }

    /// Return the signalled high-water value of fence `id`.
    pub fn value(res: &SessionResources, id: u32) -> Result<u64> {
        res.fences
            .get(id)?
            .downcast_ref::<State>()
            .map(|state| state.completed.load(Ordering::Acquire))
            .ok_or(GpuError::Invalid("wgpu: fence native type mismatch"))
    }

    pub fn scheduled(res: &SessionResources, id: u32) -> Result<u64> {
        res.fences
            .get(id)?
            .downcast_ref::<State>()
            .map(|state| state.scheduled)
            .ok_or(GpuError::Invalid("wgpu: fence native type mismatch"))
    }

    /// Raise fence `id` to `max(current, value)`.
    pub fn signal(slot: &AtomicU64, value: u64) {
        slot.fetch_max(value, Ordering::Release);
    }
}
