mod cpu;
mod descriptor;
mod memory;
mod mount;
mod network;
mod resource;
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
    ProcfsCgroupView, ProcfsCpuModel, ProcfsCpuView, ProcfsDescriptorView, ProcfsError, ProcfsLimitResource,
    ProcfsLimitView, ProcfsProcessIdentity, ProcfsProcessState, ProcfsProcessView, ProcfsSource, ProcfsStatView,
    ProcfsSystemView, ProcfsThreadIdentity, ProcfsUtsView,
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

impl ProcfsSource for TaskProcfs {
    fn resolve_process(&self, process: u32) -> Result<ProcfsProcessIdentity, ProcfsError> {
        let id = self.tasks.process_by_number(process).ok_or(ProcfsError::NotFound)?;
        let (slot, generation) = id.wire_parts();
        ProcfsProcessIdentity::new(slot, generation).ok_or(ProcfsError::NotFound)
    }

    fn resolve_thread(
        &self,
        process: ProcfsProcessIdentity,
        thread: Option<u32>,
    ) -> Result<ProcfsThreadIdentity, ProcfsError> {
        let process = self.process_id(process)?;
        let snapshot = self.tasks.snapshot();
        let leader = snapshot
            .processes
            .iter()
            .find(|candidate| candidate.id == process)
            .ok_or(ProcfsError::NotFound)?
            .leader;
        let id = match thread {
            None => leader,
            Some(number) => snapshot
                .threads
                .iter()
                .find(|candidate| candidate.process == process && candidate.id.number() == number)
                .map(|candidate| candidate.id)
                .ok_or(ProcfsError::NotFound)?,
        };
        let (slot, generation) = id.wire_parts();
        ProcfsThreadIdentity::new(slot, generation).ok_or(ProcfsError::NotFound)
    }

    fn network(&self, process: ProcfsProcessIdentity) -> Result<hl_vfs::ProcfsNetworkView, ProcfsError> {
        self.view(process)?;
        self.network
            .as_ref()
            .map(|network| network.view())
            .ok_or(ProcfsError::NotFound)
    }
    fn processes(&self) -> Result<Vec<u32>, ProcfsError> {
        let mut processes = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .map(|process| process.id.number())
            .collect::<Vec<_>>();
        processes.sort_unstable();
        Ok(processes)
    }

    fn threads(&self, process: ProcfsProcessIdentity) -> Result<Vec<u32>, ProcfsError> {
        self.view(process)?;
        let process = self.process_id(process)?;
        let mut threads = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == process)
            .map(|thread| thread.id.number())
            .collect::<Vec<_>>();
        threads.sort_unstable();
        Ok(threads)
    }

    fn root(&self, process: ProcfsProcessIdentity) -> Result<Vec<u8>, ProcfsError> {
        self.view(process)?;
        self.root.clone().ok_or(ProcfsError::NotFound)
    }

    fn cwd(&self, process: ProcfsProcessIdentity) -> Result<Vec<u8>, ProcfsError> {
        let id = self.process_id(process)?;
        let working = match &self.resources {
            Some(resources) => resources.working(id)?,
            None if self.current == Some(id) => Arc::clone(self.working.as_ref().ok_or(ProcfsError::NotFound)?),
            None => return Err(ProcfsError::NotFound),
        };
        let snapshot = working.snapshot();
        let mut path = snapshot.path.as_str().as_bytes().to_vec();
        if snapshot.deleted {
            path.extend_from_slice(b" (deleted)");
        }
        Ok(path)
    }

    fn process(&self, process: ProcfsProcessIdentity) -> Result<ProcfsProcessView, ProcfsError> {
        self.view(process)
    }

    fn oom_score_adj(&self, process: ProcfsProcessIdentity) -> Result<i16, ProcfsError> {
        self.tasks
            .process_snapshot(self.process_id(process)?)
            .map(|snapshot| snapshot.oom_score_adj)
            .map_err(|_| ProcfsError::NotFound)
    }

    fn write_oom_score_adj(
        &self,
        process: ProcfsProcessIdentity,
        _actor: hl_descriptor::OperationActor,
        value: i16,
    ) -> Result<(), hl_descriptor::ObjectError> {
        let process = self
            .process_id(process)
            .map_err(|_| hl_descriptor::ObjectError::Retired)?;
        self.tasks
            .set_oom_score_adj(process, value)
            .map_err(|_| hl_descriptor::ObjectError::InvalidArgument)
    }

    fn cmdline(&self, process: ProcfsProcessIdentity) -> Result<Vec<u8>, ProcfsError> {
        let snapshot = self
            .tasks
            .process_snapshot(self.process_id(process)?)
            .map_err(|_| ProcfsError::NotFound)?;
        let capacity = snapshot.arguments.iter().map(|argument| argument.len() + 1).sum();
        let mut bytes = Vec::with_capacity(capacity);
        for argument in snapshot.arguments {
            bytes.extend_from_slice(&argument);
            bytes.push(0);
        }
        Ok(bytes)
    }

    fn stat(&self, process: ProcfsProcessIdentity) -> Result<ProcfsStatView, ProcfsError> {
        self.stat_view(self.process_id(process)?)
    }

    fn memory(&self, process: ProcfsProcessIdentity) -> Result<hl_vfs::ProcfsMemoryView, ProcfsError> {
        let id = self.process_id(process)?;
        self.memory.as_ref().ok_or(ProcfsError::NotFound)?.sample(id)
    }

    fn address_space(&self, process: ProcfsProcessIdentity) -> Result<hl_vfs::ProcfsAddressSpaceView, ProcfsError> {
        let id = self.process_id(process)?;
        self.memory.as_ref().ok_or(ProcfsError::NotFound)?.address_space(id)
    }

    fn environment(&self, process: ProcfsProcessIdentity) -> Result<Vec<u8>, ProcfsError> {
        let id = self.process_id(process)?;
        self.memory.as_ref().ok_or(ProcfsError::NotFound)?.environment(id)
    }

    fn comm(&self, process: ProcfsProcessIdentity, thread: ProcfsThreadIdentity) -> Result<Vec<u8>, ProcfsError> {
        let process_id = self.process_id(process)?;
        let thread_id = self.thread_id(thread)?;
        let registry = self.tasks.snapshot();
        let process = registry
            .processes
            .iter()
            .find(|candidate| candidate.id == process_id)
            .ok_or(ProcfsError::NotFound)?;
        let name = if process.leader == thread_id {
            &process.name
        } else {
            &registry
                .threads
                .iter()
                .find(|candidate| candidate.process == process_id && candidate.id == thread_id)
                .ok_or(ProcfsError::NotFound)?
                .name
        };
        let mut name = name.split(|byte| *byte == 0).next().unwrap_or(&[]).to_vec();
        name.push(b'\n');
        Ok(name)
    }

    fn write_comm(
        &self,
        process: ProcfsProcessIdentity,
        thread: ProcfsThreadIdentity,
        actor: hl_descriptor::OperationActor,
        bytes: &[u8],
    ) -> Result<(), hl_descriptor::ObjectError> {
        if bytes.len() > 15 {
            return Err(hl_descriptor::ObjectError::InvalidArgument);
        }
        let actor_process = ProcessId::from_wire(actor.process, actor.process_generation)
            .ok_or(hl_descriptor::ObjectError::PermissionDenied)?;
        let actor_thread = hl_task::ThreadId::from_wire(actor.thread, actor.thread_generation)
            .ok_or(hl_descriptor::ObjectError::PermissionDenied)?;
        let process_id = self
            .process_id(process)
            .map_err(|_| hl_descriptor::ObjectError::Retired)?;
        if process_id != actor_process {
            return Err(hl_descriptor::ObjectError::PermissionDenied);
        }
        self.tasks
            .snapshot()
            .threads
            .iter()
            .find(|candidate| candidate.id == actor_thread && candidate.process == actor_process)
            .ok_or(hl_descriptor::ObjectError::PermissionDenied)?;
        let target_id = self
            .thread_id(thread)
            .map_err(|_| hl_descriptor::ObjectError::Retired)?;
        let snapshot = self.tasks.snapshot();
        let target = snapshot
            .threads
            .iter()
            .find(|candidate| candidate.process == process_id && candidate.id == target_id)
            .ok_or(hl_descriptor::ObjectError::Retired)?;
        let mut name = [0_u8; 16];
        name[..bytes.len()].copy_from_slice(bytes);
        self.tasks
            .set_name(target.id, name)
            .map_err(|_| hl_descriptor::ObjectError::Retired)
    }

    fn cpu(&self) -> Result<ProcfsCpuView, ProcfsError> {
        let topology = self.tasks.topology();
        let view = ProcfsCpuView::new(topology.online(), self.cpu_model.clone()).ok_or(ProcfsError::Invalid)?;
        Ok(self
            .cpu
            .as_ref()
            .map_or(view.clone(), |cpu| view.with_ticks(cpu.ticks(topology.online()))))
    }

    fn cgroup(&self) -> Result<ProcfsCgroupView, ProcfsError> {
        // Registry capacities bound both vectors before cgroup rendering allocates output.
        let registry = self.tasks.snapshot();
        let resources = self.system.as_ref().map(|system| system.snapshot()).unwrap_or_default();
        let processes = registry
            .processes
            .into_iter()
            .map(|process| process.id.number())
            .collect();
        let threads = registry.threads.into_iter().map(|thread| thread.id.number()).collect();
        ProcfsCgroupView::new(
            self.tasks.topology().online(),
            resources.cpu_limit,
            (resources.total_memory != 0).then_some(resources.total_memory),
            resources.total_memory.saturating_sub(resources.free_memory),
            processes,
            threads,
        )
        .ok_or(ProcfsError::Invalid)
    }

    fn system(&self) -> Result<ProcfsSystemView, ProcfsError> {
        let value = self.system.as_ref().ok_or(ProcfsError::NotFound)?.snapshot();
        let (total_memory, free_memory) = value.visible_memory();
        Ok(ProcfsSystemView {
            uptime_seconds: value.uptime_seconds,
            process_creations: value.process_creations,
            total_memory,
            free_memory,
        })
    }

    fn boot_identity(&self) -> Result<[u8; 16], ProcfsError> {
        Ok(self.system.as_ref().ok_or(ProcfsError::NotFound)?.boot_identity())
    }

    fn random_identity(&self) -> Result<[u8; 16], ProcfsError> {
        self.system
            .as_ref()
            .ok_or(ProcfsError::NotFound)?
            .random_identity()
            .map_err(|_| ProcfsError::Invalid)
    }

    fn uts(&self, process: ProcfsProcessIdentity) -> Result<ProcfsUtsView, ProcfsError> {
        let target = self
            .tasks
            .process_snapshot(self.process_id(process)?)
            .map_err(|_| ProcfsError::NotFound)?;
        let value = self.tasks.uts_identity(target.id).map_err(|_| ProcfsError::Invalid)?;
        Ok(ProcfsUtsView {
            namespace: target.namespaces.uts.serial,
            hostname: value.hostname,
            domainname: value.domainname,
        })
    }

    fn uts_namespace(&self, namespace: u64) -> Result<ProcfsUtsView, ProcfsError> {
        let identifier = hl_task::NamespaceId {
            kind: hl_task::NamespaceKind::Uts,
            serial: namespace,
        };
        let value = self
            .tasks
            .uts_namespace(identifier)
            .map_err(|_| ProcfsError::NotFound)?;
        Ok(ProcfsUtsView {
            namespace,
            hostname: value.hostname,
            domainname: value.domainname,
        })
    }

    fn write_uts(
        &self,
        namespace: u64,
        domain: bool,
        actor: hl_descriptor::OperationActor,
        bytes: &[u8],
    ) -> Result<(), hl_descriptor::ObjectError> {
        let process = hl_task::ProcessId::from_wire(actor.process, actor.process_generation)
            .ok_or(hl_descriptor::ObjectError::PermissionDenied)?;
        let thread = hl_task::ThreadId::from_wire(actor.thread, actor.thread_generation)
            .ok_or(hl_descriptor::ObjectError::PermissionDenied)?;
        let identifier = hl_task::NamespaceId {
            kind: hl_task::NamespaceKind::Uts,
            serial: namespace,
        };
        let values = if domain {
            (None, Some(bytes.to_vec()))
        } else {
            (Some(bytes.to_vec()), None)
        };
        self.tasks
            .replace_uts_namespace(process, thread, identifier, values.0, values.1)
            .map_err(|error| match error {
                hl_task::TaskError::PermissionDenied => hl_descriptor::ObjectError::PermissionDenied,
                hl_task::TaskError::InvalidProcess
                | hl_task::TaskError::InvalidThread
                | hl_task::TaskError::WrongProcess => hl_descriptor::ObjectError::PermissionDenied,
                hl_task::TaskError::InvalidCapacity => hl_descriptor::ObjectError::InvalidArgument,
                _ => hl_descriptor::ObjectError::Retired,
            })
    }

    fn mounts(&self, process: ProcfsProcessIdentity) -> Result<hl_vfs::ProcfsMountView, ProcfsError> {
        self.view(process)?;
        Self::mount_view(
            self.mounts
                .as_ref()
                .map(|mounts| mounts.snapshot())
                .as_deref()
                .unwrap_or(&[]),
        )
    }

    fn descriptor_numbers(&self, process: ProcfsProcessIdentity) -> Result<Vec<i32>, ProcfsError> {
        let id = self.process_id(process)?;
        let table = self.descriptor_table(id)?;
        const MAXIMUM_DESCRIPTORS: usize = 65_536;
        const MAXIMUM_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
        let mut numbers = table
            .bounded_active_snapshots(hl_descriptor::SnapshotBudget {
                max_items: MAXIMUM_DESCRIPTORS,
                max_peak_bytes: MAXIMUM_SNAPSHOT_BYTES,
            })
            .map_err(|_| ProcfsError::ResourceLimit)?
            .into_iter()
            .map(|snapshot| snapshot.number)
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        Ok(numbers)
    }

    fn descriptor_directory(
        &self,
        process: ProcfsProcessIdentity,
        file_type: u8,
        metadata: hl_descriptor::OfdMetadata,
    ) -> Result<Arc<dyn hl_descriptor::OpenFileDescription>, ProcfsError> {
        let id = self.process_id(process)?;
        Ok(Arc::new(descriptor::DescriptorDirectory::new(
            self.descriptor_table(id)?,
            file_type,
            metadata,
        )))
    }

    fn descriptor(&self, process: ProcfsProcessIdentity, number: i32) -> Result<ProcfsDescriptorView, ProcfsError> {
        let id = self.process_id(process)?;
        let table = self.descriptor_table(id)?;
        let snapshot = table.snapshot(number).map_err(|_| ProcfsError::NotFound)?;
        self.descriptor_view(&table, snapshot)
    }
}
