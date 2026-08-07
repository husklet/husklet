use hl_linux::{EpollOperation, Errno, EventAbi, GuestMemory, LinuxResult};

use super::errno::ErrorMap;
use super::syscalls::RuntimeEventSyscalls;
use crate::{Control, RuntimeDescriptorTable};

impl<M: GuestMemory> RuntimeEventSyscalls<M> {
    pub(super) fn epoll_control(&self, arguments: [u64; 6]) -> LinuxResult {
        let Some((control, table)) = &self.epoll else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = EventAbi::new(&self.memory, self.architecture);
        let operation = arguments[1] as i32;
        let target_number = arguments[2] as i32;
        let event = match abi.epoll_control_event(operation, arguments[3]) {
            Ok(event) => event,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        if let Err(error) = control.admit_control(table, arguments[0] as i32, target_number) {
            return LinuxResult::Error(Self::control_errno(error));
        }
        let plan = match EventAbi::<M>::epoll_control_plan(operation, target_number, event) {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let result = match plan.operation {
            EpollOperation::Add => control
                .add(
                    table,
                    arguments[0] as i32,
                    plan.descriptor,
                    plan.interests.expect("add carries interests"),
                    plan.data.expect("add carries data"),
                )
                .and_then(|key| {
                    self.add_epoll_checkpoint(arguments[0] as i32, key)
                        .map_err(|()| crate::ControlError::Epoll(hl_event::EpollError::InvalidArgument))?;
                    Ok(())
                }),
            EpollOperation::Modify => control.modify(
                table,
                arguments[0] as i32,
                plan.descriptor,
                plan.interests.expect("modify carries interests"),
                plan.data.expect("modify carries data"),
            ),
            EpollOperation::Delete => {
                let key = table
                    .descriptor_table()
                    .pin(plan.descriptor)
                    .map(|target| hl_event::EpollWatchKey {
                        descriptor_number: target.descriptor_number(),
                        descriptor_generation: target.descriptor_generation(),
                        description: target.description_identity(),
                    });
                control
                    .delete(table, arguments[0] as i32, plan.descriptor)
                    .and_then(|()| {
                        self.remove_epoll_checkpoint(arguments[0] as i32, key.map_err(crate::ControlError::Descriptor)?)
                            .map_err(|()| crate::ControlError::Epoll(hl_event::EpollError::InvalidArgument))
                    })
            }
        };
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(Self::control_errno(error)),
        }
    }

    pub(super) fn epoll_wait(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let Some((control, table)) = &self.epoll else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = EventAbi::new(&self.memory, self.architecture);
        let plan = if name == "epoll_pwait2" {
            abi.epoll_pwait2(
                arguments[1],
                arguments[2] as i32,
                arguments[3],
                arguments[4],
                arguments[5] as usize,
            )
        } else {
            abi.epoll_wait(
                arguments[1],
                arguments[2] as i32,
                arguments[3] as i32,
                if name == "epoll_pwait" { arguments[4] } else { 0 },
                if name == "epoll_pwait" {
                    arguments[5] as usize
                } else {
                    8
                },
            )
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let timeout = plan.timeout_nanoseconds.map(std::time::Duration::from_nanos);
        let mut initial_timeout = Some(timeout);
        loop {
            let wait_timeout = initial_timeout.take().unwrap_or(Some(std::time::Duration::ZERO));
            let batch = match self.peek_epoll(
                control,
                table,
                arguments[0] as i32,
                plan.maximum,
                wait_timeout,
                plan.signal_mask,
            ) {
                Ok(batch) => batch,
                Err(error) => return LinuxResult::Error(Self::control_errno(error)),
            };
            let count = batch.events().len();
            let staged = match abi.stage_epoll_events(&plan, batch.events()) {
                Ok(staged) => staged,
                Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
            };
            if let Err(error) = staged.commit(&hl_linux::GuestMarshaller::new(&self.memory, self.architecture)) {
                return LinuxResult::Error(ErrorMap::marshal(error));
            }
            match control.commit_wait(batch) {
                Ok(true) => return LinuxResult::Value(count as u64),
                Ok(false) => {}
                Err(error) => return LinuxResult::Error(Self::control_errno(error)),
            }
        }
    }

    fn peek_epoll(
        &self,
        control: &Control,
        table: &RuntimeDescriptorTable,
        descriptor: i32,
        maximum: usize,
        timeout: Option<std::time::Duration>,
        temporary: Option<hl_event::SignalMask>,
    ) -> Result<crate::epoll::RuntimeEpollBatch, crate::ControlError> {
        let Some(wait) = &self.wait else {
            return control.peek_wait(table, descriptor, maximum, timeout);
        };
        let previous = temporary
            .map(|mask| {
                wait.tasks
                    .replace_signal_mask(wait.thread, hl_task::SignalMask::from_bits(mask.bits()))
            })
            .transpose()
            .map_err(|_| crate::ControlError::Epoll(hl_event::EpollError::Interrupted))?;
        let result = control.peek_wait_interruptible(table, descriptor, maximum, timeout, wait.cancellation.as_ref());
        if let Some(previous) = previous
            && wait.tasks.replace_signal_mask(wait.thread, previous).is_err()
        {
            return Err(crate::ControlError::Epoll(hl_event::EpollError::Interrupted));
        }
        result
    }

    fn control_errno(error: crate::ControlError) -> Errno {
        match error {
            crate::ControlError::Descriptor(error) => crate::filesystem::FilesystemErrno::descriptor(error),
            crate::ControlError::Graph(crate::GraphError::Loop) => Errno::ELOOP,
            crate::ControlError::Graph(crate::GraphError::Event(error)) | crate::ControlError::Epoll(error) => {
                match error {
                    hl_event::EpollError::AlreadyExists => Errno::EEXIST,
                    hl_event::EpollError::NotFound => Errno::ENOENT,
                    hl_event::EpollError::TargetUnavailable => Errno::EPERM,
                    hl_event::EpollError::ResourceLimit => Errno::ENOSPC,
                    hl_event::EpollError::Interrupted => Errno::EINTR,
                    _ => Errno::EINVAL,
                }
            }
            crate::ControlError::Graph(crate::GraphError::ResourceLimit) => Errno::ENOSPC,
            _ => Errno::EINVAL,
        }
    }
}
