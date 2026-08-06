use std::sync::Arc;

use hl_aio::{AioError, Catalog, ContextId, Event};
use hl_descriptor::{
    CancellationNotification, DescriptorError, DescriptorTable, ObjectError, ObjectKind, OperationCancellation,
};
use hl_linux::{
    AioAbi, AioControlBlock, AioEvent, AioMarshalError, AioOpcode, AioSyscalls, Errno, GuestArchitecture, GuestMemory,
    IOCB_FLAG_RESFD, LinuxResult, SyscallOperation,
};

const TRANSFER_MAXIMUM: u64 = 16 * 1024 * 1024;

pub struct RuntimeAioSyscalls<M: GuestMemory> {
    catalog: Arc<Catalog>,
    descriptors: Arc<DescriptorTable>,
    memory: M,
    architecture: GuestArchitecture,
    cancellation: Arc<dyn OperationCancellation>,
}

impl<M: GuestMemory> RuntimeAioSyscalls<M> {
    pub fn new(
        catalog: Arc<Catalog>,
        descriptors: Arc<DescriptorTable>,
        memory: M,
        architecture: GuestArchitecture,
        cancellation: Arc<dyn OperationCancellation>,
    ) -> Self {
        Self {
            catalog,
            descriptors,
            memory,
            architecture,
            cancellation,
        }
    }

    fn setup(&self, count: u64, address: u64) -> LinuxResult {
        let abi = AioAbi::new(&self.memory);
        let current = match abi.context(address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::marshal(error)),
        };
        if current != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Ok(count) = usize::try_from(count) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let id = match self.catalog.create(count) {
            Ok(id) => id,
            Err(error) => return LinuxResult::Error(Self::domain(error)),
        };
        if abi.write_context(address, id.raw()).is_err() {
            let _ = self.catalog.destroy(id);
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(0)
    }

    fn submit(&self, raw_id: u64, raw_count: u64, pointers: u64) -> LinuxResult {
        let count = raw_count as i64;
        if count < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if count == 0 {
            return LinuxResult::Value(0);
        }
        let abi = AioAbi::new(&self.memory);
        let pointers = match abi.pointers(pointers, count as u64) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::marshal(error)),
        };
        let id = ContextId::from_raw(raw_id);
        let mut submitted = 0_u64;
        let mut first_error = Errno::EFAULT;
        for address in pointers {
            match self.submit_one(&abi, id, address) {
                Ok(()) => submitted += 1,
                Err(error) => {
                    first_error = error;
                    break;
                }
            }
        }
        if submitted == 0 {
            LinuxResult::Error(first_error)
        } else {
            LinuxResult::Value(submitted)
        }
    }

    fn submit_one(&self, abi: &AioAbi<'_, M>, id: ContextId, address: u64) -> Result<(), Errno> {
        if address == 0 {
            return Err(Errno::EFAULT);
        }
        let control = abi.control(address).map_err(Self::marshal)?;
        if control.flags & !IOCB_FLAG_RESFD != 0 {
            return Err(Errno::EINVAL);
        }
        let eventfd = if control.flags & IOCB_FLAG_RESFD != 0 {
            Some(self.eventfd(control.result_descriptor)?)
        } else {
            None
        };
        let admission = self.catalog.admit(id).map_err(Self::domain)?;
        admission.complete(self.execute(control)).map_err(Self::domain)?;
        if let Some(eventfd) = eventfd {
            eventfd.write(&1_u64.to_ne_bytes()).map_err(Self::object)?;
        }
        Ok(())
    }

    fn execute(&self, control: AioControlBlock) -> Event {
        let result = match control.opcode {
            AioOpcode::Pread => self.scalar(control, true),
            AioOpcode::Pwrite => self.scalar(control, false),
            AioOpcode::Preadv => self.vector(control, true),
            AioOpcode::Pwritev => self.vector(control, false),
            AioOpcode::Fsync => self.synchronize(control.descriptor, false),
            AioOpcode::Fdatasync => self.synchronize(control.descriptor, true),
        };
        Event {
            data: control.data,
            object: control.address,
            result,
            secondary: 0,
        }
    }

    fn scalar(&self, control: AioControlBlock, reading: bool) -> i64 {
        if control.count > TRANSFER_MAXIMUM || control.offset < 0 {
            return -i64::from(Errno::EINVAL.raw());
        }
        let length = control.count as usize;
        let lease = match self.descriptors.pin(control.descriptor) {
            Ok(lease) => lease,
            Err(error) => return -i64::from(Self::descriptor(error).raw()),
        };
        let mut bytes = vec![0; length];
        if reading {
            let count = match lease.read_at(control.offset as u64, &mut bytes) {
                Ok(count) => count.min(length),
                Err(error) => return -i64::from(Self::object(error).raw()),
            };
            return match self.memory.write(control.buffer, &bytes[..count]) {
                Ok(written) if written == count => count as i64,
                _ => -i64::from(Errno::EFAULT.raw()),
            };
        }
        if self.memory.read(control.buffer, &mut bytes).ok() != Some(length) {
            return -i64::from(Errno::EFAULT.raw());
        }
        match lease.write_at(control.offset as u64, &bytes) {
            Ok(count) => count.min(length) as i64,
            Err(error) => -i64::from(Self::object(error).raw()),
        }
    }

    fn vector(&self, control: AioControlBlock, reading: bool) -> i64 {
        if control.offset < 0 {
            return -i64::from(Errno::EINVAL.raw());
        }
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        let Ok(count) = usize::try_from(control.count) else { return -i64::from(Errno::EINVAL.raw()) };
        let plan = match marshaller.iovecs(control.buffer, count) {
            Ok(plan) if plan.total_length <= TRANSFER_MAXIMUM => plan,
            Ok(_) | Err(hl_linux::MarshalError::Invalid) => return -i64::from(Errno::EINVAL.raw()),
            Err(_) => return -i64::from(Errno::EFAULT.raw()),
        };
        let mut total = 0_i64;
        let mut offset = control.offset;
        for vector in plan.vectors {
            let segment = AioControlBlock {
                buffer: vector.base,
                count: vector.length,
                offset,
                ..control
            };
            let result = self.scalar(segment, reading);
            if result < 0 {
                return Self::vector_failure(result, total);
            }
            total += result;
            offset = match offset.checked_add(result) {
                Some(value) => value,
                None => return total,
            };
            if result as u64 != vector.length {
                break;
            }
        }
        total
    }

    fn vector_failure(result: i64, transferred: i64) -> i64 {
        if transferred == 0 { result } else { transferred }
    }

    fn synchronize(&self, descriptor: i32, data_only: bool) -> i64 {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return -i64::from(Self::descriptor(error).raw()),
        };
        match lease.synchronize(data_only) {
            Ok(()) => 0,
            Err(error) => -i64::from(Self::object(error).raw()),
        }
    }

    fn eventfd(&self, descriptor: i32) -> Result<hl_descriptor::OperationLease, Errno> {
        let lease = self.descriptors.pin(descriptor).map_err(Self::descriptor)?;
        if lease.object().kind() != ObjectKind::EventCounter {
            return Err(Errno::EINVAL);
        }
        Ok(lease)
    }

    fn getevents(&self, arguments: [u64; 6]) -> LinuxResult {
        let minimum = arguments[1] as i64;
        let maximum = arguments[2] as i64;
        if minimum < 0 || maximum < 0 || minimum > maximum {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let abi = AioAbi::new(&self.memory);
        let timeout = match abi.timeout(arguments[4]) {
            Ok(timeout) => timeout,
            Err(error) => return LinuxResult::Error(Self::marshal(error)),
        };
        let id = ContextId::from_raw(arguments[0]);
        let notification = Arc::new(Wake {
            catalog: Arc::clone(&self.catalog),
            id,
        });
        let _subscription = self.cancellation.subscribe(notification);
        let batch = match self.catalog.stage(id, minimum as usize, maximum as usize, timeout, || {
            self.cancellation.interrupted()
        }) {
            Ok(batch) => batch,
            Err(error) => return LinuxResult::Error(Self::domain(error)),
        };
        let events = batch
            .events()
            .iter()
            .map(|event| AioEvent {
                data: event.data,
                object: event.object,
                result: event.result,
                secondary: event.secondary,
            })
            .collect::<Vec<_>>();
        let staged = match abi.stage_events(arguments[3], events.len()) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(Self::marshal(error)),
        };
        if staged.publish(&events).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let count = events.len() as u64;
        batch.commit();
        LinuxResult::Value(count)
    }

    fn marshal(error: AioMarshalError) -> Errno {
        match error {
            AioMarshalError::Fault => Errno::EFAULT,
            AioMarshalError::Invalid => Errno::EINVAL,
        }
    }

    fn domain(error: AioError) -> Errno {
        match error {
            AioError::InvalidArgument | AioError::Closing => Errno::EINVAL,
            AioError::ResourceLimit => Errno::EAGAIN,
            AioError::Interrupted => Errno::EINTR,
        }
    }

    fn descriptor(error: DescriptorError) -> Errno {
        match error {
            DescriptorError::BadDescriptor => Errno::EBADF,
            DescriptorError::InvalidArgument | DescriptorError::AlreadyExists => Errno::EINVAL,
            DescriptorError::TooManyOpenFiles => Errno::EMFILE,
            DescriptorError::CheckpointFrozen => Errno::EBUSY,
            DescriptorError::StaleReservation | DescriptorError::Corrupt => Errno::EIO,
        }
    }

    fn object(error: ObjectError) -> Errno {
        match error {
            ObjectError::BadDescriptor | ObjectError::Retired => Errno::EBADF,
            ObjectError::NoSuchProcess => Errno::ESRCH,
            ObjectError::InvalidArgument => Errno::EINVAL,
            ObjectError::WouldBlock => Errno::EAGAIN,
            ObjectError::Interrupted | ObjectError::Canceled => Errno::EINTR,
            ObjectError::ResourceLimit => Errno::ENFILE,
            ObjectError::NoSpace => Errno::ENOSPC,
            ObjectError::NoExtent => Errno::ENXIO,
            ObjectError::PermissionDenied => Errno::EPERM,
            ObjectError::Busy => Errno::EBUSY,
            ObjectError::BrokenPipe => Errno::EPIPE,
            ObjectError::NotSupported => Errno::ENOSYS,
            ObjectError::Io => Errno::EIO,
        }
    }
}

impl<M: GuestMemory> AioSyscalls for RuntimeAioSyscalls<M> {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "io_setup" => self.setup(arguments[0], arguments[1]),
            "io_destroy" => match self.catalog.destroy(ContextId::from_raw(arguments[0])) {
                Ok(()) => LinuxResult::Value(0),
                Err(error) => LinuxResult::Error(Self::domain(error)),
            },
            "io_submit" => self.submit(arguments[0], arguments[1], arguments[2]),
            "io_cancel" => LinuxResult::Error(Errno::EINVAL),
            "io_getevents" => self.getevents(arguments),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}

struct Wake {
    catalog: Arc<Catalog>,
    id: ContextId,
}
impl CancellationNotification for Wake {
    fn notify(&self) {
        let _ = self.catalog.wake(self.id);
    }
}
