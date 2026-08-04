use std::sync::Arc;

use hl_memory::{ExitMappingHost, MappingCoordinator, PreparedAddressExit};
use hl_task::{ProcessId, ThreadId};

use crate::{ExitParticipant, ExitRuntimeError, PreparedExitParticipant};

pub struct Exit<H: ExitMappingHost> {
    memory: Arc<MappingCoordinator<H>>,
}

impl<H: ExitMappingHost> Exit<H> {
    pub fn new(memory: Arc<MappingCoordinator<H>>) -> Self {
        Self { memory }
    }
}

impl<H: ExitMappingHost + 'static> ExitParticipant for Exit<H>
where
    H::PreparedExit: 'static,
{
    fn prepare(&self, _: ProcessId, _: &[ThreadId]) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        self.memory
            .prepare_exit()
            .map(|prepared| Box::new(prepared) as Box<_>)
            .map_err(|_| ExitRuntimeError::Failed)
    }
}

impl<H: ExitMappingHost> PreparedExitParticipant for PreparedAddressExit<H> {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        PreparedAddressExit::publish(self).map_err(|_| ExitRuntimeError::Failed)
    }

    fn rollback(&mut self) {
        let _ = PreparedAddressExit::rollback(self);
    }

    fn finish(&mut self) {
        PreparedAddressExit::finish(self);
    }
}
