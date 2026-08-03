//! CUDA external timeline semaphore import, signal, wait and destruction.

use hl_gpu::{CommandSink, GpuError, Result, SyncExportId, TimelineWait};

use crate::model::external_semaphore::ExternalSemaphore;
use crate::model::stream::Stream;
use crate::CudaContext;

pub fn import(
    context: &mut CudaContext,
    sink: &mut dyn CommandSink,
    export: SyncExportId,
) -> Result<ExternalSemaphore> {
    sink.import_sync(export)?;
    Ok(context.external_semaphores.insert(export))
}

pub fn destroy(
    context: &mut CudaContext,
    sink: &mut dyn CommandSink,
    semaphore: ExternalSemaphore,
) -> Result<()> {
    let export = context
        .external_semaphores
        .remove(semaphore)
        .ok_or(GpuError::Invalid("unknown CUDA external semaphore"))?;
    sink.release_sync(export)
}

pub fn signal(
    context: &CudaContext,
    sink: &mut dyn CommandSink,
    semaphore: ExternalSemaphore,
    value: u64,
    stream: Stream,
) -> Result<()> {
    if !context.streams.is_valid(stream) {
        return Err(GpuError::Invalid("unknown CUDA stream"));
    }
    let export = context
        .external_semaphores
        .export(semaphore)
        .ok_or(GpuError::Invalid("unknown CUDA external semaphore"))?;
    sink.signal_sync(export, value)
}

pub fn wait(
    context: &CudaContext,
    sink: &mut dyn CommandSink,
    semaphore: ExternalSemaphore,
    value: u64,
    stream: Stream,
) -> Result<()> {
    if !context.streams.is_valid(stream) {
        return Err(GpuError::Invalid("unknown CUDA stream"));
    }
    let export = context
        .external_semaphores
        .export(semaphore)
        .ok_or(GpuError::Invalid("unknown CUDA external semaphore"))?;
    match sink.wait_sync(export, value, u64::MAX)? {
        TimelineWait::Reached => Ok(()),
        TimelineWait::Timeout => Err(GpuError::Invalid("external semaphore wait timed out")),
    }
}
