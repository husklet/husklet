# `prepare_tasks`

- [ ] Approved
- Timestamp: `1785820511286113849`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/containers/hl-engine/src/runtime/machine.rs:52:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `prepare_tasks` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.checkpoint_tasks`
- `.has_checkpoint_role`
- `.map_err`
- `.prepare_checkpoint`
- `.seccomp`
- `.with_seccomp`
- `Arc::new`
- `Ok`
- `TaskCheckpointParticipant::new`

## Source

````rust
pub(crate) fn prepare_tasks(assembly: &RuntimeAssembly) -> Result<(), CompositionError> {
    if assembly.has_checkpoint_role(hl_runtime::CheckpointRole::Task) {
        return Ok(());
    }
    assembly
        .prepare_checkpoint(Arc::new(
            TaskCheckpointParticipant::new(
                assembly.checkpoint_tasks(),
                Arc::new(TaskBindings),
                Arc::new(PortableTaskCodec),
            )
            .with_seccomp(assembly.seccomp()),
        ))
        .map_err(|_| CompositionError::RuntimeConstruction)
}
````

## Related context

### usage in `run`
fn run(
        &self,
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        assembly: &RuntimeAssembly,
        services: &RuntimeServices,
        cancellation: Arc<readiness::Cancellation>,
        threads: Arc<threads::ThreadSet>,
    ) -> Result<EngineExit, EngineError> {
        crate::runtime_machine::prepare_tasks(assembly).map_err(|_| EngineError::LaunchFailed)?;
        let path = plan
            .executable_host
            .as_deref()
            .or_else(|| plan.arguments.first().map(Vec::as_slice))
            .ok_or(EngineError::LaunchFailed)?;
        let architecture = match isa {
            GuestIsa::Aarch64 => GuestArchitecture::Aarch64,
            GuestIsa::X86_64 => GuestArchitecture::X86_64,
        };
        let guest_executable = routing::image::WorkspaceRoot::executable(plan).ok_or(EngineError::LaunchFailed)?;
        let (shared, shared_backings) = super::shared_backing::Factory::store(hl_memory::SharedLimits::default())
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .install_ipc(Arc::clone(&shared))
            .map_err(|_| EngineError::LaunchFailed)?;
        let arena = Arc::new(
            VirtualMemory::reserve_in(Arc::clone(&self.resources), arena::Capacity::DEFAULT)
                .map_err(|_| EngineError::LaunchFailed)?
                .with_shared_store(Arc::clone(&shared))
                .with_shared_backings(shared_backings),
        );
        let limits = LoadLimits {
            pie_hint: Some(0x10_00000),
            interpreter_hint: Some(0x20_00000),
            stack_hint: Some(0x30_00000),
            host_page_size: arena.page_size() as u64,
            ..LoadLimits::default()
        };
        let exec_limits = limits;
        let features = Self::guest_features(architecture);
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            Arc::clone(&shared),
            hl_memory::AddressSpaceId { slot: 1, generation: 1 },
        ));
        assembly
            .install_memory(Arc::new(hl_runtime::MemoryExit::new(Arc::clone(&mappings))))
            .map_err(|_| EngineError::LaunchFailed)?;
        let image_space = space::AddressSpace::new(Arc::clone(&arena), Arc::clone(&mappings));
        let source = match self.authority.as_ref() {
            Some(authority) => source::Source::Projected(source::ProjectedSource::new(Arc::clone(authority))),
            None => source::Source::File(source::FileSource::new(plan.rootfs.as_deref())),
        };
        let mut loader = Loader::new(source, exec_image::ImageSpace::new(Arc::clone(&image_space)), limits);
        let arguments = plan.arguments.iter().map(Vec::as_slice).collect::<Vec<_>>();
        // The Linux execution boundary preserves the caller's environment
        // byte-for-byte. Product/container defaults belong in the launch-plan
        // owner; synthesizing them here changes envp cardinality and ordering.
        let environment_refs = plan.environment.iter().map(Vec::as_slice).collect::<Vec<_>>();
        // Initial auxv identity must match the credentials installed by the
        // runtime route. Exec already projects the live task credentials.
        let uid = routing::launch_identity(plan, "HL_UID", routing::host_user())?;
        let gid = routing::launch_identity(plan, "HL_GID", routing::host_group())?;
        let loaded = loader
            .load(LoadRequest {
                architecture,
                image_path: path,
                executable_path: &guest_executable,
                arguments: &arguments,
                environment: &environment_refs,
                random: match self.entropy {
                    Some(entropy) => entropy,
                    None => self.entropy_source.read().map_err(|_| EngineError::LaunchFailed)?,
                },
                credentials: GuestCredentials {
                    user: uid,
                    effective_user: uid,
                    group: gid,
                    effective_group: gid,
                },
                features,
            })
            .map_err(EngineError::Load)?;
        let image_space_tls = loaded.initial_tls().clone();
        let auxiliary = image_data::AuxiliaryImage::encode(loaded.initial_stack());
        let (_, loaded_space) = loader.into_parts();
        if !Arc::ptr_eq(&loaded_space.space(), &image_space) {
            return Err(EngineError::LaunchFailed);
        }
        image_space.publish_procfs_image(&loaded, guest_executable.clone(), plan.environment.clone());
        let entry = loaded.dynamic_handoff().start_entry();
        let stack = loaded.initial_stack().stack_pointer();
        let cpu = match isa {
            GuestIsa::Aarch64 => {
                let mut cpu = Aarch64CpuState::default();
                cpu.pc = entry;
                cpu.sp = stack;
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            GuestIsa::X86_64 => {
                let mut cpu = CpuState::default();
                cpu.rip = entry;
                cpu.registers[4] = stack;
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        let snapshot = ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        };
        let routed = routing::create(
            Arc::clone(&arena),
            mappings,
            plan,
            assembly,
            architecture,
            Arc::clone(&cancellation),
            self.authority.clone(),
            self.entropy_source.clone(),
            &services.streams,
        )?;
        routed
            .process
            .space()
            .publish_procfs_image(&loaded, guest_executable.clone(), plan.environment.clone());
        routed
            .process
            .tasks()
            .publish_arguments(routed.process.process_id(), plan.arguments.clone())
            .map_err(|_| EngineError::LaunchFailed)?;
        routed.process.set_auxiliary(auxiliary)?;
        let execution_root = routing::image::WorkspaceRoot::select(plan);
        let load_context = Arc::new(exec_image::LoadContext::new(
            routed.process.tasks(),
            features,
            self.entropy_source.clone(),
        ));
        let loader_exec = Arc::new(hl_runtime::LoaderExecParticipant::new(
            architecture,
            exec_limits,
            source::Sources::new(execution_root.as_deref(), self.authority.clone()),
            exec_image::Spaces::new(Arc::clone(&image_space)),
            load_context.clone(),
            exec_image::Tls,
            exec_image::CpuImage,
            hl_runtime::LoaderExecImage {
                address_space: loaded_space,
                loaded,
                tls: image_space_tls,
                execution: snapshot.clone(),
            },
        ));
        let exec = Arc::new(exec_image::Coordinator::new(
            Arc::clone(&routed.process),
            Arc::clone(&threads),
            loader_exec,
            hl_runtime::TaskExecParticipant::new(routed.process.tasks()),
            execution_root,
            architecture,
            exec_limits,
            load_context,
            self.authority.clone(),
        ));
        routed
            .process
            .install_exec(&exec)
            .map_err(|_| EngineError::LaunchFailed)?;
        let exec_slot = assembly.exec_slot();
        exec_slot
            .register(routed.process.process(), exec)
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_registration(exec_image::Registration::new(exec_slot, routed.process.process()))
            .map_err(|_| EngineError::LaunchFailed)?;
        let _initial_router = routed.router;
        let contexts = Arc::new(clone::Contexts::new(Arc::clone(&routed.process), Arc::clone(&threads)));
        let clone_runtime = contexts.build();
        contexts
            .install(Arc::clone(&clone_runtime))
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_clone(&contexts)
            .map_err(|_| EngineError::LaunchFailed)?;
        let fork_runtime = fork::Runtime::new(Arc::clone(&routed.process), Arc::clone(&threads));
        routed
            .process
            .install_fork(&fork_runtime)
            .map_err(|_| EngineError::LaunchFailed)?;
        routed
            .process
            .install_threads(&threads)
            .map_err(|_| EngineError::LaunchFailed)?;
        let trap = hl_runtime::ThreadCloneTrap::new(clone_runtime, routed.thread);
        let router = Arc::new(
            routed
                .process
                .router(routed.thread, Arc::clone(&cancellation), Some(Box::new(trap))),
        );
        threads
            .prepare(
                routed.thread,
                routed.process.process_id(),
                Arc::clone(&router),
                cancellation,
                routed.process.space(),
            )
            .map_err(|_| EngineError::LaunchFailed)?;
        threads
            .stage(routed.thread, snapshot)
            .map_err(|_| EngineError::LaunchFailed)?
            .publish();
        let memfds = Arc::new(hl_runtime::MemfdBindings::new(routed.process.memfd_registry()?));
        // Tag 1 is the RuntimeMemfd file subtype. Other File-family codecs are
        // intentionally absent and therefore retain the previous reject behavior.
        let files = Arc::new(hl_runtime::FileObjectCatalog::rejecting().bind(1, memfds.clone()));
        assembly
            .prepare_checkpoint(Arc::new(hl_runtime::DescriptorCheckpointParticipant::new(
                assembly.checkpoint_descriptors(),
                Arc::new(
                    hl_runtime::DescriptorObjectCatalog::rejecting()
                        .bind(hl_descriptor::ObjectKind::File, files)
                        .bind(
                            hl_descriptor::ObjectKind::Pipe,
                            assembly.ipc_pipes().ok_or(EngineError::LaunchFailed)?.bindings(),
                        )
                        .bind(hl_descriptor::ObjectKind::Event, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::EventCounter, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::Poll, assembly.event_bindings())
                        .bind(hl_descriptor::ObjectKind::Socket, routed.process.network_bindings()),
                ),
            )))
            .map_err(|_| EngineError::LaunchFailed)?;
        let checkpoint_memory = Arc::new(hl_runtime::CheckpointMemoryState::new(Arc::new(
            hl_runtime::CheckpointMemory::new(routed.process.space().mappings(), Arc::clone(&shared)),
        )));
        assembly
            .prepare_checkpoint(Arc::new(
                hl_runtime::MemoryCheckpointParticipant::new(
                    checkpoint_memory.clone(),
                    Arc::new(checkpoint::Host::new(routed.process.space())),
                    Arc::new(hl_runtime::PortableMemoryCodec),
                )
                .with_resources(memfds),
            ))
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_provider_checkpoint()
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_event_checkpoint()
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_network_checkpoint(routed.process.network_bindings())
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .prepare_ipc_checkpoint(checkpoint_memory)
            .map_err(|_| EngineError::LaunchFailed)?;
        let machine = threads.find(routed.thread).ok_or(EngineError::LaunchFailed)?.machine;
        assembly
            .prepare_checkpoint(Arc::new(hl_runtime::ExecutionCheckpointParticipant::new(machine)))
            .map_err(|_| EngineError::LaunchFailed)?;
        assembly
            .finalize_checkpoint(hl_checkpoint::ImageLimits::new(
                8,
                arena::Capacity::MAXIMUM,
                arena::Capacity::MAXIMUM,
            ))
            .map_err(|_| EngineError::LaunchFailed)?;
        transfer::Operation::new(plan, assembly, services).apply()?;
        let waiters = waiter::Pool::new(&routed.process.tasks()).map_err(|()| EngineError::LaunchFailed)?;
        let result = Self::schedule(isa, plan, &threads, &waiters, self.host_faults.clone());
        // Every parked host wait must be interrupted before the fixed worker
        // set is joined, including successful exit-group termination.
        threads.cancel_all(9);
        waiters.stop();
        result
    }

`src/containers/hl-engine/src/ffi/linux/execution/mod.rs:129:9`

````rust
crate::runtime_machine::prepare_tasks
````

### usage in `construct`
fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        let (config, topology) = self.assembly_config(request.plan)?;
        let mut assembly =
            RuntimeAssembly::with_topology(config, topology).map_err(|_| CompositionError::RuntimeConstruction)?;
        if let Some(fork) = self.host.fork_port(&assembly)? {
            assembly
                .install_fork(fork)
                .map_err(|_| CompositionError::RuntimeConstruction)?;
        }
        if let Some(exec) = self.host.exec_port(&assembly)? {
            assembly
                .install_exec(exec)
                .map_err(|_| CompositionError::RuntimeConstruction)?;
        }
        prepare_tasks(&assembly)?;
        self.host.validate(&assembly)?;
        Ok(RustRuntimeMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            assembly,
            execution: Arc::clone(&self.execution),
            services: request.services.clone(),
        })
    }

`src/containers/hl-engine/src/runtime/machine.rs:257:9`

````rust
prepare_tasks
````
