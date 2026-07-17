//! Timeline fences. This executor drives the device synchronously — every submit is followed by a
//! `device.poll(Wait)` in the transfer/readback paths — so a fence is just a host-side high-water mark
//! (the same shape the CPU oracle uses): a submit signals it to `max(current, value)`, and a wait
//! succeeds iff the value was already signalled. No cross-process GPU semaphore is claimed (capabilities
//! advertise `supports_timeline_fences: false`).

use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

/// The signalled high-water value of fence `id`.
pub fn value(res: &SessionResources, id: u32) -> Result<u64> {
    res.fences
        .get(id)?
        .downcast_ref::<u64>()
        .copied()
        .ok_or(GpuError::Invalid("wgpu: fence native type mismatch"))
}

/// Raise fence `id` to `max(current, v)` (a submit-completion signal).
pub fn signal(res: &mut SessionResources, id: u32, v: u64) -> Result<()> {
    let slot = res
        .fences
        .get_mut(id)?
        .downcast_mut::<u64>()
        .ok_or(GpuError::Invalid("wgpu: fence native type mismatch"))?;
    *slot = (*slot).max(v);
    Ok(())
}
