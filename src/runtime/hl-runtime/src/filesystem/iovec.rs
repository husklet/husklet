//! Scatter/gather descriptor I/O for the filesystem syscall surface.

use hl_descriptor::ObjectError;
use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, IovecPlan, LinuxResult, VectorTransfer};

use super::errno::FileErrno;
use super::syscalls::RuntimeFilesystemSyscalls;
use super::{VectorDirection, VectorError, VectorPosition, VectorRequest};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn vector_io(&self, descriptor: i32, address: u64, raw_count: u64, reading: bool) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let Ok(count) = usize::try_from(raw_count) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let access = if reading { GuestAccess::Write } else { GuestAccess::Read };
        let plan = match marshaller.io_vector_records(address, count, access) {
            Ok(plan) => plan,
            Err(error) => {
                // Linux resolves the descriptor and its access mode before importing the
                // vector, so EBADF outranks every marshalling errno, not just EFAULT.
                if Self::access_rejects(&lease, reading) {
                    return LinuxResult::Error(Errno::EBADF);
                }
                return LinuxResult::Error(error.errno());
            }
        };
        self.execute_vector(&lease, &marshaller, plan, reading, None, None)
    }
    fn raise_sigpipe(&self) {
        if let Some(signal) = &self.pipe_signal {
            let _ = signal.queue_sigpipe();
        }
    }

    fn truncate_vectors(plan: &mut IovecPlan, allowed: u64) {
        if allowed >= plan.total_length {
            return;
        }
        let mut remaining = allowed;
        for vector in &mut plan.vectors {
            vector.length = vector.length.min(remaining);
            remaining -= vector.length;
        }
        plan.total_length = allowed;
    }
    pub(super) fn execute_vector(
        &self,
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        mut plan: IovecPlan,
        reading: bool,
        offset: Option<u64>,
        flags: Option<u32>,
    ) -> LinuxResult {
        if plan.vectors.is_empty() {
            return if Self::access_rejects(lease, reading) {
                LinuxResult::Error(Errno::EBADF)
            } else if flags.is_some_and(|value| value != 0) {
                LinuxResult::Error(Errno::EOPNOTSUPP)
            } else {
                LinuxResult::Value(0)
            };
        }
        if !reading {
            let Ok(maximum) = usize::try_from(plan.total_length) else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            let allowed = match self.limit_write(lease, offset, maximum) {
                Ok(value) => value as u64,
                Err(error) => return error,
            };
            Self::truncate_vectors(&mut plan, allowed);
        }
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let context = hl_descriptor::OperationContext {
            actor: self.actor,
            cancellation,
        };
        let atomic_write = !reading
            && lease
                .atomic_write_limit()
                .is_some_and(|limit| plan.total_length <= limit as u64);
        if !atomic_write && let Some(terminal) = &self.vector_terminal {
            let request = VectorRequest {
                vectors: &plan.vectors,
                direction: if reading {
                    VectorDirection::Read
                } else {
                    VectorDirection::Write
                },
                position: offset.map_or(VectorPosition::Shared, VectorPosition::At),
                flags,
            };
            match terminal.execute(lease, request) {
                Ok(count) => return LinuxResult::Value(count as u64),
                Err(VectorError::Unsupported) => {}
                Err(VectorError::Fault) if reading => return Self::failed_read_probe(lease),
                Err(VectorError::Fault) => return LinuxResult::Error(Errno::EFAULT),
                Err(VectorError::Errno(Errno::EFAULT)) if reading => return Self::failed_read_probe(lease),
                Err(VectorError::Errno(errno)) => return LinuxResult::Error(errno),
                Err(VectorError::Object(ObjectError::BrokenPipe)) => {
                    self.raise_sigpipe();
                    return LinuxResult::Error(Errno::EPIPE);
                }
                Err(VectorError::Object(error)) => return LinuxResult::Error(FileErrno::object(error)),
            }
        }
        let access = if reading { GuestAccess::Write } else { GuestAccess::Read };
        let plan = match plan.validate_io(access) {
            Ok(plan) => plan,
            Err(hl_linux::MarshalError::Fault(_)) if reading => return Self::failed_read_probe(lease),
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if flags.is_some_and(|value| value != 0) {
            return LinuxResult::Error(Errno::EOPNOTSUPP);
        }
        if reading {
            return Self::read_vector(lease, marshaller, plan, offset, context);
        }
        self.write_vector(lease, marshaller, plan, offset, context, atomic_write)
    }

    pub(super) fn access_rejects(lease: &hl_descriptor::OperationLease, reading: bool) -> bool {
        if lease.status().contains(hl_descriptor::StatusFlags::PATH_ONLY) {
            return true;
        }
        let mode = lease.status().bits() & hl_descriptor::StatusFlags::ACCESS_MODE_MASK;
        if reading { mode == 1 } else { mode == 0 }
    }

    /// A description without a vectored read path would otherwise fill only the first
    /// segment, so gather one read into a bounce buffer and scatter it across every
    /// segment the way the kernel does.
    fn scatter_read(
        lease: &hl_descriptor::OperationLease,
        output: &mut [std::io::IoSliceMut<'_>],
        offset: Option<u64>,
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let capacity = output
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        if capacity == 0 {
            return Ok(0);
        }
        let mut buffer = vec![0_u8; capacity];
        let count = match offset {
            Some(position) => lease.read_at(position, &mut buffer)?,
            None => lease.read_context(&mut buffer, context)?,
        };
        let mut copied = 0;
        for part in output.iter_mut() {
            let length = part.len().min(count.saturating_sub(copied));
            part[..length].copy_from_slice(&buffer[copied..copied + length]);
            copied += length;
            if copied == count {
                break;
            }
        }
        Ok(count)
    }

    fn read_vector(
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
        offset: Option<u64>,
        context: hl_descriptor::OperationContext<'_>,
    ) -> LinuxResult {
        let mut transfer = VectorTransfer::vacant(plan);
        let result = {
            let mut output = transfer.output();
            let result = match offset {
                Some(position) => lease.read_vector_at(position, &mut output),
                None => lease.read_vector_context(&mut output, context),
            };
            match result {
                Err(ObjectError::NotSupported) => Self::scatter_read(lease, &mut output, offset, context),
                result => result,
            }
        };
        let count = match result {
            Ok(count) => count,
            Err(ObjectError::Retired) => return Self::failed_read_probe(lease),
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let progress = transfer.publish(marshaller, count);
        if progress.copied == 0 && progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(progress.copied as u64)
        }
    }

    fn write_vector(
        &self,
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
        offset: Option<u64>,
        context: hl_descriptor::OperationContext<'_>,
        atomic: bool,
    ) -> LinuxResult {
        let Ok(maximum) = usize::try_from(plan.total_length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match lease.probe_write(maximum) {
            Ok(Some(count)) => return LinuxResult::Value(count as u64),
            Ok(None) => {}
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        }
        let transfer = match if atomic {
            VectorTransfer::capture_all(marshaller, plan)
        } else {
            VectorTransfer::capture(marshaller, plan)
        } {
            Ok(transfer) => transfer,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let input = transfer.input();
        let result = match offset {
            Some(position) => lease.write_vector_at(position, &input),
            None => lease.write_vector_context(&input, context),
        };
        match result {
            Ok(count) => LinuxResult::Value(count as u64),
            Err(ObjectError::BrokenPipe) => {
                if let Some(signal) = &self.pipe_signal {
                    let _ = signal.queue_sigpipe();
                }
                LinuxResult::Error(Errno::EPIPE)
            }
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
}
