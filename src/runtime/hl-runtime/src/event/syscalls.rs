use crate::{
    Control, EventObjectBindings, EventResourceRegistry, OperationRegistry, RuntimeDescriptorTable, SignalEventSource,
    TimerEventSource, WatchEventSource, event::catalog::CatalogBoundEvent, event::errno::ErrorMap,
    event::timer::TimerOperations,
};
use hl_descriptor::{DescriptorFlags, DescriptorTable, OperationCancellation, StatusFlags};
use hl_event::{Epoll, EventCatalog, EventFd, Inotify, InotifyLimits, SignalFd, TimerFd};
use hl_linux::{Errno, EventAbi, EventSyscalls, GuestArchitecture, GuestMemory, LinuxResult, SyscallOperation};
use std::sync::Arc;
pub struct RuntimeEventSyscalls<M: GuestMemory> {
    pub(super) descriptors: Arc<DescriptorTable>,
    pub(super) catalog: Arc<EventCatalog>,
    pub(super) memory: M,
    pub(super) architecture: GuestArchitecture,
    pub(super) epoll: Option<(Arc<Control>, Arc<RuntimeDescriptorTable>)>,
    operations: Arc<OperationRegistry>,
    timer_source: Option<Arc<dyn TimerEventSource>>,
    signal_source: Option<Arc<dyn SignalEventSource>>,
    watch_source: Option<Arc<dyn WatchEventSource>>,
    pub(super) checkpoint: Option<(Arc<EventObjectBindings>, Arc<EventResourceRegistry>)>,
    pub(super) wait: Option<EpollWaitContext>,
}

pub(super) struct EpollWaitContext {
    pub(super) tasks: Arc<hl_task::TaskRegistry>,
    pub(super) thread: hl_task::ThreadId,
    pub(super) cancellation: Arc<dyn OperationCancellation>,
}
impl<M: GuestMemory> RuntimeEventSyscalls<M> {
    #[must_use]
    pub fn new(
        descriptors: Arc<DescriptorTable>,
        catalog: Arc<EventCatalog>,
        memory: M,
        architecture: GuestArchitecture,
    ) -> Self {
        Self {
            descriptors,
            catalog,
            memory,
            architecture,
            epoll: None,
            operations: Arc::new(OperationRegistry::new()),
            timer_source: None,
            signal_source: None,
            watch_source: None,
            checkpoint: None,
            wait: None,
        }
    }
    #[must_use]
    pub fn with_event_operations(mut self, operations: Arc<OperationRegistry>) -> Self {
        self.operations = operations;
        self
    }
    #[must_use]
    pub fn with_event_sources(
        mut self,
        timer: Arc<dyn TimerEventSource>,
        signal: Arc<dyn SignalEventSource>,
        watch: Arc<dyn WatchEventSource>,
    ) -> Self {
        self.timer_source = Some(timer);
        self.signal_source = Some(signal);
        self.watch_source = Some(watch);
        self
    }
    #[must_use]
    pub fn with_timer_source(mut self, timer: Arc<dyn TimerEventSource>) -> Self {
        self.timer_source = Some(timer);
        self
    }
    #[must_use]
    pub fn with_signal_source(mut self, signal: Arc<dyn SignalEventSource>) -> Self {
        self.signal_source = Some(signal);
        self
    }
    #[must_use]
    pub fn with_watch_source(mut self, watch: Arc<dyn WatchEventSource>) -> Self {
        self.watch_source = Some(watch);
        self
    }
    #[must_use]
    pub fn with_checkpoint_resources(
        mut self,
        bindings: Arc<EventObjectBindings>,
        resources: Arc<EventResourceRegistry>,
    ) -> Self {
        self.checkpoint = Some((bindings, resources));
        self
    }
    #[must_use]
    pub fn with_epoll_control(mut self, control: Arc<Control>, table: Arc<RuntimeDescriptorTable>) -> Self {
        self.epoll = Some((control, table));
        self
    }

    #[must_use]
    pub fn with_epoll_wait(
        mut self,
        tasks: Arc<hl_task::TaskRegistry>,
        thread: hl_task::ThreadId,
        cancellation: Arc<dyn OperationCancellation>,
    ) -> Self {
        self.wait = Some(EpollWaitContext {
            tasks,
            thread,
            cancellation,
        });
        self
    }

    fn eventfd(&self, initial: u32, flags: u32) -> LinuxResult {
        let (initial, object_flags, creation) = match EventAbi::<M>::eventfd2(initial, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let object = match EventFd::new(initial, object_flags) {
            Ok(object) => Arc::new(object),
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let local = DescriptorFlags::from_bits(if creation.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let status = StatusFlags::from_bits(
            2 | if creation.nonblocking {
                StatusFlags::NONBLOCKING
            } else {
                0
            },
        );
        let bound = Arc::new(CatalogBoundEvent::new(object.clone(), self.catalog.clone()));
        let install = match self.descriptors.prepare_open(0, bound.clone(), status, local) {
            Ok(install) => install,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let id = match self.catalog.insert_eventfd(object) {
            Ok(id) => id,
            Err(_) => return LinuxResult::Error(Errno::ENFILE),
        };
        if self
            .bind_checkpoint(&bound, install.description_identity(), id)
            .is_err()
        {
            let _ = self.catalog.remove(id);
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind(id);
        LinuxResult::Value(install.publish() as u64)
    }

    fn epoll_create(&self, flags: u32) -> LinuxResult {
        let creation = match EventAbi::<M>::epoll_create1(flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let object = Arc::new(Epoll::new());
        let local = DescriptorFlags::from_bits(if creation.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let bound = Arc::new(CatalogBoundEvent::new(object.clone(), self.catalog.clone()));
        let install = match self
            .descriptors
            .prepare_open(0, bound.clone(), StatusFlags::default(), local)
        {
            Ok(install) => install,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        if let Some((control, _)) = &self.epoll {
            let identity = install.description_identity();
            control.register_epoll(identity, object.clone());
            bound.bind_epoll(control.clone(), identity);
        }
        let id = match self.catalog.insert_epoll(object, Vec::new()) {
            Ok(id) => id,
            Err(_) => return LinuxResult::Error(Errno::ENFILE),
        };
        if self
            .bind_checkpoint(&bound, install.description_identity(), id)
            .is_err()
        {
            let _ = self.catalog.remove(id);
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind(id);
        LinuxResult::Value(install.publish() as u64)
    }

    fn epoll_create_legacy(&self, size: i32) -> LinuxResult {
        if size <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.epoll_create(0)
    }

    fn timerfd_create(&self, clock: i32, flags: u32) -> LinuxResult {
        let Some(source) = &self.timer_source else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let (clock, timer_flags, creation) = match EventAbi::<M>::timerfd_create(clock, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let (resource, source) = match source.clock() {
            Ok(source) => source,
            Err(_) => return LinuxResult::Error(Errno::ENOSYS),
        };
        if let Some((_, resources)) = &self.checkpoint
            && resources.register_clock(resource, source.clone()).is_err()
        {
            return LinuxResult::Error(Errno::ENFILE);
        }
        let object = match TimerFd::new(clock, timer_flags, source) {
            Ok(object) => Arc::new(object),
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let local = DescriptorFlags::from_bits(if creation.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let status = StatusFlags::from_bits(if creation.nonblocking {
            StatusFlags::NONBLOCKING
        } else {
            0
        });
        let bound = Arc::new(CatalogBoundEvent::new(object.clone(), self.catalog.clone()));
        let install = match self.descriptors.prepare_open(0, bound.clone(), status, local) {
            Ok(install) => install,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let identity = install.description_identity();
        if self.operations.register_timer(identity, object.clone()).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind_operations(self.operations.clone(), identity);
        let id = match self.catalog.insert_timerfd(object, resource) {
            Ok(id) => id,
            Err(_) => {
                self.operations.retire(identity);
                return LinuxResult::Error(Errno::ENFILE);
            }
        };
        if self.bind_checkpoint(&bound, identity, id).is_err() {
            self.operations.retire(identity);
            let _ = self.catalog.remove(id);
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind(id);
        LinuxResult::Value(install.publish() as u64)
    }

    fn signalfd(&self, descriptor: i32, mask: u64, size: usize, flags: u32) -> LinuxResult {
        let abi = EventAbi::new(&self.memory, self.architecture);
        let (descriptor, mask, object_flags, creation) = match abi.signalfd4(descriptor, mask, size, flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        if descriptor >= 0 {
            let lease = match self.descriptors.pin(descriptor) {
                Ok(lease) => lease,
                Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
            };
            let object = match self.operations.signal(lease.description_identity()) {
                Ok(object) => object,
                Err(_) => return LinuxResult::Error(Errno::EINVAL),
            };
            return match object.set_mask(mask) {
                Ok(()) => LinuxResult::Value(descriptor as u64),
                Err(_) => LinuxResult::Error(Errno::EINVAL),
            };
        }
        let Some(source) = &self.signal_source else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let (resource, queue) = match source.queue() {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ENOSYS),
        };
        if let Some((_, resources)) = &self.checkpoint
            && resources.register_signal(resource, queue.clone()).is_err()
        {
            return LinuxResult::Error(Errno::ENFILE);
        }
        let object = match SignalFd::new(mask, object_flags, queue) {
            Ok(value) => Arc::new(value),
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let bound = Arc::new(CatalogBoundEvent::new(object.clone(), self.catalog.clone()));
        let local = DescriptorFlags::from_bits(if creation.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let status = StatusFlags::from_bits(if creation.nonblocking {
            StatusFlags::NONBLOCKING
        } else {
            0
        });
        let install = match self.descriptors.prepare_open(0, bound.clone(), status, local) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let identity = install.description_identity();
        if self.operations.register_signal(identity, object.clone()).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind_operations(self.operations.clone(), identity);
        let id = match self.catalog.insert_signalfd(object, resource) {
            Ok(value) => value,
            Err(_) => {
                self.operations.retire(identity);
                return LinuxResult::Error(Errno::ENFILE);
            }
        };
        if self.bind_checkpoint(&bound, identity, id).is_err() {
            self.operations.retire(identity);
            let _ = self.catalog.remove(id);
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind(id);
        LinuxResult::Value(install.publish() as u64)
    }

    fn inotify_init(&self, flags: u32) -> LinuxResult {
        let creation = match EventAbi::<M>::inotify_init1(flags) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let Some(source) = &self.watch_source else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let (resource, source) = match source.watches() {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ENOSYS),
        };
        if let Some((_, resources)) = &self.checkpoint
            && resources.register_watch(resource, source.clone()).is_err()
        {
            return LinuxResult::Error(Errno::ENFILE);
        }
        let object = match Inotify::new(creation.nonblocking, InotifyLimits::default(), source) {
            Ok(value) => Arc::new(value),
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let bound = Arc::new(CatalogBoundEvent::new(object.clone(), self.catalog.clone()));
        let local = DescriptorFlags::from_bits(if creation.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let status = StatusFlags::from_bits(if creation.nonblocking {
            StatusFlags::NONBLOCKING
        } else {
            0
        });
        let install = match self.descriptors.prepare_open(0, bound.clone(), status, local) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let identity = install.description_identity();
        if self.operations.register_watch(identity, object.clone()).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind_operations(self.operations.clone(), identity);
        let id = match self.catalog.insert_inotify(object, resource, Vec::new()) {
            Ok(value) => value,
            Err(_) => {
                self.operations.retire(identity);
                return LinuxResult::Error(Errno::ENFILE);
            }
        };
        if self.bind_checkpoint(&bound, identity, id).is_err()
            || self
                .checkpoint
                .as_ref()
                .is_some_and(|(bindings, _)| bindings.register_inotify_source(identity.identity, resource).is_err())
        {
            self.operations.retire(identity);
            let _ = self.catalog.remove(id);
            return LinuxResult::Error(Errno::ENFILE);
        }
        bound.bind(id);
        LinuxResult::Value(install.publish() as u64)
    }

    fn inotify_watch(&self, descriptor: i32, path: u64, mask: u32) -> LinuxResult {
        let plan = match EventAbi::new(&self.memory, self.architecture).inotify_add_watch(path, mask) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let object = match self.operations.watch(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        match object.add_watch(&plan.path, plan.mask) {
            Ok(value)
                if self
                    .refresh_inotify_checkpoint(lease.description_identity(), &object)
                    .is_ok() =>
            {
                LinuxResult::Value(value as u64)
            }
            Ok(_) => LinuxResult::Error(Errno::EIO),
            Err(error) => LinuxResult::Error(Self::inotify_errno(error)),
        }
    }

    fn inotify_remove(&self, descriptor: i32, watch: i32) -> LinuxResult {
        let watch = match EventAbi::<M>::inotify_remove_watch(watch) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let object = match self.operations.watch(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        match object.remove_watch(watch) {
            Ok(())
                if self
                    .refresh_inotify_checkpoint(lease.description_identity(), &object)
                    .is_ok() =>
            {
                LinuxResult::Value(0)
            }
            Ok(()) => LinuxResult::Error(Errno::EIO),
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }

    const fn inotify_errno(error: hl_event::InotifyError) -> Errno {
        match error {
            hl_event::InotifyError::InvalidArgument => Errno::EINVAL,
            hl_event::InotifyError::WouldBlock => Errno::EAGAIN,
            hl_event::InotifyError::AlreadyExists => Errno::EEXIST,
            hl_event::InotifyError::NotFound => Errno::ENOENT,
            hl_event::InotifyError::NotDirectory => Errno::ENOTDIR,
            hl_event::InotifyError::NameTooLong => Errno::ENAMETOOLONG,
            hl_event::InotifyError::PermissionDenied => Errno::EACCES,
            hl_event::InotifyError::ResourceLimit => Errno::ENOMEM,
            hl_event::InotifyError::Interrupted => Errno::EINTR,
            hl_event::InotifyError::NotSupported => Errno::ENOSYS,
            hl_event::InotifyError::Retired | hl_event::InotifyError::SourceFailed => Errno::EIO,
        }
    }
}

impl<M: GuestMemory> EventSyscalls for RuntimeEventSyscalls<M> {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        let _abi_owner = (&self.memory, self.architecture);
        match operation.name {
            "eventfd" => self.eventfd(arguments[0] as u32, 0),
            "eventfd2" => self.eventfd(arguments[0] as u32, arguments[1] as u32),
            "epoll_create" => self.epoll_create_legacy(arguments[0] as i32),
            "epoll_create1" => self.epoll_create(arguments[0] as u32),
            "timerfd_create" => self.timerfd_create(arguments[0] as i32, arguments[1] as u32),
            "timerfd_gettime" => TimerOperations {
                descriptors: self.descriptors.clone(),
                operations: self.operations.clone(),
                memory: &self.memory,
                architecture: self.architecture,
            }
            .gettime(arguments[0] as i32, arguments[1]),
            "timerfd_settime" => TimerOperations {
                descriptors: self.descriptors.clone(),
                operations: self.operations.clone(),
                memory: &self.memory,
                architecture: self.architecture,
            }
            .settime(arguments),
            "signalfd4" => self.signalfd(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as usize,
                arguments[3] as u32,
            ),
            "inotify_init1" => self.inotify_init(arguments[0] as u32),
            "inotify_add_watch" => self.inotify_watch(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "inotify_rm_watch" => self.inotify_remove(arguments[0] as i32, arguments[1] as i32),
            "epoll_ctl" => self.epoll_control(arguments),
            "epoll_wait" | "epoll_pwait" | "epoll_pwait2" => self.epoll_wait(operation.name, arguments),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}

#[cfg(test)]
#[path = "syscalls_test.rs"]
mod tests;

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod lifecycle_tests;
