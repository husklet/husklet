use super::{HostCapacityPlan, RuntimeAssembly, RuntimeAssemblyError, RuntimeDomain};
use crate::{CheckpointTaskRegistry, Control, SeccompControl};
use hl_event::EventCatalog;
use hl_network::{NetworkCatalog, NetworkConfiguration};
use hl_provider::{HandleNamespace, NamespaceLimits};
use hl_task::{CpuTopology, RegistryConfig, TaskRegistry};
use hl_vfs::AdvisoryLockCoordinator;
use std::sync::{Arc, Mutex};

impl RuntimeAssembly {
    pub fn new(config: HostCapacityPlan) -> Result<Self, RuntimeAssemblyError> {
        let topology =
            CpuTopology::new(1).map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::Task, &cause))?;
        Self::with_topology(config, topology)
    }

    pub fn with_topology(config: HostCapacityPlan, topology: CpuTopology) -> Result<Self, RuntimeAssemblyError> {
        let mut assembly = Self {
            tasks: None,
            epoll: None,
            descriptors: None,
            checkpoint_descriptors: None,
            events: None,
            event_resources: None,
            event_bindings: None,
            network: None,
            providers: None,
            provider_registry: None,
            seccomp: None,
            checkpoint: Mutex::new(None),
            checkpoint_pending: Mutex::new(Vec::new()),
            fork: None,
            exec: Arc::new(super::ExecSlot::new()),
            memory: Mutex::new(None),
            ipc: Mutex::new(None),
            ipc_pipes: Mutex::new(None),
            locks: Arc::new(AdvisoryLockCoordinator::new()),
            system: Arc::new(crate::SystemAuthority::default()),
        };
        assembly.tasks = Some(Arc::new(CheckpointTaskRegistry::new(Arc::new(
            TaskRegistry::new(RegistryConfig {
                max_processes: config.maximum_processes,
                max_threads: config.maximum_threads,
                max_groups: config.maximum_groups,
                max_pending_signals: config.maximum_pending_signals,
                online_cpus: topology.online(),
            })
            .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::Task, &cause))?,
        ))));
        let (epoll, descriptors) = Control::new(config.descriptor_limit, config.epoll_graph_limit)
            .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::DescriptorEvent, &cause))?;
        assembly.epoll = Some(Arc::new(epoll));
        let descriptors = Arc::new(descriptors);
        assembly.checkpoint_descriptors = Some(Arc::new(crate::CheckpointDescriptorTable::from_slot(
            descriptors.image_slot(),
        )));
        assembly.descriptors = Some(descriptors);
        let events = Arc::new(
            EventCatalog::new(config.event_capacity)
                .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::EventCatalog, &cause))?,
        );
        assembly.events = Some(Arc::new(crate::CheckpointEventCatalog::new(events)));
        let resources = Arc::new(crate::EventResourceRegistry::new());
        assembly.event_bindings = Some(Arc::new(crate::EventObjectBindings::new(
            Arc::new(crate::DescriptorObjectCatalog::rejecting()),
            resources.clone(),
        )));
        assembly.event_resources = Some(resources);
        let network = Arc::new(NetworkCatalog::new(
            NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new())
                .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::Network, &cause))?,
        ));
        assembly.network = Some(Arc::new(crate::CheckpointNetworkCatalog::new(network)));
        let providers = Arc::new(
            NamespaceLimits::new(config.provider_capacity)
                .and_then(HandleNamespace::with_limits)
                .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::Provider, &cause))?,
        );
        assembly.providers = Some(Arc::new(crate::CheckpointProviderNamespace::new(providers)));
        assembly.provider_registry = Some(Arc::new(crate::ProviderRegistry::new()));
        assembly.seccomp = Some(Arc::new(
            SeccompControl::new(config.maximum_seccomp_threads)
                .map_err(|cause| RuntimeAssemblyError::construction(RuntimeDomain::Seccomp, &cause))?,
        ));
        Ok(assembly)
    }
}
