//! `cuEventCreate` / `cuEventRecord` / `cuStreamWaitEvent` / `cuEventSynchronize` / `cuEventQuery` /
//! `cuEventDestroy` — the cross-stream ordering primitives.
//!
//! The executor is synchronous (every prior submit has already completed when a driver call returns), so
//! an event is *complete* the instant it is recorded and `cuStreamWaitEvent` is an ordering point whose
//! dependency is already satisfied. Each entry here is therefore its faithful counterpart guarded by
//! honest handle validation — a bogus event or stream handle is a hard error ([`GpuError::Invalid`], the
//! `CUDA_ERROR_INVALID_VALUE`/`INVALID_HANDLE` analogue this crate returns for handle-validation failures),
//! never a silent success. When async streams land, `record`/`wait` gain timeline-fence signal/wait `Cmd`s;
//! the handle-validation contract here is unchanged.

use crate::model::context::CudaContext;
use crate::model::event::Event;
use crate::model::stream::Stream;
use hl_gpu::{GpuError, Result};

/// `cuEventCreate(flags)` — mint a fresh event handle. Infallible in the model (no resource pressure).
pub fn event_create(ctx: &mut CudaContext) -> Event {
    let e = ctx.events.create();
    hl_log::hl_debug!(hl_log::tag::CUDA, "event_create ev={}", e.0);
    e
}

/// `cuEventRecord(event, stream)` — capture the work outstanding on `stream` into `event`. Validates both
/// handles, then marks the event recorded. In the synchronous model the captured work has already
/// completed, so a recorded event is immediately complete.
pub fn event_record(ctx: &mut CudaContext, event: Event, stream: Stream) -> Result<()> {
    if !ctx.streams.is_valid(stream) {
        return Err(GpuError::Invalid("cuEventRecord: invalid stream handle"));
    }
    if !ctx.events.mark_recorded(event) {
        return Err(GpuError::Invalid("cuEventRecord: invalid event handle"));
    }
    hl_log::hl_debug!(hl_log::tag::CUDA, "event_record ev={} stream={}", event.0, stream.0);
    Ok(())
}

/// `cuStreamWaitEvent(stream, event, flags)` — make future work submitted to `stream` wait until `event`
/// completes. Validates both handles. Because the event's captured work has already completed in the
/// synchronous model, this is a validated ordering point that imposes no additional blocking — the
/// dependency is honored by construction. A never-recorded event is a benign no-op (as in real CUDA).
pub fn stream_wait_event(ctx: &CudaContext, stream: Stream, event: Event) -> Result<()> {
    if !ctx.streams.is_valid(stream) {
        return Err(GpuError::Invalid("cuStreamWaitEvent: invalid stream handle"));
    }
    if !ctx.events.is_valid(event) {
        return Err(GpuError::Invalid("cuStreamWaitEvent: invalid event handle"));
    }
    hl_log::hl_debug!(hl_log::tag::CUDA, "stream_wait_event stream={} ev={}", stream.0, event.0);
    Ok(())
}

/// `cuEventSynchronize(event)` — block the host until `event` completes. Validates the handle; in the
/// synchronous model the event is already complete once recorded, so this returns as soon as the handle
/// checks out. Errors on an unknown handle.
pub fn event_synchronize(ctx: &CudaContext, event: Event) -> Result<()> {
    if !ctx.events.is_valid(event) {
        return Err(GpuError::Invalid("cuEventSynchronize: invalid event handle"));
    }
    Ok(())
}

/// `cuEventQuery(event)` — is `event` complete? Returns `Ok(true)` when complete (recorded, hence done in
/// the synchronous model), `Ok(false)` for a live-but-never-recorded event (the `CUDA_ERROR_NOT_READY`
/// analogue), and errors on an unknown handle.
pub fn event_query(ctx: &CudaContext, event: Event) -> Result<bool> {
    if !ctx.events.is_valid(event) {
        return Err(GpuError::Invalid("cuEventQuery: invalid event handle"));
    }
    Ok(ctx.events.is_recorded(event))
}

/// `cuEventDestroy(event)` — release the event handle. Errors on an unknown / already-destroyed handle.
pub fn event_destroy(ctx: &mut CudaContext, event: Event) -> Result<()> {
    if ctx.events.destroy(event) {
        Ok(())
    } else {
        Err(GpuError::Invalid("cuEventDestroy: invalid event handle"))
    }
}
