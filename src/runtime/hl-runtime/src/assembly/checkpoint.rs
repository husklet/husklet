use std::sync::Arc;

use hl_checkpoint::{CheckpointSink, CheckpointSource, ImageLimits};
use hl_memory::MappingHost;
use hl_network::NetworkCheckpointRebind;

use crate::{
    CheckpointRole, DescriptorEventRebind, EventCheckpointParticipant, EventWireCodec, IpcCheckpointParticipant,
    IpcResourceRebind, NetworkCheckpointParticipant, PortableIpcCodec, PortableNetworkCodec, PortableProviderCodec,
    ProviderCheckpointParticipant, RuntimeCheckpointCoordinator,
};

use super::{AssemblyCheckpointError, RuntimeAssembly, RuntimeAssemblyError, RuntimeDomain};

impl RuntimeAssembly {
    #[must_use]
    pub fn has_checkpoint_role(&self, role: CheckpointRole) -> bool {
        self.checkpoint_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|participant| participant.role() == role)
    }

    pub fn prepare_network_checkpoint(
        &self,
        rebind: Arc<dyn NetworkCheckpointRebind>,
    ) -> Result<(), RuntimeAssemblyError> {
        self.prepare_checkpoint(Arc::new(NetworkCheckpointParticipant::new(
            self.checkpoint_network(),
            rebind,
            Arc::new(PortableNetworkCodec),
        )))
    }

    pub fn prepare_event_checkpoint(&self) -> Result<(), RuntimeAssemblyError> {
        self.prepare_checkpoint(Arc::new(EventCheckpointParticipant::new(
            self.checkpoint_events(),
            Arc::new(DescriptorEventRebind::new(
                self.checkpoint_descriptors(),
                self.event_bindings(),
            )),
            Arc::new(EventWireCodec),
        )))
    }

    pub fn prepare_provider_checkpoint(&self) -> Result<(), RuntimeAssemblyError> {
        let registry = self.provider_registry();
        self.prepare_checkpoint(Arc::new(ProviderCheckpointParticipant::new(
            self.checkpoint_providers(),
            registry.clone(),
            registry,
            Arc::new(PortableProviderCodec),
        )))
    }

    pub fn prepare_ipc_checkpoint<H: MappingHost + 'static>(
        &self,
        memory: Arc<crate::CheckpointMemoryState<H>>,
    ) -> Result<(), RuntimeAssemblyError> {
        let catalog = self.checkpoint_ipc().ok_or_else(Self::checkpoint_error)?;
        let pipes = self.ipc_pipes().ok_or_else(Self::checkpoint_error)?.bindings();
        let rebind = Arc::new(IpcResourceRebind::new(memory, pipes, self.checkpoint_tasks()));
        self.prepare_checkpoint(Arc::new(IpcCheckpointParticipant::new(
            catalog,
            rebind,
            Arc::new(PortableIpcCodec),
        )))
    }

    pub fn install_checkpoint(
        &self,
        coordinator: Arc<RuntimeCheckpointCoordinator>,
    ) -> Result<(), RuntimeAssemblyError> {
        let mut current = self.checkpoint.lock().unwrap_or_else(|error| error.into_inner());
        if current.is_some() {
            return Err(Self::checkpoint_error());
        }
        *current = Some(coordinator);
        Ok(())
    }

    pub fn prepare_checkpoint(
        &self,
        participant: Arc<dyn crate::CheckpointParticipant>,
    ) -> Result<(), RuntimeAssemblyError> {
        if self.checkpoint().is_some() {
            return Err(Self::checkpoint_error());
        }
        let mut pending = self.checkpoint_pending.lock().map_err(|_| Self::checkpoint_error())?;
        let ordered = pending.last().is_none_or(|last| last.role() < participant.role());
        let dependencies_ready = participant
            .dependencies()
            .iter()
            .all(|dependency| pending.iter().any(|value| value.role() == *dependency));
        if !ordered || !dependencies_ready {
            return Err(Self::checkpoint_error());
        }
        pending.push(participant);
        Ok(())
    }

    pub fn finalize_checkpoint(&self, limits: ImageLimits) -> Result<(), RuntimeAssemblyError> {
        let participants = {
            let mut pending = self.checkpoint_pending.lock().map_err(|_| Self::checkpoint_error())?;
            std::mem::take(&mut *pending)
        };
        let coordinator =
            RuntimeCheckpointCoordinator::new(participants.clone(), limits).map_err(|_| Self::checkpoint_error());
        match coordinator {
            Ok(coordinator) => self.install_checkpoint(Arc::new(coordinator)),
            Err(error) => {
                *self
                    .checkpoint_pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = participants;
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn checkpoint(&self) -> Option<Arc<RuntimeCheckpointCoordinator>> {
        self.checkpoint
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn checkpoint_ready(&self, coordinator: &RuntimeCheckpointCoordinator) -> Result<(), AssemblyCheckpointError> {
        let roles = coordinator.roles();
        for (role, domain) in [
            (CheckpointRole::Task, RuntimeDomain::Task),
            (CheckpointRole::Descriptors, RuntimeDomain::DescriptorEvent),
            (CheckpointRole::Event, RuntimeDomain::EventCatalog),
            (CheckpointRole::Provider, RuntimeDomain::Provider),
            (CheckpointRole::Network, RuntimeDomain::Network),
        ] {
            if !roles.contains(&role) {
                return Err(AssemblyCheckpointError::Unsupported(domain));
            }
        }
        if self.memory().is_some() && !roles.contains(&CheckpointRole::Memory) {
            return Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Memory));
        }
        if self.ipc().is_some() && !roles.contains(&CheckpointRole::Ipc) {
            return Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Ipc));
        }
        if self.exec().is_some() && !roles.contains(&CheckpointRole::Execution) {
            return Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Execution));
        }
        if self.fork().is_some() {
            return Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Fork));
        }
        let locks = self.locks.snapshot();
        if !locks.flocks.is_empty() || !locks.ranges.is_empty() {
            return Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Linux));
        }
        Ok(())
    }

    pub fn capture_checkpoint<S: CheckpointSink>(&self, sink: &mut S) -> Result<(), AssemblyCheckpointError> {
        let coordinator = self.checkpoint().ok_or(AssemblyCheckpointError::Missing)?;
        self.checkpoint_ready(&coordinator)?;
        coordinator
            .checkpoint(sink)
            .map_err(AssemblyCheckpointError::Transaction)
    }

    pub fn restore_checkpoint<S: CheckpointSource>(&self, source: &mut S) -> Result<(), AssemblyCheckpointError> {
        let coordinator = self.checkpoint().ok_or(AssemblyCheckpointError::Missing)?;
        self.checkpoint_ready(&coordinator)?;
        coordinator
            .restore(source)
            .map_err(AssemblyCheckpointError::Transaction)
    }

    const fn checkpoint_error() -> RuntimeAssemblyError {
        RuntimeAssemblyError::Construction(RuntimeDomain::Checkpoint)
    }
}
