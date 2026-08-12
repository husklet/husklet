use std::sync::Arc;

use hl_descriptor::CancellationNotification;
use hl_linux::{Errno, GuestMemory, LinuxResult};
use hl_vfs::FlockOwnerToken;

use crate::{
    FileIdentity, LockCancellation, LockError, LockRange, ProcessLockOwner, RangeLockKind, RangeLockRequest,
    RangeWhence,
    filesystem::{errno::FileErrno, syscalls::RuntimeFilesystemSyscalls},
};

use super::LockWake;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn record_lock(&self, descriptor: i32, command: u32, address: u64) -> LinuxResult {
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
        let Some(actor) = self.actor else {
            return LinuxResult::Error(Errno::EIO);
        };
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
        // The raw host (dev, ino) a copy-up splits in two; the coordinator holds
        // the per-container translation that puts both halves back on one file.
        let file = FileIdentity {
            device: metadata.device,
            inode: metadata.inode,
        };
        // Mount provenance carried from open: true only for bind/volume files,
        // whose inode another container in this daemon can also hold open.
        let shared = lease.shared_domain();
        if command == 5 || command == 36 {
            let Some(kind) = kind else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            if let Some(conflict) = locks.query_range(file, owner, kind, range, shared) {
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
        if observation.is_some_and(hl_descriptor::OperationCancellation::interrupted) {
            locks.interrupt(&cancellation);
        }
        let result = locks.set_range(
            file,
            owner,
            kind,
            range,
            command == 7 || command == 38,
            shared,
            &cancellation,
        );
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
}
