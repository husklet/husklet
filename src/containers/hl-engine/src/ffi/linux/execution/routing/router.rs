//! Syscall router assembly for a routed process context.

use hl_runtime::{
    RouterDependencies, RuntimeEventSyscalls, RuntimeFilesystemSyscalls, RuntimeProcessSyscalls,
    RuntimeSeccompSyscalls, RuntimeSyscallRouter,
};
use std::sync::{Arc, Mutex, Weak};

use super::super::process_memory::ProcessMemory;
use super::super::syscall::{DescriptorPort, EventPort, FilesystemPort, MemoryPort};
use super::super::{BackingChanges, fork, network, path, ports, readiness, signal_frame, signal_source, task, watch};
use super::{ProcessContext, aio, ptrace, signal};

impl ProcessContext {
    pub(in crate::ffi::linux::execution) fn router(
        self: &Arc<Self>,
        thread: hl_task::ThreadId,
        cancellation: Arc<readiness::Cancellation>,
        clone: Option<Box<dyn hl_runtime::ThreadCloneTrapPort>>,
    ) -> RuntimeSyscallRouter {
        let epoll_table = self.files(thread);
        let table = epoll_table.descriptor_table();
        let descriptors = Arc::new(self.descriptors.fork(Arc::clone(&table)));
        let mappings = self.space.mappings();
        let arena = self.space.arena();
        let process_memory = self.space.guest_memory();
        let siblings = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|entry| entry.process == self.process && entry.id != thread)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let _ = self.seccomp.register_inheriting(thread, &siblings);
        let unshare_cancellation = Arc::clone(&cancellation);
        let interruption = cancellation.interruption();
        self.interruptions.register(thread, Arc::clone(&interruption));
        self.alarms.register_interruption(thread, interruption);
        let robust = Arc::new(hl_runtime::RobustExitHandler::new(
            Arc::clone(&self.tasks),
            Arc::clone(&mappings),
            self.futex.clone(),
        ));
        let frame = self
            .threads
            .get()
            .and_then(Weak::upgrade)
            .map(|threads| signal_frame::Port::new(threads, self.sigreturn_pc));
        let configure = |runtime: RuntimeProcessSyscalls<ProcessMemory>| {
            let mut runtime = runtime
                .with_clock(self.clock.clone())
                .with_cpu_clock(self.clock.clone())
                .with_system(Arc::clone(&self.system))
                .with_sleep_port(Arc::new(task::SleepPort(Arc::clone(&self.deadlines))))
                .with_blocking_wait(cancellation.clone())
                .with_yield_port(Arc::new(task::CooperativeYield))
                .with_futex_port(self.futex.clone())
                .with_robust_exit(robust.clone())
                .with_exit_runtime(Arc::clone(&self.exit))
                .with_reap_port(Arc::clone(&self.procfs_spaces) as Arc<dyn hl_runtime::RuntimeReapPort>)
                .with_alarms(Arc::clone(&self.alarms))
                .with_timers(Arc::clone(&self.timers))
                .with_process_handles(Arc::clone(&table), Arc::clone(&self.handles))
                .with_namespace_handles(Arc::clone(&table), Arc::clone(&self.namespace_handles))
                .with_exec_queue(Arc::clone(&self.exec_queue))
                .with_seccomp(Arc::new(
                    RuntimeSeccompSyscalls::new(
                        Arc::clone(&self.seccomp),
                        Arc::clone(&self.tasks),
                        self.process,
                        thread,
                        process_memory.clone(),
                    )
                    .with_baseline(self.seccomp_baseline),
                ))
                .with_ptrace(self.ptrace.clone());
            if let Some(exec) = self.exec.for_process(self.process) {
                runtime = runtime.with_exec_port(exec);
            }
            match &frame {
                Some(frame) => runtime.with_signal_frame(frame.clone()),
                None => runtime,
            }
        };
        let process = configure(
            RuntimeProcessSyscalls::new(
                Arc::clone(&self.tasks),
                self.process,
                thread,
                process_memory.clone(),
                self.architecture,
            )
            .with_fs_context(Arc::clone(&self.fs_context)),
        );
        let signal = frame.as_ref().map(|_| {
            configure(
                RuntimeProcessSyscalls::new(
                    Arc::clone(&self.tasks),
                    self.process,
                    thread,
                    process_memory.clone(),
                    self.architecture,
                )
                .with_fs_context(Arc::clone(&self.fs_context)),
            )
        });
        let masks = Arc::new(readiness::SignalMasks::new());
        let mut filesystem =
            RuntimeFilesystemSyscalls::new(Arc::clone(&table), process_memory.clone(), self.architecture)
                .with_fs_context(Arc::clone(&self.fs_context))
                .with_actor(self.process, thread)
                .with_advisory_locks(Arc::clone(&self.locks))
                .with_backing_changes(Arc::new(BackingChanges::new(self.space.mappings())))
                .with_socket_ioctl(self.network.socket_ioctl())
                .with_vector_terminal(Arc::new(super::super::vector::VectorAdapter::new(
                    process_memory.clone(),
                    self.network.files(),
                )))
                .with_memfds(
                    self.memory
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .memfd_registry(),
                )
                .with_working_directory(Arc::clone(&self.working))
                .with_pipe_cancellation(Arc::new(
                    hl_runtime::RuntimePipeCancellation::new(cancellation.interruption())
                        .with_signals(Arc::clone(&self.tasks), thread),
                ))
                .with_pipe_registry(Arc::clone(&self.ipc_pipes))
                .with_pipe_signal(Arc::new(signal::PipeSignal {
                    tasks: Arc::clone(&self.tasks),
                    process: self.process,
                }))
                .with_file_size_limit(Arc::new(signal::FileSizeLimit {
                    tasks: Arc::clone(&self.tasks),
                    process: self.process,
                }))
                .with_async_signal(Arc::new(signal::AsyncSignal {
                    tasks: Arc::clone(&self.tasks),
                }))
                .with_dnotify(Arc::new(signal::DnotifySignal {
                    tasks: Arc::clone(&self.tasks),
                    process: self.process,
                }));
        let mut unix_socket_paths: Option<Arc<dyn hl_runtime::UnixSocketPathPort>> = None;
        if let Some(path_host) = &self.path_host {
            let path_host = path_host.for_process(
                self.process,
                thread,
                Arc::clone(&table),
                Arc::clone(&self.working),
                Arc::clone(&self.fs_context),
                Arc::clone(&self.procfs_spaces),
                Arc::clone(&self.procfs_resources),
                self.network.clone(),
                Arc::clone(&self.seccomp),
                self.seccomp_baseline,
            );
            let paths = path::UnixSocketPaths::new(
                Arc::clone(&path_host),
                self.network.unix_namespace(),
                Arc::clone(&self.fs_context),
            );
            filesystem = filesystem
                .with_terminals(path_host.terminal_bindings())
                .with_terminal_tasks(Arc::clone(&self.tasks), self.process)
                .with_path_host(path_host)
                .with_unix_socket_paths(paths.clone());
            unix_socket_paths = Some(paths);
        }
        let filesystem = Arc::new(Mutex::new(filesystem));
        let event_objects = RuntimeEventSyscalls::new(
            Arc::clone(&table),
            Arc::clone(&self.events),
            process_memory.clone(),
            self.architecture,
        )
        .with_event_operations(Arc::clone(&self.event_operations));
        let mut event_objects = self
            .event_checkpoint
            .configure(event_objects)
            .with_epoll_control(Arc::clone(&self.epoll), Arc::clone(&epoll_table))
            .with_epoll_wait(
                Arc::clone(&self.tasks),
                thread,
                Arc::new(hl_runtime::RuntimePipeCancellation::new(cancellation.interruption())),
            )
            .with_timer_source(Arc::new(task::ClockSource(self.clock.clone())))
            .with_signal_source(Arc::new(signal_source::Source::new(Arc::clone(&self.tasks), thread)));
        if let Some(watches) = &self.watches {
            event_objects = event_objects.with_watch_source(Arc::new(watch::Provider(Arc::clone(watches))));
        }
        let network = network::runtime(
            Arc::clone(&table),
            Arc::clone(&self.tasks),
            self.process,
            process_memory.clone(),
            self.architecture,
            &self.network,
            self.network_enabled,
            Arc::new(hl_runtime::SafeNetworkWait::new(
                cancellation.interruption(),
                self.clock.clone(),
            )),
            unix_socket_paths,
        );
        let ipc = hl_runtime::RuntimeIpcSyscalls::new(
            Arc::clone(&self.ipc_catalog),
            Arc::clone(&self.tasks),
            self.process,
            process_memory.clone(),
            self.architecture,
            self.clock.clone(),
        )
        .with_memory_port(self.ipc.clone())
        .with_wait_port(cancellation.clone())
        .with_posix_queues(Arc::clone(&self.posix_queues), Arc::clone(&table));
        let aio = aio::runtime(
            self,
            Arc::clone(&table),
            process_memory.clone(),
            Arc::new(hl_runtime::RuntimePipeCancellation::new(cancellation.interruption())),
        );
        let router = RuntimeSyscallRouter::new(RouterDependencies {
            aio: Box::new(aio),
            process_fork: self
                .fork
                .get()
                .and_then(Weak::upgrade)
                .map(|runtime| Box::new(fork::Trap(runtime, thread)) as Box<dyn hl_runtime::ProcessForkTrap>),
            architecture_memory: Box::new(process_memory.clone()),
            thread_clone: clone,
            filesystem: Box::new(FilesystemPort(Arc::clone(&filesystem))),
            descriptor_io: Box::new(DescriptorPort {
                standard: ports::DescriptorPort::new(
                    Arc::clone(&arena),
                    Arc::clone(&descriptors),
                    Arc::clone(&self.entropy),
                ),
                filesystem,
                epoll: Arc::clone(&self.epoll),
                epoll_table: Arc::clone(&epoll_table),
                unshare: Arc::new(super::super::syscall::Unshare::new(
                    Arc::downgrade(self),
                    thread,
                    unshare_cancellation,
                )),
                locks: Arc::clone(&self.locks),
                process: self.process,
            }),
            event: Box::new(EventPort {
                readiness: readiness::EventPort::new(
                    Arc::clone(&arena),
                    descriptors,
                    cancellation,
                    masks,
                    Arc::clone(&self.deadlines),
                    Arc::clone(&self.tasks),
                    thread,
                ),
                objects: event_objects,
            }),
            memory: Box::new(MemoryPort(Arc::clone(&self.memory))),
            network: Box::new(network),
            task_signal_time: Box::new(task::TaskAdapter::new(process)),
            ipc: Box::new(ipc),
            seccomp: Box::new(
                RuntimeSeccompSyscalls::new(
                    Arc::clone(&self.seccomp),
                    Arc::clone(&self.tasks),
                    self.process,
                    thread,
                    process_memory,
                )
                .with_baseline(self.seccomp_baseline),
            ),
        })
        .with_seccomp_control(Arc::clone(&self.seccomp))
        .with_task_identity(self.process, thread);
        let router = router.with_exec_queue(thread, Arc::clone(&self.exec_queue));
        let router = match self.ptrace.safepoint(Arc::clone(&self.tasks), self.process) {
            Some(ptrace) => {
                if let Some(threads) = self.threads.get().cloned() {
                    self.ptrace
                        .register_wake(self.process, Arc::new(ptrace::Wake::new(threads, thread)));
                }
                router.with_ptrace(ptrace)
            }
            None => router,
        };
        let router = match signal {
            Some(signal) => router.with_signal_boundary(Box::new(signal::SignalBoundary(signal))),
            None => router,
        };
        if self.trace { router.with_trace(64) } else { router }
    }
}
