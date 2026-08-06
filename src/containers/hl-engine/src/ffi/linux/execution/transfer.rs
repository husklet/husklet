use hl_checkpoint::{MemorySink, MemorySource};
use hl_runtime::{AssemblyCheckpointError, RuntimeAssembly};

use super::diagnostics::{LaunchError, LaunchPhase};
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

    pub(super) fn apply(&self) -> Result<(), LaunchError> {
        if self.plan.options.get("HL_RESTORE").is_some() {
            self.restore()?;
        }
        if self.plan.options.get("HL_CHECKPOINT").is_some() {
            self.capture()?;
        }
        Ok(())
    }

    fn restore(&self) -> Result<(), LaunchError> {
        let transport = self
            .services
            .checkpoint_source
            .as_ref()
            .ok_or(EngineError::Unsupported)?;
        let bytes = transport
            .read(MAXIMUM_IMAGE_BYTES + 1)
            .map_err(|_| LaunchError::phase(LaunchPhase::Transfer))?;
        if bytes.len() > MAXIMUM_IMAGE_BYTES {
            return Err(LaunchError::phase(LaunchPhase::Transfer));
        }
        let mut source = MemorySource::new(bytes);
        self.assembly
            .restore_checkpoint(&mut source)
            .map_err(|error| match error {
                AssemblyCheckpointError::Missing | AssemblyCheckpointError::Unsupported(_) => {
                    LaunchError::from(EngineError::Unsupported)
                }
                AssemblyCheckpointError::Transaction(_) => LaunchError::phase(LaunchPhase::Transfer),
            })
    }

    fn capture(&self) -> Result<(), LaunchError> {
        let transport = self.services.checkpoint_sink.as_ref().ok_or(EngineError::Unsupported)?;
        let mut sink = MemorySink::new();
        self.assembly
            .capture_checkpoint(&mut sink)
            .map_err(|error| match error {
                AssemblyCheckpointError::Missing | AssemblyCheckpointError::Unsupported(_) => {
                    LaunchError::from(EngineError::Unsupported)
                }
                AssemblyCheckpointError::Transaction(_) => LaunchError::phase(LaunchPhase::Transfer),
            })?;
        transport
            .replace(
                sink.committed()
                    .ok_or_else(|| LaunchError::phase(LaunchPhase::Transfer))?,
            )
            .map_err(|_| LaunchError::phase(LaunchPhase::Transfer))
    }
}
