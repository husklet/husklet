use hl_descriptor::{ObjectError, OperationLease};
use hl_ipc::{PipeTransfer, PipeTransferMode};
use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult};

use crate::{filesystem::errno::FileErrno, filesystem::syscalls::RuntimeFilesystemSyscalls};

const SPLICE_FLAGS: u64 = 0xf;
const SPLICE_NONBLOCK: u64 = 0x2;

#[derive(Clone, Copy)]
struct SpliceOffset {
    pointer: u64,
    value: Option<u64>,
}

struct FileSplicePlan<'a> {
    source: &'a OperationLease,
    target: &'a OperationLease,
    input: SpliceOffset,
    output: SpliceOffset,
    maximum: u64,
    nonblocking: bool,
}

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn sendfile(&self, arguments: [u64; 6]) -> LinuxResult {
        if (arguments[3] as i64) < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.descriptors.pin(arguments[0] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let source = match self.descriptors.pin(arguments[1] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let input = match self.splice_offset(arguments[2]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        self.splice_file_side(FileSplicePlan {
            source: &source,
            target: &target,
            input: SpliceOffset {
                pointer: arguments[2],
                value: input,
            },
            output: SpliceOffset {
                pointer: 0,
                value: None,
            },
            maximum: arguments[3].min(0x7fff_f000),
            nonblocking: false,
        })
    }

    pub(super) fn copy_file_range(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[5] != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let source = match self.descriptors.pin(arguments[0] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let target = match self.descriptors.pin(arguments[2] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let input = match self.splice_offset(arguments[1]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let output = match self.splice_offset(arguments[3]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(maximum) = usize::try_from(arguments[4]) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let result = source.copy_file_range(&target, input, output, maximum, false, cancellation);
        match result {
            Ok(Some((count, input_start, output_start))) => {
                let input_copyout = self.splice_copyout_offset(
                    SpliceOffset {
                        pointer: arguments[1],
                        value: input.map(|_| input_start),
                    },
                    count,
                );
                let output_copyout = self.splice_copyout_offset(
                    SpliceOffset {
                        pointer: arguments[3],
                        value: output.map(|_| output_start),
                    },
                    count,
                );
                if count == 0 && (input_copyout.is_err() || output_copyout.is_err()) {
                    LinuxResult::Error(Errno::EFAULT)
                } else {
                    LinuxResult::Value(count as u64)
                }
            }
            Ok(None) => LinuxResult::Error(Errno::ENOSYS),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
    pub(super) fn vmsplice(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[3] & !SPLICE_FLAGS != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let descriptor = arguments[0] as i32;
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if lease.pipe_transfer_endpoint().is_none() {
            return LinuxResult::Error(Errno::EBADF);
        }
        let reading = lease.status().bits() & 3 == 0;
        drop(lease);
        self.vector_io(descriptor, arguments[1], arguments[2], reading)
    }

    pub(super) fn tee(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[3] & !SPLICE_FLAGS != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.pipe_transfer(
            arguments[0] as i32,
            arguments[1] as i32,
            arguments[2],
            PipeTransferMode::Duplicate,
            arguments[3] & SPLICE_NONBLOCK != 0,
        )
    }

    pub(super) fn splice(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[5] & !SPLICE_FLAGS != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let source = match self.descriptors.pin(arguments[0] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let target = match self.descriptors.pin(arguments[2] as i32) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let source_pipe = source.pipe_transfer_endpoint().is_some();
        let target_pipe = target.pipe_transfer_endpoint().is_some();
        if !source_pipe && !target_pipe {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if (source_pipe && arguments[1] != 0) || (target_pipe && arguments[3] != 0) {
            return LinuxResult::Error(Errno::ESPIPE);
        }
        let nonblocking = arguments[5] & SPLICE_NONBLOCK != 0;
        if source_pipe && target_pipe {
            return self.transfer_leases(&source, &target, arguments[4], PipeTransferMode::Move, nonblocking);
        }
        let input_offset = match self.splice_offset(arguments[1]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let output_offset = match self.splice_offset(arguments[3]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        self.splice_file_side(FileSplicePlan {
            source: &source,
            target: &target,
            input: SpliceOffset {
                pointer: arguments[1],
                value: input_offset,
            },
            output: SpliceOffset {
                pointer: arguments[3],
                value: output_offset,
            },
            maximum: arguments[4],
            nonblocking,
        })
    }

    fn pipe_transfer(
        &self,
        source: i32,
        target: i32,
        maximum: u64,
        mode: PipeTransferMode,
        nonblocking: bool,
    ) -> LinuxResult {
        let source = match self.descriptors.pin(source) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let target = match self.descriptors.pin(target) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        self.transfer_leases(&source, &target, maximum, mode, nonblocking)
    }

    fn transfer_leases(
        &self,
        source: &OperationLease,
        target: &OperationLease,
        maximum: u64,
        mode: PipeTransferMode,
        nonblocking: bool,
    ) -> LinuxResult {
        let Some(source) = source.pipe_transfer_endpoint() else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Some(target) = target.pipe_transfer_endpoint() else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Ok(maximum) = usize::try_from(maximum) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        match PipeTransfer::execute(source, target, maximum, mode, nonblocking, cancellation) {
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

    fn splice_offset(&self, address: u64) -> Result<Option<u64>, Errno> {
        if address == 0 {
            return Ok(None);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let mut bytes = [0_u8; 8];
        let progress = marshaller.copy_from(address, &mut bytes);
        if progress.fault.is_some() {
            return Err(Errno::EFAULT);
        }
        match marshaller.probe(address, 8, GuestAccess::Write) {
            Ok(8) => {}
            _ => return Err(Errno::EFAULT),
        }
        let offset = i64::from_le_bytes(bytes);
        if offset < 0 {
            return Err(Errno::EINVAL);
        }
        Ok(Some(offset as u64))
    }

    fn splice_file_side(&self, plan: FileSplicePlan<'_>) -> LinuxResult {
        let Ok(maximum) = usize::try_from(plan.maximum.min(65_536)) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let prepared = match plan
            .source
            .prepare_splice_read(plan.input.value, maximum, plan.nonblocking, cancellation)
        {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return LinuxResult::Error(Errno::ENOSYS),
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let result = match plan.output.value {
            Some(offset) => plan.target.write_at(offset, prepared.bytes()),
            None => match cancellation {
                Some(cancellation) => plan.target.write_with_cancellation(prepared.bytes(), cancellation),
                None => plan.target.write(prepared.bytes()),
            },
        };
        let count = match result {
            Ok(count) => count.min(prepared.bytes().len()),
            Err(ObjectError::BrokenPipe) => {
                if let Some(signal) = &self.pipe_signal {
                    let _ = signal.queue_sigpipe();
                }
                return LinuxResult::Error(Errno::EPIPE);
            }
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        if prepared.commit(count).is_err() {
            return LinuxResult::Value(count as u64);
        }
        // Linux reports a failed offset writeback as EFAULT; this path keeps the transfer count.
        for (name, offset) in [("off_in", plan.input), ("off_out", plan.output)] {
            if let Err(errno) = self.splice_copyout_offset(offset, count) {
                hl_log::hl_debug!(
                    hl_log::tag::FS,
                    "splice {name} writeback faulted count={count} errno={errno:?}"
                );
            }
        }
        LinuxResult::Value(count as u64)
    }

    fn splice_copyout_offset(&self, offset: SpliceOffset, count: usize) -> Result<(), Errno> {
        let Some(value) = offset.value.and_then(|value| value.checked_add(count as u64)) else {
            return Ok(());
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let progress = marshaller.copy_to(offset.pointer, &(value as i64).to_le_bytes());
        if progress.fault.is_some() {
            Err(Errno::EFAULT)
        } else {
            Ok(())
        }
    }
}
