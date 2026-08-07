use std::fmt;
use std::sync::Arc;

use hl_descriptor::{
    DescriptorError, DescriptorFlags, ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};
use hl_ipc::{MqAccess, MqDescription, MqError, MqEvent as DomainMqEvent, MqOpen};
use hl_linux::{Errno, GuestMemory, LinuxResult, MqAbi, MqAttributes, MqMarshalError, MqNotify};
use hl_sync::WaitOutcome;
use hl_task::{PendingTarget, SignalInfo, SignalNumber};
use hl_time::{Deadline, Duration};

use super::RuntimeIpcSyscalls;

const O_CREAT: u32 = 0x40;
const O_EXCL: u32 = 0x80;
const O_NONBLOCK: u32 = 0x800;
const O_CLOEXEC: u32 = 0x8_0000;
const OPEN_FLAGS: u32 = 3 | O_CREAT | O_EXCL | O_NONBLOCK | O_CLOEXEC;

struct RuntimeMq {
    description: MqDescription,
}

impl fmt::Debug for RuntimeMq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RuntimeMq").finish_non_exhaustive()
    }
}

impl OpenFileDescription for RuntimeMq {
    fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn kind(&self) -> ObjectKind {
        ObjectKind::Other
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.description.readiness(interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.description.subscribe_readiness(observer)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        let _ = self
            .description
            .set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0);
        Ok(())
    }
}

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    fn mq_deadline(&self, timeout: Option<hl_linux::MqTimespec>) -> Result<Option<Deadline>, Errno> {
        let Some(timeout) = timeout else { return Ok(None) };
        let requested = timeout
            .seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(u64::from(timeout.nanoseconds)))
            .ok_or(Errno::EINVAL)?;
        let realtime = self
            .clock
            .realtime_now()
            .map_err(|_| Errno::EIO)?
            .checked_nanoseconds()
            .ok_or(Errno::EIO)?;
        let monotonic = self.clock.monotonic_now().map_err(|_| Errno::EIO)?;
        Ok(Some(monotonic.deadline_after(Duration::from_nanoseconds(
            requested.saturating_sub(realtime),
        ))))
    }

    fn with_mq<T>(
        &self,
        descriptor: u64,
        operation: impl FnOnce(&MqDescription) -> Result<T, Errno>,
    ) -> Result<T, Errno> {
        let number = i32::try_from(descriptor).map_err(|_| Errno::EBADF)?;
        let table = self.descriptors.as_ref().ok_or(Errno::ENOSYS)?;
        let lease = table.pin(number).map_err(|_| Errno::EBADF)?;
        let runtime = lease
            .object()
            .domain_extension()
            .and_then(|extension| extension.downcast_ref::<RuntimeMq>())
            .ok_or(Errno::EBADF)?;
        operation(&runtime.description)
    }

    pub(super) fn mq_open(&self, arguments: [u64; 6]) -> LinuxResult {
        let (Some(namespace), Some(descriptors)) = (&self.posix, &self.descriptors) else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = MqAbi::new(&self.memory);
        let name = match abi.name(arguments[0]) {
            Ok(name) => name,
            Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
        };
        let flags = arguments[1] as u32;
        if flags & !OPEN_FLAGS != 0 || flags & 3 == 3 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let exists = match namespace.contains(&name) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::mq_error(error)),
        };
        let geometry = if flags & O_CREAT != 0 && !exists && arguments[3] != 0 {
            let value = match abi.attributes(arguments[3]) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
            };
            let (Ok(messages), Ok(bytes)) = (
                usize::try_from(value.maximum_messages),
                usize::try_from(value.message_bytes),
            ) else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            (Some(messages), Some(bytes))
        } else {
            (None, None)
        };
        let access = match flags & 3 {
            0 => MqAccess::Read,
            1 => MqAccess::Write,
            2 => MqAccess::ReadWrite,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let reservation = match descriptors.reserve(0) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::descriptor_error(error)),
        };
        let description = match namespace.open(
            &name,
            MqOpen {
                create: flags & O_CREAT != 0,
                exclusive: flags & O_EXCL != 0,
                nonblocking: flags & O_NONBLOCK != 0,
                access,
                maximum_messages: geometry.0,
                message_bytes: geometry.1,
            },
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::mq_error(error)),
        };
        let status = StatusFlags::from_bits(flags & (3 | O_NONBLOCK));
        let local = DescriptorFlags::from_bits(u32::from(flags & O_CLOEXEC != 0));
        let descriptor = match descriptors.commit(reservation, Arc::new(RuntimeMq { description }), status, local) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::descriptor_error(error)),
        };
        LinuxResult::Value(descriptor as u64)
    }

    pub(super) fn mq_unlink(&self, arguments: [u64; 6]) -> LinuxResult {
        let Some(namespace) = &self.posix else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let name = match MqAbi::new(&self.memory).name(arguments[0]) {
            Ok(name) => name,
            Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
        };
        match namespace.unlink(&name) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(Self::mq_error(error)),
        }
    }

    pub(super) fn mq_timedsend(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = MqAbi::new(&self.memory);
        let timeout = match abi.timeout(arguments[4]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
        };
        let deadline = match self.mq_deadline(timeout) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let priority = match MqAbi::<M>::priority(arguments[3] as u32) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
        };
        let result = self.with_mq(arguments[0], |description| {
            let length = usize::try_from(arguments[2]).map_err(|_| Errno::EMSGSIZE)?;
            let bytes = abi
                .message(arguments[1], length, description.attributes().message_bytes)
                .map_err(Self::mq_marshal)?;
            loop {
                let observed = description.wait_queue().observation();
                let sent = match description.send(&bytes, priority) {
                    Ok(event) => Some(Ok(event)),
                    Err(MqError::Again) if !description.attributes().nonblocking => None,
                    Err(error) => Some(Err(Self::mq_error(error))),
                };
                let Some(sent) = sent else {
                    let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
                    let outcome = description
                        .wait_queue()
                        .wait(observed, wait.interruption().as_ref(), deadline, self.clock.as_ref())
                        .map_err(|_| Errno::EIO)?;
                    Self::mq_wait_outcome(outcome)?;
                    continue;
                };
                break sent;
            }
        });
        match result {
            Ok(Some(DomainMqEvent::Signal { owner, signal, value })) => {
                let Ok(signal) = SignalNumber::new(signal) else {
                    return LinuxResult::Error(Errno::EINVAL);
                };
                let (_, sender_process, _) = match self.context() {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(error),
                };
                let sender_user = self
                    .tasks
                    .snapshot()
                    .processes
                    .into_iter()
                    .find(|process| process.id == self.process)
                    .map_or(0, |process| process.credentials.real_user);
                let Some(target) = self
                    .tasks
                    .snapshot()
                    .processes
                    .into_iter()
                    .find(|process| process.id.number() == owner)
                    .map(|process| process.id)
                else {
                    return LinuxResult::Error(Errno::ESRCH);
                };
                let information = SignalInfo {
                    signal,
                    code: -3,
                    sender_process,
                    sender_user,
                    value,
                    ..SignalInfo::bare(signal)
                };
                match self.tasks.enqueue_signal(PendingTarget::Process(target), information) {
                    Ok(_) => LinuxResult::Value(0),
                    Err(_) => LinuxResult::Error(Errno::EIO),
                }
            }
            Ok(_) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error),
        }
    }

    pub(super) fn mq_timedreceive(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = MqAbi::new(&self.memory);
        let timeout = match abi.timeout(arguments[4]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
        };
        let deadline = match self.mq_deadline(timeout) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = self.with_mq(arguments[0], |description| {
            let capacity = usize::try_from(arguments[2]).map_err(|_| Errno::EMSGSIZE)?;
            let receipt = loop {
                let observed = description.wait_queue().observation();
                let attempt = description.receive_transactional(capacity, |receipt| {
                    abi.stage_receive(arguments[1], receipt.bytes.len(), arguments[3])
                        .map_err(|_| MqError::Fault)
                        .and_then(|staged| {
                            staged
                                .commit(&receipt.bytes, receipt.priority)
                                .map_err(|_| MqError::Fault)
                        })
                });
                let received = match attempt {
                    Ok(receipt) => Some(receipt),
                    Err(MqError::Again) if !description.attributes().nonblocking => None,
                    Err(error) => return Err(Self::mq_error(error)),
                };
                let Some(received) = received else {
                    let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
                    let outcome = description
                        .wait_queue()
                        .wait(observed, wait.interruption().as_ref(), deadline, self.clock.as_ref())
                        .map_err(|_| Errno::EIO)?;
                    Self::mq_wait_outcome(outcome)?;
                    continue;
                };
                break received;
            };
            Ok(receipt.bytes.len())
        });
        match result {
            Ok(length) => LinuxResult::Value(length as u64),
            Err(error) => LinuxResult::Error(error),
        }
    }

    pub(super) fn mq_getsetattr(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = MqAbi::new(&self.memory);
        let replacement = if arguments[1] == 0 {
            None
        } else {
            match abi.attributes(arguments[1]) {
                Ok(value) => Some(value.flags & i64::from(O_NONBLOCK) != 0),
                Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
            }
        };
        let result = self.with_mq(arguments[0], |description| {
            let old = match replacement {
                Some(nonblocking) => description.set_nonblocking(nonblocking),
                None => description.attributes(),
            };
            if arguments[2] != 0 {
                let staged = abi
                    .stage_attributes(
                        arguments[2],
                        MqAttributes {
                            flags: if old.nonblocking { i64::from(O_NONBLOCK) } else { 0 },
                            maximum_messages: old.maximum_messages as i64,
                            message_bytes: old.message_bytes as i64,
                            current_messages: old.current_messages as i64,
                        },
                    )
                    .map_err(Self::mq_marshal)?;
                staged.commit().map_err(Self::mq_marshal)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error),
        }
    }

    pub(super) fn mq_notify(&self, arguments: [u64; 6]) -> LinuxResult {
        let event = if arguments[1] == 0 {
            None
        } else {
            let value = match MqAbi::new(&self.memory).event(arguments[1]) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(Self::mq_marshal(error)),
            };
            let owner = self.process.number();
            Some(match value.notify {
                MqNotify::Signal => DomainMqEvent::Signal {
                    owner,
                    signal: value.signal as u8,
                    value: value.value,
                },
                MqNotify::None => DomainMqEvent::None { owner },
                MqNotify::Thread => DomainMqEvent::Thread {
                    owner,
                    cookie: value.value,
                },
            })
        };
        let result = self.with_mq(arguments[0], |description| match event {
            Some(event) => description.register(event).map_err(Self::mq_error),
            None => {
                description.unregister(self.process.number());
                Ok(())
            }
        });
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error),
        }
    }

    const fn mq_wait_outcome(outcome: WaitOutcome) -> Result<(), Errno> {
        match outcome {
            WaitOutcome::Notified => Ok(()),
            WaitOutcome::Interrupted => Err(Errno::EINTR),
            WaitOutcome::TimedOut => Err(Errno::ETIMEDOUT),
        }
    }

    const fn mq_marshal(error: MqMarshalError) -> Errno {
        match error {
            MqMarshalError::Fault => Errno::EFAULT,
            MqMarshalError::NameTooLong => Errno::ENAMETOOLONG,
            MqMarshalError::MessageTooBig => Errno::EMSGSIZE,
            MqMarshalError::Invalid => Errno::EINVAL,
        }
    }

    const fn mq_error(error: MqError) -> Errno {
        match error {
            MqError::InvalidName | MqError::InvalidGeometry | MqError::Priority => Errno::EINVAL,
            MqError::NameTooLong => Errno::ENAMETOOLONG,
            MqError::NotFound => Errno::ENOENT,
            MqError::Exists => Errno::EEXIST,
            MqError::Capacity => Errno::ENOSPC,
            MqError::BadAccess => Errno::EBADF,
            MqError::MessageTooBig => Errno::EMSGSIZE,
            MqError::Again => Errno::EAGAIN,
            MqError::Busy => Errno::EBUSY,
            MqError::Fault => Errno::EFAULT,
        }
    }

    const fn descriptor_error(error: DescriptorError) -> Errno {
        match error {
            DescriptorError::TooManyOpenFiles => Errno::EMFILE,
            _ => Errno::EIO,
        }
    }
}
