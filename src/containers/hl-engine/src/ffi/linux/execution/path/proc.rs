use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{lease, namespace, projected};

struct HostCpu;

impl hl_runtime::ProcfsCpuPort for HostCpu {
    fn ticks(&self, online: usize) -> Vec<hl_runtime::ProcfsCpuTicks> {
        let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
            return Vec::new();
        };
        stat.lines()
            .filter_map(|line| {
                let mut fields = line.split_ascii_whitespace();
                let name = fields.next()?;
                if !name
                    .strip_prefix("cpu")
                    .is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
                {
                    return None;
                }
                Some(hl_runtime::ProcfsCpuTicks {
                    user: fields.next()?.parse().ok()?,
                    nice: fields.next()?.parse().ok()?,
                    system: fields.next()?.parse().ok()?,
                    idle: fields.next()?.parse().ok()?,
                })
            })
            .take(online)
            .collect()
    }
}

impl super::NativePath {
    pub(in crate::ffi::linux::execution) fn with_system(mut self, system: Arc<hl_runtime::SystemAuthority>) -> Self {
        self.system = Some(system);
        self
    }

    #[cfg(test)]
    pub(in crate::ffi::linux::execution) fn for_test(
        &self,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        descriptors: Arc<hl_descriptor::DescriptorTable>,
        working: Arc<hl_runtime::WorkingDirectory>,
        fs_context: Arc<hl_runtime::FsContext>,
    ) -> Arc<Self> {
        self.for_process_inner(
            process,
            thread,
            descriptors,
            working,
            fs_context,
            None,
            None,
            None,
            None,
            hl_linux::SeccompBaseline::Container,
        )
    }

    pub(in crate::ffi::linux::execution) fn for_process(
        &self,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        descriptors: Arc<hl_descriptor::DescriptorTable>,
        working: Arc<hl_runtime::WorkingDirectory>,
        fs_context: Arc<hl_runtime::FsContext>,
        spaces: Arc<super::super::process_memory::ProcfsSpaces>,
        resources: Arc<super::super::process_resources::Catalog>,
        network: Arc<dyn hl_runtime::ProcfsNetworkPort>,
        seccomp: Arc<hl_runtime::SeccompControl>,
        seccomp_baseline: hl_linux::SeccompBaseline,
    ) -> Arc<Self> {
        self.for_process_inner(
            process,
            thread,
            descriptors,
            working,
            fs_context,
            Some(Arc::new(
                super::super::process_memory::ProcfsMemory::new(spaces).with_paths(self.mapping_paths()),
            )),
            Some(network),
            Some(resources),
            Some(seccomp),
            seccomp_baseline,
        )
    }

    fn for_process_inner(
        &self,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
        descriptors: Arc<hl_descriptor::DescriptorTable>,
        working: Arc<hl_runtime::WorkingDirectory>,
        fs_context: Arc<hl_runtime::FsContext>,
        memory: Option<Arc<super::super::process_memory::ProcfsMemory>>,
        network: Option<Arc<dyn hl_runtime::ProcfsNetworkPort>>,
        resources: Option<Arc<super::super::process_resources::Catalog>>,
        seccomp: Option<Arc<hl_runtime::SeccompControl>>,
        seccomp_baseline: hl_linux::SeccompBaseline,
    ) -> Arc<Self> {
        let executable = self
            .executable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let auxiliary = self
            .auxiliary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let tasks = self.tasks.clone();
        let procfs = tasks.as_ref().map(|tasks| {
            let source = source(
                Arc::clone(tasks),
                process,
                descriptors,
                Arc::clone(&self.paths),
                self.projected.clone(),
                self.system.as_ref(),
                self.namespace_root.as_slice(),
                working,
                fs_context,
                self.source.mount_port(),
                memory,
                network,
                resources,
                self.cpu_model.clone(),
                seccomp.clone(),
                seccomp_baseline,
            );
            Arc::new(hl_runtime::Procfs::new(Arc::new(source)))
        });
        Arc::new(Self {
            source: self.source.clone(),
            paths: Arc::clone(&self.paths),
            synthetic_paths: Arc::clone(&self.synthetic_paths),
            projected: self.projected.clone(),
            writes: Arc::clone(&self.writes),
            ownership: Arc::clone(&self.ownership),
            locks: self.locks.clone(),
            watches: Arc::clone(&self.watches),
            executable: Arc::new(Mutex::new(executable)),
            auxiliary: Arc::new(Mutex::new(auxiliary)),
            namespace_root: Arc::clone(&self.namespace_root),
            procfs,
            tasks: self.tasks.clone(),
            process: Some(process),
            thread: Some(thread),
            namespace_handles: self.namespace_handles.clone(),
            terminals: Arc::clone(&self.terminals),
            terminal_bindings: Arc::clone(&self.terminal_bindings),
            terminal_signals: Arc::clone(&self.terminal_signals),
            transfers: Arc::clone(&self.transfers),
            fifos: Arc::clone(&self.fifos),
            entropy: Arc::clone(&self.entropy),
            system: self.system.clone(),
            cpu_model: self.cpu_model.clone(),
        })
    }
}

pub(super) fn source(
    tasks: Arc<hl_task::TaskRegistry>,
    process: hl_task::ProcessId,
    descriptors: Arc<hl_descriptor::DescriptorTable>,
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    projected: projected::Registry,
    system: Option<&Arc<hl_runtime::SystemAuthority>>,
    root: &[u8],
    working: Arc<hl_runtime::WorkingDirectory>,
    fs_context: Arc<hl_runtime::FsContext>,
    mounts: Option<Arc<dyn hl_runtime::ProcfsMountPort>>,
    memory: Option<Arc<super::super::process_memory::ProcfsMemory>>,
    network: Option<Arc<dyn hl_runtime::ProcfsNetworkPort>>,
    resources: Option<Arc<super::super::process_resources::Catalog>>,
    cpu_model: hl_runtime::ProcfsCpuModel,
    seccomp: Option<Arc<hl_runtime::SeccompControl>>,
    seccomp_baseline: hl_linux::SeccompBaseline,
) -> hl_runtime::TaskProcfs {
    let source = hl_runtime::TaskProcfs::with_descriptors(
        tasks,
        process,
        descriptors,
        namespace::ProcfsTargets::new(paths, projected),
    );
    let source = source
        .with_root(root.to_vec())
        .with_working(working)
        .with_fs_context(fs_context)
        .with_cpu(Arc::new(HostCpu))
        .with_cpu_model(cpu_model);
    let source = match seccomp {
        Some(seccomp) => source.with_seccomp(seccomp, seccomp_baseline),
        None => source,
    };
    let source = match mounts {
        Some(mounts) => source.with_mounts(mounts),
        None => source,
    };
    let source = match memory {
        Some(memory) => {
            let stat: Arc<dyn hl_runtime::ProcfsStatPort> = memory.clone();
            let memory: Arc<dyn hl_runtime::ProcfsMemoryPort> = memory;
            source.with_memory(memory).with_stat(stat)
        }
        None => source,
    };
    let source = match network {
        Some(network) => source.with_network(network),
        None => source,
    };
    let source = match resources {
        Some(resources) => source.with_resources(resources),
        None => source,
    };
    match system {
        Some(system) => source.with_system(Arc::clone(system)),
        None => source,
    }
}
