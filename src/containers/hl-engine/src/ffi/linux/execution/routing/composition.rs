use super::{Arc, EngineError, RuntimeLaunchPlan, ProcessLimits, Resource, Limit, VirtualMemory, MappingCoordinator, MappingHostAdapter, RuntimeAssembly, readiness, Mutex, ports, Route, network, OperationRegistry, event_checkpoint, ProcessCredentials, task, OnceLock, ProcessMemory, image, BrkRegion, BrkSnapshot, GuestAddress, descriptor_table, exit_runtime, RuntimeMemorySyscalls, itimer, RuntimePathHost, ProcessContext, TableAdmission, BTreeMap};
use crate::composition::StandardStreams;

struct SystemLaunchPublication(hl_runtime::SystemLaunchUpdate);

impl SystemLaunchPublication {
    fn prepare(
        system: &Arc<hl_runtime::SystemAuthority>,
        boot_key: &[u8],
        resources: hl_runtime::ResourceSnapshot,
    ) -> Result<Self, EngineError> {
        system
            .prepare_launch(boot_key, resources)
            .map(Self)
            .map_err(|_| EngineError::LaunchFailed)
    }

    fn construction_observer(&mut self) -> hl_runtime::SystemObservationHandle {
        self.0.construction_observer()
    }

    fn finish<T>(self, result: Result<T, EngineError>) -> Result<T, EngineError> {
        let value = result?;
        self.0.commit();
        Ok(value)
    }
}

pub(in crate::ffi::linux::execution) fn launch_identity(
    plan: &RuntimeLaunchPlan,
    name: &str,
    inherited: u32,
) -> Result<u32, EngineError> {
    let value = plan.options.integer(name).map_err(|_| EngineError::LaunchFailed)?;
    match value {
        Some(value) => u32::try_from(value).map_err(|_| EngineError::LaunchFailed),
        None if plan.rootfs.is_some() => Ok(0),
        None => Ok(inherited),
    }
}

pub(in crate::ffi::linux::execution) fn host_user() -> u32 {
    // SAFETY: geteuid reads process credentials, retains no pointer, and cannot fail.
    unsafe { libc::geteuid() }
}

pub(in crate::ffi::linux::execution) fn host_group() -> u32 {
    // SAFETY: getegid reads process credentials, retains no pointer, and cannot fail.
    unsafe { libc::getegid() }
}

struct LaunchPolicy<'a> {
    plan: &'a RuntimeLaunchPlan,
}

impl<'a> LaunchPolicy<'a> {
    fn new(plan: &'a RuntimeLaunchPlan) -> Self {
        Self { plan }
    }

    fn seccomp_baseline(&self) -> Result<hl_linux::SeccompBaseline, EngineError> {
        match self.plan.options.get("HL_SECCOMP_BASELINE") {
            None | Some("container") => Ok(hl_linux::SeccompBaseline::Container),
            Some("disabled") => Ok(hl_linux::SeccompBaseline::Disabled),
            Some(_) => Err(EngineError::LaunchFailed),
        }
    }

    fn limits(&self) -> Result<(ProcessLimits, Vec<(Resource, Limit)>), EngineError> {
        let mut limits = ProcessLimits::default();
        let mut overrides = Vec::new();
        let Some(specification) = self.plan.options.get("HL_ULIMITS").filter(|value| !value.is_empty()) else {
            return Ok((limits, overrides));
        };
        for record in specification.split(',') {
            let (name, values) = record.split_once('=').ok_or(EngineError::LaunchFailed)?;
            let Some(resource) = Self::resource(name) else { continue };
            let (soft, hard) = values.split_once(':').map_or((values, values), |pair| pair);
            let limit = Limit::new(Self::value(soft)?, Self::value(hard)?).map_err(|_| EngineError::LaunchFailed)?;
            limits.set(resource, limit);
            overrides.push((resource, limit));
        }
        Ok((limits, overrides))
    }

    fn resource(name: &str) -> Option<Resource> {
        match name {
            "cpu" => Some(Resource::CpuTime),
            "fsize" => Some(Resource::FileSize),
            "data" => Some(Resource::Data),
            "stack" => Some(Resource::Stack),
            "core" => Some(Resource::Core),
            "rss" => Some(Resource::ResidentSet),
            "nproc" => Some(Resource::Processes),
            "nofile" => Some(Resource::OpenFiles),
            "memlock" => Some(Resource::LockedMemory),
            "as" => Some(Resource::AddressSpace),
            "locks" => Some(Resource::Locks),
            "sigpending" => Some(Resource::PendingSignals),
            "msgqueue" => Some(Resource::MessageQueue),
            "nice" => Some(Resource::Nice),
            "rtprio" => Some(Resource::RealtimePriority),
            "rttime" => Some(Resource::RealtimeTime),
            _ => None,
        }
    }

    fn value(value: &str) -> Result<u64, EngineError> {
        match value {
            "unlimited" | "-1" => Ok(u64::MAX),
            _ => value.parse().map_err(|_| EngineError::LaunchFailed),
        }
    }
}

pub(in crate::ffi::linux::execution) fn create(
    arena: Arc<VirtualMemory>,
    mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
    plan: &RuntimeLaunchPlan,
    assembly: &RuntimeAssembly,
    architecture: hl_linux::GuestArchitecture,
    cancellation: Arc<readiness::Cancellation>,
    authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
    entropy: Arc<dyn ports::random::EntropySource>,
    streams: &StandardStreams,
) -> Result<Route, EngineError> {
    let projected = authority.is_some();
    // A rooted container starts as root. A direct, rootless launch inherits
    // the host identity, matching Linux execution and the retained engine.
    let uid = launch_identity(plan, "HL_UID", host_user())?;
    let gid = launch_identity(plan, "HL_GID", host_group())?;
    let policy = LaunchPolicy::new(plan);
    let seccomp_baseline = policy.seccomp_baseline()?;
    let table = assembly.descriptors().descriptor_table();
    let handles = Arc::new(hl_runtime::ProcessHandleRegistry::new());
    let namespace_handles = Arc::new(hl_runtime::NamespaceHandleRegistry::new());
    let events = assembly.events();
    let epoll = assembly.epoll();
    let epoll_table = assembly.descriptors();
    let descriptor_image = epoll_table.image_slot();
    let network_enabled = (plan.options.get("HL_NET_HOST") == Some("1")
        || plan.options.get("HL_UNTRUSTED") == Some("1"))
        && plan.options.get("HL_NET_ISOLATE") != Some("1");
    let network_policy = hl_network::NetworkPolicy::from_launch(
        plan.options.get("HL_NET_ISOLATE") == Some("1"),
        plan.options.get_bytes("HL_NETBR").unwrap_or_default(),
        plan.options.get_bytes("HL_IP").unwrap_or_default(),
        plan.options.get_bytes("HL_NETIFS").unwrap_or_default(),
    )
    .map_err(|_| EngineError::LaunchFailed)?;
    let network = network::CheckpointRuntime::new(
        assembly.checkpoint_network(),
        assembly.checkpoint_descriptors(),
        authority.clone(),
        network_policy,
    );
    let event_operations = Arc::new(OperationRegistry::new());
    let event_checkpoint = event_checkpoint::Resources::new(assembly);
    let tasks = assembly.tasks();
    let seccomp = assembly.seccomp();
    let mut launch_credentials = ProcessCredentials::new(uid, gid, &[], 32).expect("valid launch credentials");
    launch_credentials.capabilities = hl_task::CapabilitySets {
        effective: hl_task::CapabilitySets::CONTAINER,
        permitted: hl_task::CapabilitySets::CONTAINER,
        inheritable: 0,
        ambient: 0,
    };
    launch_credentials.capability_bounding = hl_task::CapabilitySets::CONTAINER;
    let (launch_limits, limit_overrides) = policy.limits()?;
    let source = match tasks.snapshot().init {
        Some(init) => {
            tasks
                .replace_credentials(init, launch_credentials)
                .map_err(|_| EngineError::LaunchFailed)?;
            for (resource, limit) in limit_overrides {
                tasks
                    .set_limit(init, resource, limit)
                    .map_err(|_| EngineError::LaunchFailed)?;
            }
            tasks
                .snapshot()
                .processes
                .into_iter()
                .find(|process| process.id == init)
                .expect("init process exists")
                .leader
        }
        None => {
            tasks
                .create_init(launch_credentials, launch_limits)
                .expect("task registry has init capacity")
                .1
        }
    };
    if let Some(hostname) = plan.options.get_bytes("HL_HOSTNAME") {
        let init = tasks.snapshot().init.ok_or(EngineError::LaunchFailed)?;
        let current = tasks.uts_identity(init).map_err(|_| EngineError::LaunchFailed)?;
        let owner = current.owner();
        let identity = hl_task::UtsIdentity::owned(hostname.to_vec(), current.domainname, owner)
            .map_err(|_| EngineError::LaunchFailed)?;
        tasks
            .replace_uts_identity(init, identity)
            .map_err(|_| EngineError::LaunchFailed)?;
    }
    let memory_limit = plan
        .options
        .get("HL_MEM_MAX")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0);
    let cpu_limit = plan
        .options
        .get("HL_CPUS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value != 0)
        .map(|_| tasks.topology().online());
    let uptime_seconds =
        crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::Monotonic)
            .unwrap_or(1_000_000_000)
            .saturating_div(1_000_000_000)
            .max(1);
    let system = assembly.system();
    let boot_key = plan
        .options
        .get_bytes("HL_NETNS")
        .or_else(|| plan.options.get_bytes("HL_HOSTNAME"))
        .or(plan.rootfs.as_deref())
        .or(plan.executable_host.as_deref())
        .unwrap_or(b"hl-engine");
    let child_plan = tasks
        .begin_fork_process(source)
        .map_err(|_| EngineError::LaunchFailed)?;
    let mut system_launch = match SystemLaunchPublication::prepare(
        &system,
        boot_key,
        hl_runtime::ResourceSnapshot {
            uptime_seconds,
            total_memory: memory_limit.unwrap_or(0),
            free_memory: memory_limit.unwrap_or(0),
            cpu_limit,
            ..hl_runtime::ResourceSnapshot::default()
        },
    ) {
        Ok(publication) => publication,
        Err(error) => {
            tasks
                .rollback_fork_process(child_plan)
                .map_err(|_| EngineError::LaunchFailed)?;
            return Err(error);
        }
    };
    let system_observer = system_launch.construction_observer();
    let child = tasks
        .commit_fork_process(child_plan)
        .map_err(|_| EngineError::LaunchFailed)?;
    // A syscall router can be composed before an executable image is attached
    // (checkpoint restore and the route-level harness both do this). Linux task
    // identity still needs a stable initial comm; exec replaces it with the
    // image basename when an image is present.
    let executable = super::image::WorkspaceRoot::executable(plan).unwrap_or_else(|| b"/hl-engine".to_vec());
    tasks
        .set_name(child.1, hl_linux::ExecPlan::comm_from_path(&executable))
        .map_err(|_| EngineError::LaunchFailed)?;
    seccomp
        .register_inheriting(source, &[])
        .map_err(|_| EngineError::LaunchFailed)?;
    seccomp.fork(source, child.1).map_err(|_| EngineError::LaunchFailed)?;
    let deadlines = readiness::deadline::Queue::new().map_err(|_| EngineError::LaunchFailed)?;
    let clock = Arc::new(task::ClockIdentity::new(
        u64::from(uid),
        u64::from(gid),
        child.0,
        Arc::clone(&deadlines),
        Arc::clone(&tasks),
    ));
    let interruption = Arc::new(task::FutexInterrupt::new());
    let futex = Arc::new(
        hl_runtime::SafeRuntimeFutex::new(
            Arc::clone(&mappings),
            clock.clone(),
            interruption.clone(),
            hl_sync::FutexLimits::default(),
        )
        .map_err(|_| EngineError::LaunchFailed)?,
    );
    let ipc_catalog = assembly
        .ipc()
        .ok_or(EngineError::Construction(crate::composition::ConstructionError::Ipc))?;
    let ipc_pipes = assembly
        .ipc_pipes()
        .ok_or(EngineError::Construction(crate::composition::ConstructionError::Ipc))?;
    let ipc = Arc::new(hl_runtime::MemoryMappings::new(Arc::clone(&mappings)));
    let vfork = Arc::new(OnceLock::new());
    let space = super::super::space::AddressSpace::new(Arc::clone(&arena), Arc::clone(&mappings));
    let procfs_spaces = super::super::process_memory::ProcfsSpaces::new(child.0, &space);
    let process_memory = ProcessMemory::new(Arc::clone(&space));
    let ptrace = Arc::new(hl_runtime::PtraceCatalog::default());
    let trace_exchange = hl_runtime::TraceExchange::new(Arc::new(process_memory.clone()));
    ptrace.register(child.0, Arc::clone(&trace_exchange));
    let sigreturn_pc = image::SignalGateway::install(&mappings, &process_memory, architecture)?;
    let mut brk = BrkRegion::new(
        Arc::clone(&mappings),
        BrkSnapshot {
            lower: GuestAddress::new(0x80_0000),
            current: GuestAddress::new(0x80_0000),
            upper: GuestAddress::new(0xf0_0000),
            backing_identity: hl_runtime::BRK_BACKING_IDENTITY,
        },
    )
    .map_err(|_| EngineError::LaunchFailed)?;
    if let Some(limit) = memory_limit {
        brk = brk
            .with_account(Arc::new(super::super::memory_account::MemoryAccount::new(
                limit,
                system_observer,
            )))
            .map_err(|_| EngineError::LaunchFailed)?;
    }
    let (path_host, watches) = image::WorkspaceRoot::host(
        plan,
        authority,
        Arc::clone(&tasks),
        child.0,
        Arc::clone(&namespace_handles),
        Arc::clone(&table),
        network.files(),
        Arc::clone(&entropy),
        assembly.system(),
        architecture,
    )?;
    if path_host
        .as_ref()
        .is_some_and(|host| image::WorkspaceRoot::configure(host, plan, projected).is_err())
        && plan.rootfs.is_none()
    {
        return Err(EngineError::LaunchFailed);
    }
    let descriptors = Arc::new(match streams.terminal() {
        Some(terminal) => path_host
            .as_ref()
            .ok_or(EngineError::Construction(
                crate::composition::ConstructionError::Descriptor,
            ))?
            .initial_terminal(Arc::clone(&table), &tasks, child.0, child.1, &terminal)
            .map_err(|_| EngineError::Construction(crate::composition::ConstructionError::Descriptor))?,
        None => {
            descriptor_table::Set::with_table(Arc::clone(&table), streams).expect("valid standard descriptor table")
        }
    });
    let exit = exit_runtime(
        Arc::clone(&tasks),
        Arc::clone(&mappings),
        Arc::clone(&descriptor_image),
        Arc::clone(&epoll),
        futex.clone(),
        Arc::clone(&ipc_catalog),
        Arc::clone(&ipc),
        assembly.locks(),
        Arc::clone(&clock),
        Arc::clone(&vfork),
        Arc::clone(&handles),
        Arc::clone(&ptrace),
        Arc::clone(&procfs_spaces),
        path_host.as_ref().map(|host| host.terminal_catalog()),
    );
    let mut memory = RuntimeMemorySyscalls::new(
        Arc::clone(&mappings),
        Arc::clone(&table),
        process_memory.clone(),
        architecture,
    )
    .with_process(child.0.number())
    .with_brk(brk)
    .with_address_limit(arena.length() as u64)
    .with_host(Arc::new(super::super::super::memory_control::Control::new(
        Arc::clone(&arena),
        Arc::clone(&mappings),
        Arc::new(super::super::memory_limit::MemoryLimit::new(
            Arc::clone(&tasks),
            child.0,
        )),
    )));
    if let Some(shared) = mappings.shared_objects() {
        memory = memory.with_memfd_objects(
            Arc::clone(&shared),
            u64::from(child.0.number()),
            Arc::new(hl_runtime::MemfdRegistry::new()),
        );
        if let Some(host) = &path_host {
            memory = memory.with_descriptor_source(Arc::new(host.mapping_source(Arc::clone(&arena))));
        }
    }
    let alarms =
        hl_runtime::AlarmRegistry::new(Arc::clone(&tasks), Arc::new(itimer::Scheduler(Arc::clone(&deadlines))));
    let working_path = plan.options.get("HL_CWD").unwrap_or("/");
    let working_path = hl_runtime::GuestPath::new(working_path).map_err(|_| EngineError::LaunchFailed)?;
    if !working_path.is_absolute() {
        return Err(EngineError::LaunchFailed);
    }
    if let Some(host) = &path_host {
        host.working_base(working_path.clone())
            .map_err(|_| EngineError::LaunchFailed)?;
    }
    let working = Arc::new(hl_runtime::WorkingDirectory::root());
    working.replace(working_path);
    let procfs_resources = super::super::process_resources::Catalog::new(child.0, &table, &working);
    let process = Arc::new(ProcessContext {
        projected,
        aio: Arc::new(hl_runtime::AioCatalog::default()),
        descriptors,
        entropy,
        handles,
        namespace_handles,
        events,
        epoll,
        epoll_table,
        table_admission: TableAdmission::with_root(),
        _table_permit: None,
        thread_files: Mutex::new(BTreeMap::new()),
        network,
        network_enabled,
        event_operations,
        event_checkpoint,
        tasks,
        seccomp,
        seccomp_baseline,
        process: child.0,
        memory: Arc::new(Mutex::new(memory)),
        clock,
        deadlines,
        alarms: Arc::clone(&alarms),
        timers: hl_runtime::TimerRegistry::new(child.0, Arc::clone(&alarms)),
        exec: assembly.exec_slot(),
        exec_queue: Arc::new(hl_runtime::ExecQueue::default()),
        futex,
        interruptions: interruption,
        architecture,
        trace: plan.result_path.is_some(),
        path_host,
        watches,
        space,
        procfs_spaces,
        procfs_resources,
        fork: OnceLock::new(),
        threads: OnceLock::new(),
        clone_context: OnceLock::new(),
        exec_coordinator: OnceLock::new(),
        sigreturn_pc,
        exec_registration: OnceLock::new(),
        working,
        fs_context: Arc::new(hl_runtime::FsContext::default()),
        ipc_catalog,
        posix_queues: Arc::new(hl_runtime::MqNamespace::new(hl_runtime::MqLimits::default())),
        ipc_pipes,
        ipc,
        locks: assembly.locks(),
        exit,
        ptrace,
        system: assembly.system(),
        vfork,
    });
    let router = process.router(child.1, cancellation, None);
    let route = Route {
        router,
        thread: child.1,
        process,
    };
    system_launch.finish(Ok(route))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;

    fn plan(options: Options) -> RuntimeLaunchPlan {
        RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options,
        }
    }

    #[test]
    fn absent_identity_inherits_host() {
        let plan = plan(Options::default());
        assert_eq!(launch_identity(&plan, "HL_UID", 501), Ok(501));
        assert_eq!(launch_identity(&plan, "HL_GID", 20), Ok(20));
    }

    #[test]
    fn explicit_root_does_not_inherit() {
        let mut options = Options::default();
        options.set("HL_UID", "0", true).unwrap();
        options.set("HL_GID", "0", true).unwrap();
        let plan = plan(options);
        assert_eq!(launch_identity(&plan, "HL_UID", 501), Ok(0));
        assert_eq!(launch_identity(&plan, "HL_GID", 20), Ok(0));
    }

    #[test]
    fn seccomp_baseline_is_typed_and_defaults_to_container() {
        assert_eq!(
            LaunchPolicy::new(&plan(Options::default())).seccomp_baseline(),
            Ok(hl_linux::SeccompBaseline::Container),
        );
        for (value, expected) in [
            ("container", Ok(hl_linux::SeccompBaseline::Container)),
            ("disabled", Ok(hl_linux::SeccompBaseline::Disabled)),
        ] {
            let mut options = Options::default();
            options.set("HL_SECCOMP_BASELINE", value, true).unwrap();
            assert_eq!(LaunchPolicy::new(&plan(options)).seccomp_baseline(), expected);
        }
        let mut options = Options::default();
        options.set("HL_SECCOMP_BASELINE", "unknown", true).unwrap();
        assert_eq!(
            LaunchPolicy::new(&plan(options)).seccomp_baseline(),
            Err(EngineError::LaunchFailed)
        );
    }

    #[test]
    fn launch_limits_parse_linux_resources() {
        let mut options = Options::default();
        options
            .set("HL_ULIMITS", "nofile=1024:2048,core=unlimited,unknown=bad", true)
            .unwrap();
        let (limits, overrides) = LaunchPolicy::new(&plan(options)).limits().unwrap();

        assert_eq!(limits.get(Resource::OpenFiles), Some(Limit { soft: 1024, hard: 2048 }));
        assert_eq!(
            limits.get(Resource::Core),
            Some(Limit {
                soft: u64::MAX,
                hard: u64::MAX
            })
        );
        assert_eq!(overrides.len(), 2);
    }

    #[test]
    fn launch_limits_reject_invalid_known_values() {
        for specification in ["nofile=2048:1024", "nofile=bad", "nofile"] {
            let mut options = Options::default();
            options.set("HL_ULIMITS", specification, true).unwrap();
            assert!(matches!(
                LaunchPolicy::new(&plan(options)).limits(),
                Err(EngineError::LaunchFailed)
            ));
        }
    }

    #[test]
    fn downstream_failure_drops_unpublished_system_launch() {
        let system = Arc::new(hl_runtime::SystemAuthority::default());
        let before = (system.boot_identity(), system.snapshot());
        let update = SystemLaunchPublication::prepare(
            &system,
            b"next",
            hl_runtime::ResourceSnapshot {
                total_memory: 4096,
                free_memory: 2048,
                ..hl_runtime::ResourceSnapshot::default()
            },
        )
        .unwrap();
        assert_eq!(
            update.finish::<()>(Err(EngineError::LaunchFailed)),
            Err(EngineError::LaunchFailed)
        );
        assert_eq!((system.boot_identity(), system.snapshot()), before);
    }
}
