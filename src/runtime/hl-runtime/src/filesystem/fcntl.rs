use crate::{
    FileIdentity, LockCancellation, LockError, LockRange, ProcessLockOwner, RangeLockKind, RangeLockRequest,
    RangeWhence,
};
use hl_descriptor::CancellationNotification;
use hl_descriptor::DescriptorFlags;
use hl_linux::{Errno, GuestArchitecture, GuestMemory, LinuxResult};
use hl_vfs::FlockOwnerToken;
use std::sync::Arc;

use crate::{filesystem::errno::FileErrno, filesystem::syscalls::RuntimeFilesystemSyscalls};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn fallocate(&self, arguments: [u64; 6]) -> LinuxResult {
        let mode = arguments[1] as u32;
        let offset = arguments[2] as i64;
        let length = arguments[3] as i64;
        if offset < 0 || length <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if mode & !0x7b != 0 {
            return LinuxResult::Error(Errno::EOPNOTSUPP);
        }
        if mode & 0x02 != 0 && mode & 0x01 == 0
            || mode & 0x08 != 0 && mode != 0x08
            || mode & 0x20 != 0 && mode != 0x20
            || mode & 0x40 != 0 && mode & !(0x40 | 0x01) != 0
        {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if offset.checked_add(length).is_none() {
            return LinuxResult::Error(Errno::EFBIG);
        }
        let lease = match self.descriptors.pin(arguments[0] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if mode & (0x02 | 0x08) == 0
            && let Some(limit) = &self.file_size_limit
            && limit
                .soft_limit()
                .is_ok_and(|soft| soft != u64::MAX && (offset + length) as u64 > soft)
        {
            let _ = limit.queue_sigxfsz();
            return LinuxResult::Error(Errno::EFBIG);
        }
        match lease.allocate(hl_descriptor::AllocationRequest {
            mode,
            offset: offset as u64,
            length: length as u64,
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(hl_descriptor::ObjectError::NotSupported) => LinuxResult::Error(Errno::EOPNOTSUPP),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
    pub(super) fn fcntl(&self, descriptor: i32, command: u32, argument: u64) -> LinuxResult {
        match command {
            0 => Self::descriptor_result(self.descriptors.duplicate(
                descriptor,
                argument as i32,
                DescriptorFlags::default(),
            )),
            1 => match self.descriptors.flags(descriptor) {
                Ok(flags) => LinuxResult::Value(flags.bits() as u64),
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            },
            2 => match self
                .descriptors
                .set_flags(descriptor, DescriptorFlags::from_fcntl(argument as u32))
            {
                Ok(()) => LinuxResult::Value(0),
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            },
            3 => match self.descriptors.pin(descriptor) {
                Ok(lease) => {
                    let largefile = if self.architecture == GuestArchitecture::X86_64 {
                        0x8000
                    } else {
                        0
                    };
                    let mut status = lease.status().bits();
                    if self.architecture == GuestArchitecture::Aarch64
                        && status & hl_descriptor::StatusFlags::DIRECT != 0
                    {
                        status = status & !hl_descriptor::StatusFlags::DIRECT | 0x1_0000;
                    }
                    LinuxResult::Value((status | largefile) as u64)
                }
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            },
            4 => self.set_status(descriptor, argument as u32),
            5 | 6 | 7 | 36 | 37 | 38 => self.record_lock(descriptor, command, argument),
            8 => self.set_owner(descriptor, argument),
            9 => self.get_owner(descriptor),
            10 => self.set_signal(descriptor, argument),
            11 => self.get_signal(descriptor),
            15 => self.set_owner_ex(descriptor, argument),
            16 => self.get_owner_ex(descriptor, argument),
            1024 => self.set_lease(descriptor, argument),
            1025 => self.get_lease(descriptor),
            1026 => self.notify(descriptor, argument as u32),
            1030 => Self::descriptor_result(self.descriptors.duplicate(
                descriptor,
                argument as i32,
                DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
            )),
            1031 => self.set_pipe_capacity(descriptor, argument),
            1032 => self.pipe_capacity(descriptor),
            1033 => self.add_seals(descriptor, argument),
            1034 => self.get_seals(descriptor),
            1035 => self.get_write_hint(descriptor, argument),
            1036 => self.set_write_hint(descriptor, argument),
            _ => LinuxResult::Error(Errno::EINVAL),
        }
    }

    fn notify(&self, descriptor: i32, mask: u32) -> LinuxResult {
        const VALID: u32 = 0x8000_003f;
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        let metadata = match lease.metadata() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        if metadata.kind != 4 {
            return LinuxResult::Error(Errno::ENOTDIR);
        }
        if mask & !VALID != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if mask == 0 {
            lease.set_notify_subscription(None);
            return LinuxResult::Value(0);
        }
        let Some(port) = &self.dnotify else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        match port.arm(&lease, mask, lease.delivery_signal()) {
            Ok(subscription) => {
                lease.set_notify_subscription(Some(subscription));
                LinuxResult::Value(0)
            }
            Err(crate::DnotifyError::Invalid) => LinuxResult::Error(Errno::EINVAL),
            Err(crate::DnotifyError::NotDirectory) => LinuxResult::Error(Errno::ENOTDIR),
            Err(crate::DnotifyError::Failed) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn pin_fcntl(&self, descriptor: i32) -> Result<hl_descriptor::OperationLease, LinuxResult> {
        self.descriptors
            .pin(descriptor)
            .map_err(|error| LinuxResult::Error(FileErrno::descriptor(error)))
    }

    fn set_owner(&self, descriptor: i32, owner: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        lease.set_signal_owner(hl_descriptor::SignalOwner::from_legacy(owner as i32));
        LinuxResult::Value(0)
    }

    fn get_owner(&self, descriptor: i32) -> LinuxResult {
        match self.pin_fcntl(descriptor) {
            Ok(lease) => LinuxResult::Value(lease.signal_owner().legacy() as i64 as u64),
            Err(result) => result,
        }
    }

    fn set_owner_ex(&self, descriptor: i32, address: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        let mut raw = [0_u8; 8];
        if self.memory.read(address, &mut raw) != Ok(8) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let kind = i32::from_le_bytes(raw[0..4].try_into().unwrap());
        let identity = i32::from_le_bytes(raw[4..8].try_into().unwrap());
        if !(0..=2).contains(&kind) || identity < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        lease.set_signal_owner(match kind {
            0 => hl_descriptor::SignalOwner::Thread(identity),
            1 => hl_descriptor::SignalOwner::Process(identity),
            2 => hl_descriptor::SignalOwner::Group(identity),
            _ => unreachable!(),
        });
        LinuxResult::Value(0)
    }

    fn get_owner_ex(&self, descriptor: i32, address: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        let owner = lease.signal_owner();
        let mut raw = [0_u8; 8];
        let (kind, identity) = match owner {
            hl_descriptor::SignalOwner::Thread(identity) => (0_i32, identity),
            hl_descriptor::SignalOwner::Process(identity) => (1_i32, identity),
            hl_descriptor::SignalOwner::Group(identity) => (2_i32, identity),
        };
        raw[0..4].copy_from_slice(&kind.to_le_bytes());
        raw[4..8].copy_from_slice(&identity.to_le_bytes());
        match self.memory.write(address, &raw) {
            Ok(8) => LinuxResult::Value(0),
            _ => LinuxResult::Error(Errno::EFAULT),
        }
    }

    fn set_signal(&self, descriptor: i32, signal: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        if signal > 64 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        lease.set_delivery_signal(signal as u8);
        LinuxResult::Value(0)
    }

    fn get_signal(&self, descriptor: i32) -> LinuxResult {
        match self.pin_fcntl(descriptor) {
            Ok(lease) => LinuxResult::Value(u64::from(lease.delivery_signal())),
            Err(result) => result,
        }
    }

    fn set_lease(&self, descriptor: i32, kind: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        if kind > 2 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let metadata = match lease.metadata() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        if metadata.kind != 8 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let next = match kind {
            0 => {
                if lease.status().bits() & hl_descriptor::StatusFlags::ACCESS_MODE_MASK != 0 {
                    return LinuxResult::Error(Errno::EAGAIN);
                }
                Some(hl_descriptor::LeaseKind::Read)
            }
            1 => Some(hl_descriptor::LeaseKind::Write),
            _ => None,
        };
        lease.set_lease_kind(next);
        LinuxResult::Value(0)
    }

    fn get_lease(&self, descriptor: i32) -> LinuxResult {
        match self.pin_fcntl(descriptor) {
            Ok(lease) => LinuxResult::Value(match lease.lease_kind() {
                Some(hl_descriptor::LeaseKind::Read) => 0,
                Some(hl_descriptor::LeaseKind::Write) => 1,
                None => 2,
            }),
            Err(result) => result,
        }
    }

    fn get_write_hint(&self, descriptor: i32, address: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        match self.memory.write(address, &lease.write_life_hint().to_le_bytes()) {
            Ok(8) => LinuxResult::Value(0),
            _ => LinuxResult::Error(Errno::EFAULT),
        }
    }

    fn set_write_hint(&self, descriptor: i32, address: u64) -> LinuxResult {
        let lease = match self.pin_fcntl(descriptor) {
            Ok(value) => value,
            Err(result) => return result,
        };
        let mut raw = [0_u8; 8];
        if self.memory.read(address, &mut raw) != Ok(8) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let hint = u64::from_le_bytes(raw);
        if hint > 5 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        lease.set_write_life_hint(hint);
        LinuxResult::Value(0)
    }

    fn record_lock(&self, descriptor: i32, command: u32, address: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let mut raw = [0_u8; 32];
        if self.memory.read(address, &mut raw) != Ok(32) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let lock_type = i16::from_le_bytes(raw[0..2].try_into().unwrap());
        let whence = match i16::from_le_bytes(raw[2..4].try_into().unwrap()) {
            0 => RangeWhence::Start,
            1 => RangeWhence::Current,
            2 => RangeWhence::End,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let kind = match lock_type {
            0 => Some(RangeLockKind::Read),
            1 => Some(RangeLockKind::Write),
            2 => None,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let metadata = match lease.metadata() {
            Ok(value) if value.kind == 8 => value,
            Ok(_) => return LinuxResult::Error(Errno::EBADF),
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let current = match lease.seek(hl_descriptor::SeekPosition::Current(0)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let request = RangeLockRequest {
            kind: kind.unwrap_or(RangeLockKind::Read),
            whence,
            start: i64::from_le_bytes(raw[8..16].try_into().unwrap()),
            length: i64::from_le_bytes(raw[16..24].try_into().unwrap()),
        };
        let range = match LockRange::normalize(request, current, metadata.size) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::lock_errno(error)),
        };
        let Some(actor) = self.actor else { return LinuxResult::Error(Errno::EIO) };
        let owner = if command >= 36 {
            let identity = lease.description_identity();
            ProcessLockOwner::open_file(FlockOwnerToken {
                identity: identity.identity,
                generation: identity.generation,
            })
        } else {
            ProcessLockOwner {
                identity: u64::from(actor.process),
                generation: u32::from(actor.process_generation),
            }
        };
        let Some(locks) = &self.locks else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let file = FileIdentity {
            device: metadata.device,
            inode: metadata.inode,
        };
        if command == 5 || command == 36 {
            let Some(kind) = kind else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            if let Some(conflict) = locks.query_range(file, owner, kind, range) {
                raw[0..2].copy_from_slice(
                    &(match conflict.kind {
                        RangeLockKind::Read => 0_i16,
                        RangeLockKind::Write => 1,
                    })
                    .to_le_bytes(),
                );
                raw[2..4].copy_from_slice(&0_i16.to_le_bytes());
                raw[8..16].copy_from_slice(&(conflict.range.start as i64).to_le_bytes());
                let length = conflict
                    .range
                    .end
                    .map_or(0, |end| end.saturating_sub(conflict.range.start) as i64);
                raw[16..24].copy_from_slice(&length.to_le_bytes());
                let process = if conflict.owner.is_open_file() {
                    -1
                } else {
                    conflict.owner.identity as i32
                };
                raw[24..28].copy_from_slice(&process.to_le_bytes());
            } else {
                raw[0..2].copy_from_slice(&2_i16.to_le_bytes());
            }
            return match self.memory.write(address, &raw) {
                Ok(32) => LinuxResult::Value(0),
                _ => LinuxResult::Error(Errno::EFAULT),
            };
        }
        let cancellation = Arc::new(LockCancellation::default());
        let observation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let subscription = observation.map(|observation| {
            observation.subscribe(Arc::new(LockWake {
                locks: Arc::clone(locks),
                cancellation: Arc::clone(&cancellation),
            }))
        });
        if observation.is_some_and(|value| value.interrupted()) {
            locks.interrupt(&cancellation);
        }
        let result = locks.set_range(file, owner, kind, range, command == 7 || command == 38, &cancellation);
        drop(subscription);
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(Self::lock_errno(error)),
        }
    }

    fn lock_errno(error: LockError) -> Errno {
        match error {
            LockError::WouldBlock => Errno::EAGAIN,
            LockError::Deadlock => Errno::EDEADLK,
            LockError::Interrupted | LockError::Canceled => Errno::EINTR,
            LockError::ResourceLimit => Errno::ENOLCK,
            LockError::InvalidArgument | LockError::Overflow => Errno::EINVAL,
            LockError::ConcurrentMutation => Errno::EBUSY,
        }
    }

    fn set_status(&self, descriptor: i32, requested: u32) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let requested = if self.architecture == GuestArchitecture::Aarch64 {
            requested & !0x1_0000
                | if requested & 0x1_0000 != 0 {
                    hl_descriptor::StatusFlags::DIRECT
                } else {
                    0
                }
        } else {
            requested
        };
        let previous = lease.status();
        let status = previous.update_from_fcntl(requested);
        let was_async = previous.contains(hl_descriptor::StatusFlags::ASYNC);
        let is_async = status.contains(hl_descriptor::StatusFlags::ASYNC);
        if !was_async && is_async {
            let Some(port) = &self.async_signal else {
                return LinuxResult::Error(Errno::ENOSYS);
            };
            let observer = Arc::new(AsyncObserver {
                source: lease.signal_source(),
                port: Arc::clone(port),
            });
            if let Err(error) = lease.set_async_observer(Some(observer)) {
                return LinuxResult::Error(FileErrno::object(error));
            }
        }
        if let Err(error) = lease.set_status(status) {
            if !was_async && is_async {
                let _ = lease.set_async_observer(None);
            }
            return LinuxResult::Error(FileErrno::object(error));
        }
        if was_async && !is_async {
            let _ = lease.set_async_observer(None);
        }
        LinuxResult::Value(0)
    }
}

struct AsyncObserver {
    source: hl_descriptor::SignalSource,
    port: Arc<dyn crate::AsyncSignalPort>,
}

impl hl_descriptor::ReadinessObserver for AsyncObserver {
    fn readiness_changed(&self) {
        let _ = self.port.deliver(self.source.clone());
    }
}

struct LockWake {
    locks: Arc<crate::AdvisoryLockCoordinator>,
    cancellation: Arc<LockCancellation>,
}

impl CancellationNotification for LockWake {
    fn notify(&self) {
        self.locks.interrupt(&self.cancellation);
    }
}
