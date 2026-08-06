use super::*;
use hl_descriptor::OfdMetadata;

#[test]
fn cpu_model_features() {
    assert_eq!(
        super::CpuPolicy::model(
            hl_isa::GuestArchitecture::Aarch64,
            hl_loader::GuestFeatures {
                hardware: 0x10_01fb,
                hardware_second: 0,
            },
        ),
        hl_vfs::ProcfsCpuModel::Aarch64 {
            hardware: 0x10_01fb,
            hardware_second: 0,
        },
    );
    let hl_vfs::ProcfsCpuModel::X86_64 {
        vendor, name, flags, ..
    } = super::CpuPolicy::model(hl_isa::GuestArchitecture::X86_64, hl_loader::GuestFeatures::default())
    else {
        panic!("x86 model");
    };
    assert_eq!(vendor, "GenuineIntel");
    assert_eq!(name, "hl JIT x86-64 processor");
    assert!(flags.contains(&"fpu"));
    assert!(flags.contains(&"sse2"));
    assert!(!flags.contains(&"avx"));
}
use hl_task::{ExitStatus, Limit, ProcessCredentials, ProcessLimits, RegistryConfig};

struct Targets;

#[derive(Debug)]
struct DescriptorObject(u64);

impl hl_descriptor::OpenFileDescription for DescriptorObject {
    fn kind(&self) -> hl_descriptor::ObjectKind {
        hl_descriptor::ObjectKind::File
    }

    fn metadata(&self) -> Result<OfdMetadata, hl_descriptor::ObjectError> {
        let timestamp = hl_descriptor::OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 1,
            inode: self.0,
            kind: 8,
            permissions: 0o600,
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }
}

impl DescriptorTarget for Targets {
    fn path(&self, _metadata: &OfdMetadata) -> Result<Vec<u8>, ProcfsError> {
        Err(ProcfsError::NotFound)
    }
}

#[test]
fn descriptor_reuse() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, _) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let first = table
        .install(
            0,
            Arc::new(DescriptorObject(11)),
            hl_descriptor::DescriptorFlags::default(),
        )
        .unwrap();
    let stale = table.snapshot(first).unwrap();
    table.close(first).unwrap();
    let replacement = table
        .install(
            0,
            Arc::new(DescriptorObject(22)),
            hl_descriptor::DescriptorFlags::default(),
        )
        .unwrap();
    assert_eq!(replacement, first);
    let source = TaskProcfs::with_descriptors(Arc::clone(&tasks), process, Arc::clone(&table), Arc::new(Targets));
    assert_eq!(source.descriptor_view(&table, stale), Err(ProcfsError::NotFound));
    assert_eq!(
        source
            .descriptor(identity(&source, process.number()), replacement)
            .unwrap()
            .inode,
        22
    );
}

fn identity(source: &TaskProcfs, number: u32) -> ProcfsProcessIdentity {
    source.resolve_process(number).unwrap()
}

struct StatMetrics;

impl StatPort for StatMetrics {
    fn sample(&self, _process: ProcessId) -> Result<super::StatMetrics, ProcfsError> {
        Ok(metrics())
    }
}

fn metrics() -> super::StatMetrics {
    super::StatMetrics {
        terminal: 0,
        flags: 4_194_560,
        minor_faults: 1,
        child_minor_faults: 2,
        major_faults: 3,
        child_major_faults: 4,
        user_ticks: 5,
        system_ticks: 6,
        child_user_ticks: 7,
        child_system_ticks: 8,
        priority: 20,
        nice: 0,
        interval_ticks: 0,
        start_ticks: 9,
        virtual_bytes: 65_536,
        resident_pages: 10,
        resident_limit: u64::MAX,
        code_start: 0x1000,
        code_end: 0x2000,
        stack_start: 0x8000,
        stack_pointer: 0x7ff0,
        instruction_pointer: 0x1234,
        wait_channel: 0,
        swapped_pages: 0,
        child_swapped_pages: 0,
        exit_signal: 17,
        processor: 2,
        realtime_priority: 0,
        policy: 0,
        delay_ticks: 11,
        guest_ticks: 12,
        child_guest_ticks: 13,
        data_start: 0x3000,
        data_end: 0x4000,
        heap_start: 0x5000,
        arguments_start: 0x6000,
        arguments_end: 0x6100,
        environment_start: 0x6200,
        environment_end: 0x6300,
    }
}

#[test]
fn process_identity_matches_task_wire_and_resolves_self_and_peer() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let (peer, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(thread).unwrap())
        .unwrap();
    let source = TaskProcfs::new(Arc::clone(&tasks));

    for process in [parent, peer] {
        let identity = source.resolve_process(process.number()).unwrap();
        assert_eq!((identity.slot(), identity.generation()), process.wire_parts());
        assert_eq!(source.process_id(identity), Ok(process));
    }
    assert_eq!(source.resolve_process(0), Err(ProcfsError::NotFound));
    assert_eq!(source.resolve_process(u32::MAX), Err(ProcfsError::NotFound));
}

#[test]
fn process_identity_changes_on_reuse_and_stale_identity_is_rejected() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            max_threads: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let (parent, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let source = TaskProcfs::new(Arc::clone(&tasks));
    let (first, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(thread).unwrap())
        .unwrap();
    let stale = source.resolve_process(first.number()).unwrap();

    tasks.exit_process(first, ExitStatus::Code(0)).unwrap();
    tasks.reap(parent, first).unwrap();
    assert_eq!(source.process_id(stale), Err(ProcfsError::NotFound));
    assert_eq!(source.resolve_process(first.number()), Err(ProcfsError::NotFound));

    let (second, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(thread).unwrap())
        .unwrap();
    let current = source.resolve_process(second.number()).unwrap();
    assert_eq!(current.slot(), stale.slot());
    assert_ne!(current.generation(), stale.generation());
    assert_eq!(source.process_id(stale), Err(ProcfsError::NotFound));
    assert_eq!(source.process_id(current), Ok(second));
}

#[test]
fn task_projection() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            online_cpus: 6,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let mut limits = ProcessLimits::default();
    limits.set(Resource::Core, Limit::new(9, 10).unwrap());
    let (process, thread) = tasks
        .create_init(ProcessCredentials::new(12, 34, &[56], 8).unwrap(), limits)
        .unwrap();
    let fs_context = Arc::new(crate::FsContext::default());
    let source = TaskProcfs::new(Arc::clone(&tasks))
        .with_root(b"/guest".to_vec())
        .with_fs_context(Arc::clone(&fs_context));
    let view = source.process(identity(&source, process.number())).unwrap();
    assert_eq!(view.process, 1);
    assert_eq!(view.threads, 1);
    assert_eq!(view.real_user, 12);
    assert_eq!(view.real_group, 34);
    assert_eq!(view.groups, [56]);
    assert_eq!(view.umask, None);
    assert_eq!(fs_context.replace_mask(0o077), 0o022);
    assert_eq!(source.process(identity(&source, process.number())).unwrap().umask, None);
    assert_eq!(view.allowed_mask, "3f");
    assert_eq!(view.allowed_list, "0-5");
    assert_eq!(source.cpu().unwrap().online(), 6);
    tasks.set_thread_blocked(thread, true).unwrap();
    assert_eq!(
        source.process(identity(&source, process.number())).unwrap().state,
        ProcfsProcessState::Sleeping
    );
    tasks.set_thread_blocked(thread, false).unwrap();
    assert!(
        view.limits
            .iter()
            .any(|limit| { limit.resource == ProcfsLimitResource::Core && limit.soft == 9 && limit.hard == 10 })
    );
}

#[test]
fn stat_requires_metrics() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, _) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let source = TaskProcfs::new(tasks);
    assert_eq!(
        source.stat(identity(&source, process.number())),
        Err(ProcfsError::NotFound)
    );
}

#[test]
fn process_view_uses_selected_seccomp_baseline() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let seccomp = Arc::new(crate::SeccompControl::new(1).unwrap());
    seccomp.register(thread).unwrap();

    let disabled_source =
        TaskProcfs::new(Arc::clone(&tasks)).with_seccomp(Arc::clone(&seccomp), hl_linux::SeccompBaseline::Disabled);
    let disabled = disabled_source
        .process(identity(&disabled_source, process.number()))
        .unwrap();
    assert_eq!((disabled.seccomp_mode, disabled.seccomp_filters), (0, 0));

    let container_source = TaskProcfs::new(tasks).with_seccomp(seccomp, hl_linux::SeccompBaseline::Container);
    let container = container_source
        .process(identity(&container_source, process.number()))
        .unwrap();
    assert_eq!((container.seccomp_mode, container.seccomp_filters), (2, 1));
}

#[test]
fn cmdline_is_process_image_snapshot() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    tasks
        .publish_arguments(process, vec![b"/bin/program".to_vec(), b"two words".to_vec()])
        .unwrap();
    let child = tasks
        .commit_fork_process(tasks.begin_fork_process(thread).unwrap())
        .unwrap()
        .0;
    let source = TaskProcfs::new(Arc::clone(&tasks));
    assert_eq!(
        source.cmdline(identity(&source, process.number())).unwrap(),
        b"/bin/program\0two words\0"
    );
    assert_eq!(
        source.cmdline(identity(&source, child.number())).unwrap(),
        b"/bin/program\0two words\0"
    );
}

#[test]
fn stat_joins_domains() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, _) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let source = TaskProcfs::new(tasks).with_stat(Arc::new(StatMetrics));
    let bytes = source.stat(identity(&source, process.number())).unwrap().bytes();
    let text = String::from_utf8(bytes).unwrap();
    let suffix = text.rsplit_once(") ").unwrap().1;
    let fields = suffix.split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 50);
    assert_eq!(fields[0], "R");
    assert_eq!(fields[3], "1");
    assert_eq!(fields[7..11], ["1", "2", "3", "4"]);
    assert_eq!(fields[11..15], ["5", "6", "7", "8"]);
    assert_eq!(fields[19..22], ["9", "65536", "10"]);
    assert_eq!(fields[23..25], ["4096", "8192"]);
    assert_eq!(fields[35], "17");
    assert_eq!(fields[36], "2");
    assert_eq!(
        fields[42..49],
        ["12288", "16384", "20480", "24576", "24832", "25088", "25344"]
    );
}

#[test]
fn zombie_stat_keeps_identity() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let fork = tasks.begin_fork_process(parent_thread).unwrap();
    let child = fork.process();
    let child_thread = fork.thread();
    tasks.commit_fork_process(fork).unwrap();
    tasks.set_name(child_thread, *b"dead-child\0\0\0\0\0\0").unwrap();
    tasks.exit_process(child, hl_task::ExitStatus::Code(0)).unwrap();

    let source = TaskProcfs::new(Arc::clone(&tasks)).with_stat(Arc::new(StatMetrics));
    let child_identity = identity(&source, child.number());
    let leader_identity = source.resolve_thread(child_identity, None).unwrap();
    assert_eq!(source.comm(child_identity, leader_identity).unwrap(), b"dead-child\n");
    let text = String::from_utf8(source.stat(child_identity).unwrap().bytes()).unwrap();
    assert!(text.starts_with(&format!("{} (dead-child) Z {} ", child.number(), parent.number())));
    tasks.reap(parent, child).unwrap();
    assert_eq!(source.stat(child_identity), Err(ProcfsError::NotFound));
    assert_eq!(source.comm(child_identity, leader_identity), Err(ProcfsError::NotFound));
}

#[test]
fn stat_reuse() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let (child, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    let source = TaskProcfs::new(Arc::clone(&tasks)).with_stat(Arc::new(StatMetrics));
    let resolved = child;

    tasks.exit_process(child, ExitStatus::Code(0)).unwrap();
    tasks.reap(parent, child).unwrap();
    let (replacement, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    assert_eq!(replacement.number(), resolved.number());
    assert_ne!(replacement, resolved);

    assert_eq!(source.stat_view(resolved), Err(ProcfsError::NotFound));
}

#[test]
fn live_task_identity() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let (child, child_thread) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    let source = TaskProcfs::with_descriptors(
        Arc::clone(&tasks),
        parent,
        Arc::new(DescriptorTable::new(8).unwrap()),
        Arc::new(Targets),
    )
    .with_root(b"/guest".to_vec())
    .with_fs_context(Arc::new(crate::FsContext::new(0o077)));
    let child_identity = identity(&source, child.number());
    let child_thread_identity = source
        .resolve_thread(child_identity, Some(child_thread.number()))
        .unwrap();
    assert_eq!(source.processes().unwrap(), [parent.number(), child.number()]);
    assert_eq!(
        source
            .resolve_thread(child_identity, Some(child_thread.number()))
            .map(|_| ()),
        Ok(())
    );
    assert_eq!(
        source.threads(identity(&source, child.number())).unwrap(),
        [child_thread.number()]
    );
    assert_eq!(source.root(identity(&source, child.number())).unwrap(), b"/guest");
    assert_eq!(
        source.process(identity(&source, parent.number())).unwrap().umask,
        Some(0o077)
    );
    assert_eq!(source.process(identity(&source, child.number())).unwrap().umask, None);
    assert_eq!(
        source
            .resolve_thread(identity(&source, parent.number()), Some(child_thread.number()))
            .map(|_| ()),
        Err(ProcfsError::NotFound)
    );
    assert_eq!(
        source.comm(identity(&source, parent.number()), child_thread_identity),
        Err(ProcfsError::NotFound)
    );
    let child_name = tasks
        .snapshot()
        .threads
        .into_iter()
        .find(|thread| thread.id == child_thread)
        .unwrap()
        .name;
    let (process_slot, process_generation) = parent.wire_parts();
    let (thread_slot, thread_generation) = parent_thread.wire_parts();
    let actor = hl_descriptor::OperationActor {
        process: process_slot,
        process_generation,
        thread: thread_slot,
        thread_generation,
    };
    assert_eq!(
        source.write_comm(
            identity(&source, parent.number()),
            child_thread_identity,
            actor,
            b"wrong"
        ),
        Err(hl_descriptor::ObjectError::Retired)
    );
    assert_eq!(
        tasks
            .snapshot()
            .threads
            .into_iter()
            .find(|thread| thread.id == child_thread)
            .unwrap()
            .name,
        child_name
    );
    tasks.exit_process(child, ExitStatus::Code(0)).unwrap();
    tasks.reap(parent, child).unwrap();
    assert_eq!(source.processes().unwrap(), [parent.number()]);
    assert_eq!(
        source
            .resolve_thread(child_identity, Some(child_thread.number()))
            .map(|_| ()),
        Err(ProcfsError::NotFound)
    );
}

#[test]
fn open_comm_pins_process_generation_across_pid_reuse() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let (child, child_thread) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    tasks.set_name(child_thread, *b"old-child\0\0\0\0\0\0\0").unwrap();
    let procfs = hl_vfs::Procfs::new(Arc::new(TaskProcfs::new(Arc::clone(&tasks))));
    let path = format!("/proc/{}/comm", child.number());
    let old = procfs
        .open(path.as_bytes(), parent.number(), hl_vfs::OpenIntent::default())
        .unwrap()
        .unwrap();

    tasks.exit_process(child, ExitStatus::Code(0)).unwrap();
    tasks.reap(parent, child).unwrap();
    let (replacement, replacement_thread) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    assert_eq!(replacement.number(), child.number());
    tasks.set_name(replacement_thread, *b"new-child\0\0\0\0\0\0\0").unwrap();

    assert!(old.read(&mut [0; 32]).is_err());
    let current = procfs
        .open(path.as_bytes(), parent.number(), hl_vfs::OpenIntent::default())
        .unwrap()
        .unwrap();
    let mut output = [0; 32];
    let count = current.read(&mut output).unwrap();
    assert_eq!(&output[..count], b"new-child\n");
}

#[test]
fn oom_reuse() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let (parent, parent_thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let (child, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    let procfs = hl_vfs::Procfs::new(Arc::new(TaskProcfs::new(Arc::clone(&tasks))));
    let path = format!("/proc/{}/oom_score_adj", child.number());
    let old = procfs
        .open(
            path.as_bytes(),
            parent.number(),
            hl_vfs::OpenIntent::from_bits(hl_vfs::OpenIntent::WRITE),
        )
        .unwrap()
        .unwrap();

    tasks.exit_process(child, ExitStatus::Code(0)).unwrap();
    tasks.reap(parent, child).unwrap();
    let (replacement, _) = tasks
        .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
        .unwrap();
    assert_eq!(replacement.number(), child.number());
    let (process, process_generation) = parent.wire_parts();
    let (thread, thread_generation) = parent_thread.wire_parts();
    let context = hl_descriptor::OperationContext {
        actor: Some(hl_descriptor::OperationActor {
            process,
            process_generation,
            thread,
            thread_generation,
        }),
        cancellation: None,
    };

    assert_eq!(old.read(&mut [0; 16]), Err(hl_descriptor::ObjectError::Retired));
    assert_eq!(
        old.write_context(b"500\n", context),
        Err(hl_descriptor::ObjectError::Retired)
    );
    assert_eq!(tasks.process_snapshot(replacement).unwrap().oom_score_adj, 0);
}

#[test]
fn live_working_directory() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, _) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let working = Arc::new(crate::WorkingDirectory::root());
    let source = TaskProcfs::with_descriptors(
        Arc::clone(&tasks),
        process,
        Arc::new(DescriptorTable::new(8).unwrap()),
        Arc::new(Targets),
    )
    .with_fs_context(Arc::new(crate::FsContext::default()))
    .with_working(Arc::clone(&working));
    assert_eq!(source.cwd(identity(&source, process.number())).unwrap(), b"/");
    working.replace_path("/work").unwrap();
    assert_eq!(source.cwd(identity(&source, process.number())).unwrap(), b"/work");
    working.mark_deleted();
    assert_eq!(
        source.cwd(identity(&source, process.number())).unwrap(),
        b"/work (deleted)"
    );
    assert_eq!(source.resolve_process(process.number() + 1), Err(ProcfsError::NotFound));
}

#[test]
fn comm_write() {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_threads: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let (process, leader) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let worker = tasks
        .commit_clone_thread(tasks.begin_clone_thread(leader).unwrap())
        .unwrap();
    let source = TaskProcfs::with_descriptors(
        Arc::clone(&tasks),
        process,
        Arc::new(DescriptorTable::new(8).unwrap()),
        Arc::new(Targets),
    )
    .with_fs_context(Arc::new(crate::FsContext::default()));
    let procfs = hl_vfs::Procfs::new(Arc::new(source));
    let file = procfs
        .open(
            format!("/proc/self/task/{}/comm", worker.number()).as_bytes(),
            process.number(),
            hl_vfs::OpenIntent::from_bits(hl_vfs::OpenIntent::WRITE),
        )
        .unwrap()
        .unwrap();
    let (process_slot, process_generation) = process.wire_parts();
    let (thread_slot, thread_generation) = leader.wire_parts();
    let context = hl_descriptor::OperationContext {
        actor: Some(hl_descriptor::OperationActor {
            process: process_slot,
            process_generation,
            thread: thread_slot,
            thread_generation,
        }),
        cancellation: None,
    };
    assert_eq!(file.write_context(b"worker-renamed-too-long\nignored", context), Ok(31));
    let snapshot = tasks.snapshot();
    let worker = snapshot.threads.iter().find(|thread| thread.id == worker).unwrap();
    assert_eq!(&worker.name, b"worker-renamed-\0");
    let leader = snapshot.threads.iter().find(|thread| thread.id == leader).unwrap();
    assert_ne!(&leader.name, b"worker-renamed-\0");
    let mut output = [0_u8; 32];
    let count = file.read(&mut output).unwrap();
    assert_eq!(&output[..count], b"worker-renamed-\n");
    let stale = worker.id;
    tasks.exit_thread(stale, ExitStatus::Code(0)).unwrap();
    let replacement = tasks
        .commit_clone_thread(tasks.begin_clone_thread(leader.id).unwrap())
        .unwrap();
    assert_eq!(replacement.number(), stale.number());
    assert_ne!(replacement.wire_parts(), stale.wire_parts());
    assert_eq!(file.metadata(), Err(hl_descriptor::ObjectError::Retired));
    assert_eq!(file.read(&mut output), Err(hl_descriptor::ObjectError::Retired));
    assert_eq!(
        file.write_context(b"stale", context),
        Err(hl_descriptor::ObjectError::Retired)
    );
    let replacement = tasks
        .snapshot()
        .threads
        .into_iter()
        .find(|thread| thread.id == replacement)
        .unwrap();
    assert_ne!(&replacement.name, b"stale\0\0\0\0\0\0\0\0\0\0\0");
}

#[test]
fn uts_pin() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let mut credentials = tasks.credentials(process).unwrap();
    credentials.capabilities.effective |= hl_task::CapabilitySets::SYS_ADMIN;
    tasks.replace_credentials(process, credentials).unwrap();
    let procfs = hl_vfs::Procfs::new(Arc::new(TaskProcfs::new(Arc::clone(&tasks))));
    let file = procfs
        .open(
            b"/proc/sys/kernel/hostname",
            process.number(),
            hl_vfs::OpenIntent::from_bits(hl_vfs::OpenIntent::WRITE),
        )
        .unwrap()
        .unwrap();
    let initial = tasks.namespaces(process).unwrap().uts;
    assert_eq!(
        procfs.read_link(b"/proc/self/ns/uts", process.number()).unwrap(),
        Some(format!("uts:[{}]", initial.serial).into_bytes()),
    );
    tasks.unshare_namespace(process, hl_task::NamespaceKind::Uts).unwrap();
    let replacement = tasks.namespaces(process).unwrap().uts;
    assert_ne!(replacement, initial);
    assert_eq!(
        procfs.uts_namespace(b"/proc/self/ns/uts", process.number()),
        Ok(Some(replacement.serial)),
    );
    let (process_slot, process_generation) = process.wire_parts();
    let (thread_slot, thread_generation) = thread.wire_parts();
    let context = hl_descriptor::OperationContext {
        actor: Some(hl_descriptor::OperationActor {
            process: process_slot,
            process_generation,
            thread: thread_slot,
            thread_generation,
        }),
        cancellation: None,
    };
    assert_eq!(file.write_context(b"pinned\n", context), Ok(7));
    assert_eq!(tasks.uts_namespace(initial).unwrap().hostname, b"pinned");
    assert_eq!(tasks.uts_identity(process).unwrap().hostname, b"jit");
}

#[test]
fn mount_oracle() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, _) = tasks
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
        .unwrap();
    let procfs = hl_vfs::Procfs::new(Arc::new(TaskProcfs::new(tasks)));
    let file = procfs
        .open(
            b"/proc/self/mountinfo",
            process.number(),
            hl_vfs::OpenIntent::from_bits(hl_vfs::OpenIntent::READ),
        )
        .unwrap()
        .unwrap();
    let mut output = vec![0_u8; 2048];
    let count = file.read(&mut output).unwrap();
    assert_eq!(
        &output[..count],
        b"23 0 0:24 / / rw,relatime - overlay overlay rw\n\
24 23 0:25 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n\
25 23 0:26 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755\n\
26 25 0:27 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666\n\
27 23 0:28 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro\n\
28 27 0:29 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw,nsdelegate\n\
29 25 0:30 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw\n\
30 25 0:31 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k\n"
    );
}
