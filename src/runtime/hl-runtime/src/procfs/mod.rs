mod cpu;
mod descriptor;
mod memory;
mod mount;
mod network;
mod resource;
mod source;
mod stat;
#[cfg(test)]
mod test;

pub use cpu::{CpuPolicy, CpuPort};
pub use descriptor::DescriptorTarget;
pub use memory::MemoryPort;
pub use mount::MountPort;
pub use network::NetworkPort;
pub use resource::ResourcePort;
pub use stat::{StatMetrics, StatPort};
use std::sync::Arc;

use hl_descriptor::DescriptorTable;
use hl_task::{ProcessId, ProcessLifecycle, Resource, TaskRegistry, ThreadId};
use hl_vfs::{
    ProcfsCpuModel, ProcfsError, ProcfsLimitResource, ProcfsLimitView, ProcfsProcessIdentity, ProcfsProcessState,
    ProcfsProcessView, ProcfsThreadIdentity,
};
/// Task-owned producer for typed process-filesystem views.
pub struct TaskProcfs {
    tasks: Arc<TaskRegistry>,
    current: Option<ProcessId>,
    descriptors: Option<Arc<DescriptorTable>>,
    targets: Option<Arc<dyn DescriptorTarget>>,
    resources: Option<Arc<dyn ResourcePort>>,
    system: Option<Arc<crate::SystemAuthority>>,
    root: Option<Vec<u8>>,
    working: Option<Arc<crate::WorkingDirectory>>,
    executable: Option<Vec<u8>>,
    fs_context: Option<Arc<crate::FsContext>>,
    stat: Option<Arc<dyn StatPort>>,
    memory: Option<Arc<dyn MemoryPort>>,
    mounts: Option<Arc<dyn MountPort>>,
    network: Option<Arc<dyn NetworkPort>>,
    cpu: Option<Arc<dyn CpuPort>>,
    cpu_model: ProcfsCpuModel,
    seccomp: Option<Arc<crate::SeccompControl>>,
    seccomp_baseline: hl_linux::SeccompBaseline,
}

impl TaskProcfs {
    fn process_id(&self, identity: ProcfsProcessIdentity) -> Result<ProcessId, ProcfsError> {
        let id = ProcessId::from_wire(identity.slot(), identity.generation()).ok_or(ProcfsError::NotFound)?;
        self.tasks
            .process_snapshot(id)
            .map(|_| id)
            .map_err(|_| ProcfsError::NotFound)
    }

    fn thread_id(&self, identity: ProcfsThreadIdentity) -> Result<ThreadId, ProcfsError> {
        ThreadId::from_wire(identity.slot(), identity.generation()).ok_or(ProcfsError::NotFound)
    }

    #[must_use]
    pub fn new(tasks: Arc<TaskRegistry>) -> Self {
        Self {
            tasks,
            current: None,
            descriptors: None,
            targets: None,
            resources: None,
            system: None,
            root: None,
            working: None,
            executable: None,
            fs_context: None,
            stat: None,
            memory: None,
            mounts: None,
            network: None,
            cpu: None,
            cpu_model: ProcfsCpuModel::Aarch64 {
                hardware: 0,
                hardware_second: 0,
            },
            seccomp: None,
            seccomp_baseline: hl_linux::SeccompBaseline::Container,
        }
    }

    #[must_use]
    pub fn with_descriptors(
        tasks: Arc<TaskRegistry>,
        current: ProcessId,
        descriptors: Arc<DescriptorTable>,
        targets: Arc<dyn DescriptorTarget>,
    ) -> Self {
        Self {
            tasks,
            current: Some(current),
            descriptors: Some(descriptors),
            targets: Some(targets),
            resources: None,
            system: None,
            root: None,
            working: None,
            executable: None,
            fs_context: None,
            stat: None,
            memory: None,
            mounts: None,
            network: None,
            cpu: None,
            cpu_model: ProcfsCpuModel::Aarch64 {
                hardware: 0,
                hardware_second: 0,
            },
            seccomp: None,
            seccomp_baseline: hl_linux::SeccompBaseline::Container,
        }
    }

    #[must_use]
    pub fn with_seccomp(mut self, seccomp: Arc<crate::SeccompControl>, baseline: hl_linux::SeccompBaseline) -> Self {
        self.seccomp = Some(seccomp);
        self.seccomp_baseline = baseline;
        self
    }

    #[must_use]
    pub fn with_system(mut self, system: Arc<crate::SystemAuthority>) -> Self {
        self.system = Some(system);
        self
    }

    #[must_use]
    pub fn with_resources(mut self, resources: Arc<dyn ResourcePort>) -> Self {
        self.resources = Some(resources);
        self
    }

    #[must_use]
    pub fn with_cpu_model(mut self, model: ProcfsCpuModel) -> Self {
        self.cpu_model = model;
        self
    }

    #[must_use]
    pub fn with_cpu(mut self, cpu: Arc<dyn CpuPort>) -> Self {
        self.cpu = Some(cpu);
        self
    }

    #[must_use]
    pub fn with_root(mut self, root: Vec<u8>) -> Self {
        self.root = Some(root);
        self
    }

    #[must_use]
    pub fn with_executable(mut self, executable: Vec<u8>) -> Self {
        self.executable = Some(executable);
        self
    }

    #[must_use]
    pub fn with_working(mut self, working: Arc<crate::WorkingDirectory>) -> Self {
        self.working = Some(working);
        self
    }

    #[must_use]
    pub fn with_fs_context(mut self, context: Arc<crate::FsContext>) -> Self {
        self.fs_context = Some(context);
        self
    }

    #[must_use]
    pub fn with_stat(mut self, stat: Arc<dyn StatPort>) -> Self {
        self.stat = Some(stat);
        self
    }

    #[must_use]
    pub fn with_memory(mut self, memory: Arc<dyn MemoryPort>) -> Self {
        self.memory = Some(memory);
        self
    }

    #[must_use]
    pub fn with_mounts(mut self, mounts: Arc<dyn MountPort>) -> Self {
        self.mounts = Some(mounts);
        self
    }

    #[must_use]
    pub fn with_network(mut self, network: Arc<dyn NetworkPort>) -> Self {
        self.network = Some(network);
        self
    }

    fn view(&self, identity: ProcfsProcessIdentity) -> Result<ProcfsProcessView, ProcfsError> {
        let id = self.process_id(identity)?;
        let registry = self.tasks.snapshot();
        let snapshot = registry
            .processes
            .iter()
            .find(|candidate| candidate.id == id)
            .ok_or(ProcfsError::NotFound)?;
        let leader = registry
            .threads
            .iter()
            .find(|thread| thread.id == snapshot.leader)
            .ok_or(ProcfsError::Invalid)?;
        let credentials = &snapshot.credentials;
        let affinity = self.tasks.affinity(snapshot.leader).map_err(|_| ProcfsError::Invalid)?;
        let pending_signals = snapshot
            .signals
            .pending
            .iter()
            .chain(&leader.signals.pending)
            .fold(0_u64, |mask, signal| mask | (1_u64 << (signal.signal.get() - 1)));
        let (ignored_signals, caught_signals) = snapshot.signals.actions.iter().fold(
            (0_u64, 0_u64),
            |(ignored, caught), (signal, action): &(hl_task::SignalNumber, hl_task::SignalAction)| {
                let bit = 1_u64 << (signal.get() - 1);
                match action.disposition {
                    hl_task::SignalDisposition::Default => (ignored, caught),
                    hl_task::SignalDisposition::Ignore => (ignored | bit, caught),
                    hl_task::SignalDisposition::Handler(_) => (ignored, caught | bit),
                }
            },
        );
        let memory = self.memory_view(snapshot.id)?;
        let seccomp = self
            .seccomp
            .as_ref()
            .map_or_else(
                || Ok(self.seccomp_baseline.status()),
                |control| control.status(snapshot.leader, self.seccomp_baseline),
            )
            .map_err(|_| ProcfsError::Invalid)?;
        Ok(ProcfsProcessView {
            process: id.number(),
            parent: snapshot.parent.map_or(0, hl_task::ProcessId::number),
            name: leader.name,
            state: self.process_state(snapshot.lifecycle, leader.lifecycle),
            threads: snapshot.threads.len(),
            umask: self
                .current
                .filter(|current| *current == id)
                .and_then(|_| self.fs_context.as_ref().map(|context| context.mask())),
            real_user: credentials.real_user,
            effective_user: credentials.effective_user,
            saved_user: credentials.saved_user,
            filesystem_user: credentials.filesystem_user,
            real_group: credentials.real_group,
            effective_group: credentials.effective_group,
            saved_group: credentials.saved_group,
            filesystem_group: credentials.filesystem_group,
            groups: credentials.supplementary_groups().to_vec(),
            inheritable: credentials.capabilities.inheritable,
            permitted: credentials.capabilities.permitted,
            effective: credentials.capabilities.effective,
            bounding: credentials.capability_bounding,
            ambient: credentials.capabilities.ambient,
            no_new_privileges: credentials.no_new_privileges,
            seccomp_mode: match seccomp.mode {
                hl_linux::SeccompMode::Disabled => 0,
                hl_linux::SeccompMode::Strict => 1,
                hl_linux::SeccompMode::Filter => 2,
            },
            seccomp_filters: seccomp.filters,
            pending_signals,
            blocked_signals: leader.signals.mask.bits(),
            ignored_signals,
            caught_signals,
            limits: snapshot
                .limits
                .iter()
                .map(|(resource, limit)| ProcfsLimitView {
                    resource: self.limit_resource(*resource),
                    soft: limit.soft,
                    hard: limit.hard,
                })
                .collect(),
            allowed_mask: affinity.mask_text(),
            allowed_list: affinity.list_text(),
            memory,
        })
    }

    const fn process_state(&self, process: ProcessLifecycle, leader: hl_task::ThreadLifecycle) -> ProcfsProcessState {
        match process {
            ProcessLifecycle::Starting | ProcessLifecycle::Running => match leader {
                hl_task::ThreadLifecycle::Blocked => ProcfsProcessState::Sleeping,
                hl_task::ThreadLifecycle::Starting
                | hl_task::ThreadLifecycle::Runnable
                | hl_task::ThreadLifecycle::Exiting => ProcfsProcessState::Running,
            },
            ProcessLifecycle::Stopped => ProcfsProcessState::Stopped,
            ProcessLifecycle::Exiting => ProcfsProcessState::Exiting,
            ProcessLifecycle::Zombie => ProcfsProcessState::Zombie,
        }
    }

    const fn limit_resource(&self, resource: Resource) -> ProcfsLimitResource {
        match resource {
            Resource::CpuTime => ProcfsLimitResource::CpuTime,
            Resource::FileSize => ProcfsLimitResource::FileSize,
            Resource::Data => ProcfsLimitResource::Data,
            Resource::Stack => ProcfsLimitResource::Stack,
            Resource::Core => ProcfsLimitResource::Core,
            Resource::ResidentSet => ProcfsLimitResource::ResidentSet,
            Resource::Processes => ProcfsLimitResource::Processes,
            Resource::OpenFiles => ProcfsLimitResource::OpenFiles,
            Resource::LockedMemory => ProcfsLimitResource::LockedMemory,
            Resource::AddressSpace => ProcfsLimitResource::AddressSpace,
            Resource::Locks => ProcfsLimitResource::Locks,
            Resource::PendingSignals => ProcfsLimitResource::PendingSignals,
            Resource::MessageQueue => ProcfsLimitResource::MessageQueue,
            Resource::Nice => ProcfsLimitResource::Nice,
            Resource::RealtimePriority => ProcfsLimitResource::RealtimePriority,
            Resource::RealtimeTime => ProcfsLimitResource::RealtimeTime,
        }
    }
}
