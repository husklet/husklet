//! `ProcfsSource` trait adapter for the task-backed procfs.

use std::sync::Arc;

use hl_task::ProcessId;
use hl_vfs::{
    ProcfsCgroupView, ProcfsCpuView, ProcfsDescriptorView, ProcfsError, ProcfsProcessIdentity, ProcfsProcessView,
    ProcfsSource, ProcfsStatView, ProcfsSystemView, ProcfsThreadIdentity, ProcfsUtsView,
};

use super::{TaskProcfs, descriptor};

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
        let process = ProcessId::from_wire(process.slot(), process.generation()).ok_or(ProcfsError::NotFound)?;
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

    fn executable(&self, process: ProcfsProcessIdentity) -> Result<Vec<u8>, ProcfsError> {
        let id = self.process_id(process)?;
        let path = match &self.resources {
            Some(resources) => resources.executable(id)?,
            None if self.current == Some(id) => self.executable.clone().ok_or(ProcfsError::NotFound)?,
            None => return Err(ProcfsError::NotFound),
        };
        if path.is_empty() {
            return Err(ProcfsError::NotFound);
        }
        Ok(path)
    }

    fn process(&self, process: ProcfsProcessIdentity) -> Result<ProcfsProcessView, ProcfsError> {
        self.view(process)
    }

    fn oom_score_adj(
        &self,
        process: ProcfsProcessIdentity,
        thread: Option<ProcfsThreadIdentity>,
    ) -> Result<i16, ProcfsError> {
        let process = ProcessId::from_wire(process.slot(), process.generation()).ok_or(ProcfsError::NotFound)?;
        let thread = thread.map(|thread| self.thread_id(thread)).transpose()?;
        self.tasks
            .task_oom_score_adj(process, thread)
            .map_err(|_| ProcfsError::NotFound)
    }

    fn write_oom_score_adj(
        &self,
        process: ProcfsProcessIdentity,
        thread: Option<ProcfsThreadIdentity>,
        _actor: hl_descriptor::OperationActor,
        value: i16,
    ) -> Result<(), hl_descriptor::ObjectError> {
        let task_qualified = thread.is_some();
        let process = ProcessId::from_wire(process.slot(), process.generation()).ok_or_else(|| {
            thread.map_or(hl_descriptor::ObjectError::Retired, |_| {
                hl_descriptor::ObjectError::NoSuchProcess
            })
        })?;
        let thread = thread
            .map(|thread| self.thread_id(thread))
            .transpose()
            .map_err(|_| hl_descriptor::ObjectError::NoSuchProcess)?;
        self.tasks
            .set_task_oom_score_adj(process, thread, value)
            .map_err(|error| match error {
                hl_task::TaskError::InvalidLimit => hl_descriptor::ObjectError::InvalidArgument,
                _ if task_qualified => hl_descriptor::ObjectError::NoSuchProcess,
                _ => hl_descriptor::ObjectError::Retired,
            })
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
            resources.process_limit,
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
                hl_task::TaskError::PermissionDenied(_) => hl_descriptor::ObjectError::PermissionDenied,
                hl_task::TaskError::InvalidProcess(_)
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
        #[allow(clippy::items_after_statements)]
        const MAXIMUM_DESCRIPTORS: usize = 65_536;
        #[allow(clippy::items_after_statements)]
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
