use hl_checkpoint::{MemorySink, MemorySource};
use hl_runtime::{AssemblyCheckpointError, RuntimeAssembly};

use crate::composition::RuntimeServices;
use crate::engine::EngineError;
use crate::launch_plan::RuntimeLaunchPlan;

const MAXIMUM_IMAGE_BYTES: usize = 512 * 1024 * 1024;

pub(super) struct Operation<'a> {
    plan: &'a RuntimeLaunchPlan,
    assembly: &'a RuntimeAssembly,
    services: &'a RuntimeServices,
}

impl<'a> Operation<'a> {
    pub(super) fn new(
        plan: &'a RuntimeLaunchPlan,
        assembly: &'a RuntimeAssembly,
        services: &'a RuntimeServices,
    ) -> Self {
        Self {
            plan,
            assembly,
            services,
        }
    }

    pub(super) fn apply(&self) -> Result<(), EngineError> {
        if self.plan.options.get("HL_RESTORE").is_some() {
            self.restore()?;
        }
        if self.plan.options.get("HL_CHECKPOINT").is_some() {
            self.capture()?;
        }
        Ok(())
    }

    fn restore(&self) -> Result<(), EngineError> {
        let transport = self
            .services
            .checkpoint_source
            .as_ref()
            .ok_or(EngineError::Unsupported)?;
        let bytes = transport
            .read(MAXIMUM_IMAGE_BYTES + 1)
            .map_err(|_| EngineError::LaunchFailed)?;
        if bytes.len() > MAXIMUM_IMAGE_BYTES {
            return Err(EngineError::LaunchFailed);
        }
        let mut source = MemorySource::new(bytes);
        self.assembly
            .restore_checkpoint(&mut source)
            .map_err(|error| match error {
                AssemblyCheckpointError::Missing | AssemblyCheckpointError::Unsupported(_) => EngineError::Unsupported,
                AssemblyCheckpointError::Transaction(_) => EngineError::LaunchFailed,
            })
    }

    fn capture(&self) -> Result<(), EngineError> {
        let transport = self.services.checkpoint_sink.as_ref().ok_or(EngineError::Unsupported)?;
        let mut sink = MemorySink::new();
        self.assembly
            .capture_checkpoint(&mut sink)
            .map_err(|error| match error {
                AssemblyCheckpointError::Missing | AssemblyCheckpointError::Unsupported(_) => EngineError::Unsupported,
                AssemblyCheckpointError::Transaction(_) => EngineError::LaunchFailed,
            })?;
        transport
            .replace(sink.committed().ok_or(EngineError::LaunchFailed)?)
            .map_err(|_| EngineError::LaunchFailed)
    }
}
