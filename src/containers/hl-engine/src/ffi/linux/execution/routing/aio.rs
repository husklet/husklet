use std::sync::Arc;

use super::super::process_memory::ProcessMemory;

pub(super) fn runtime(
    context: &super::ProcessContext,
    descriptors: Arc<hl_descriptor::DescriptorTable>,
    memory: ProcessMemory,
    cancellation: Arc<dyn hl_descriptor::OperationCancellation>,
) -> hl_runtime::RuntimeAioSyscalls<ProcessMemory> {
    hl_runtime::RuntimeAioSyscalls::new(
        Arc::clone(&context.aio),
        descriptors,
        memory,
        context.architecture,
        cancellation,
    )
}
