use std::fmt;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::composition::StandardStreams;

use hl_descriptor::{
    DescriptorError, DescriptorFlags, DescriptorTable, ExactDuplicate, ObjectError, OfdMetadata, OfdTimestamp,
    OpenFileDescription, OperationLease, StatusFlags,
};
use hl_linux::Errno;

#[cfg(test)]
const DESCRIPTOR_LIMIT: i32 = 1024;

#[derive(Clone, Copy)]
pub(super) struct Slot {
    pub(super) native: i32,
}

pub(super) struct Set {
    table: Arc<DescriptorTable>,
    standard: [u64; 3],
}

impl Set {
    pub(super) fn fork(&self, table: Arc<DescriptorTable>) -> Self {
        Self {
            table,
            standard: self.standard,
        }
    }

    #[cfg(test)]
    pub(super) fn descriptor_table(&self) -> Arc<DescriptorTable> {
        Arc::clone(&self.table)
    }

    #[cfg(test)]
    pub(super) fn new() -> Result<Self, DescriptorError> {
        Self::with_table(
            Arc::new(DescriptorTable::new(DESCRIPTOR_LIMIT)?),
            &StandardStreams::default(),
        )
    }

    #[cfg(test)]
    pub(super) fn with_input(input: Box<dyn Read + Send>) -> Result<Self, DescriptorError> {
        let streams = StandardStreams::new(input, std::io::sink(), std::io::sink());
        Self::with_table(Arc::new(DescriptorTable::new(DESCRIPTOR_LIMIT)?), &streams)
    }

    pub(super) fn with_table(table: Arc<DescriptorTable>, streams: &StandardStreams) -> Result<Self, DescriptorError> {
        let objects: [Arc<dyn OpenFileDescription>; 3] = [
            Arc::new(StandardIo::input(streams.input(), 1)),
            Arc::new(StandardIo::output(streams.output(), 2)),
            Arc::new(StandardIo::output(streams.error(), 3)),
        ];
        let mut standard = [0; 3];
        for (number, object) in objects.into_iter().enumerate() {
            let number = number as i32;
            let reservation = table.reserve_exact(number)?;
            let access = u32::from(number != 0);
            table.commit(
                reservation,
                object,
                StatusFlags::from_bits(access),
                DescriptorFlags::default(),
            )?;
            standard[number as usize] = table.snapshot(number)?.description_identity;
        }
        Ok(Self { table, standard })
    }

    pub(super) fn with_terminal(
        table: Arc<DescriptorTable>,
        slave: Arc<hl_runtime::TerminalDescription>,
        bindings: &Arc<hl_runtime::TerminalBindings>,
    ) -> Result<Self, DescriptorError> {
        let reservation = table.reserve_exact(0)?;
        table.commit(
            reservation,
            slave.clone(),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )?;
        let description_identity = table.pin(0)?.description_identity();
        let identity = table.snapshot(0)?.description_identity;
        slave.bind(description_identity, bindings);
        table.duplicate_exact(0, 1, ExactDuplicate::Dup2)?;
        table.duplicate_exact(0, 2, ExactDuplicate::Dup2)?;
        Ok(Self {
            table,
            standard: [identity; 3],
        })
    }

    pub(super) fn slot(&self, descriptor: i32) -> Option<Slot> {
        let snapshot = self.table.snapshot(descriptor).ok()?;
        let native = self
            .standard
            .iter()
            .position(|identity| *identity == snapshot.description_identity)?;
        Some(Slot { native: native as i32 })
    }

    pub(super) fn current(&self, descriptor: i32, generation: u64) -> bool {
        self.table
            .snapshot(descriptor)
            .ok()
            .is_some_and(|snapshot| u64::from(snapshot.descriptor_generation) == generation)
    }

    pub(super) fn pin(&self, descriptor: i32) -> Result<OperationLease, Errno> {
        self.table.pin(descriptor).map_err(Self::errno)
    }

    pub(super) fn close(&self, descriptor: i32) -> Result<(), Errno> {
        self.table.close(descriptor).map_err(Self::errno)
    }

    pub(super) fn duplicate(&self, source: i32, minimum: i32, flags: DescriptorFlags) -> Result<i32, Errno> {
        self.table.duplicate(source, minimum, flags).map_err(Self::errno)
    }

    pub(super) fn duplicate_exact(
        &self,
        source: i32,
        destination: i32,
        operation: ExactDuplicate,
    ) -> Result<i32, Errno> {
        self.table
            .duplicate_exact(source, destination, operation)
            .map_err(Self::errno)
    }

    pub(super) fn flags(&self, descriptor: i32) -> Result<DescriptorFlags, Errno> {
        self.table.flags(descriptor).map_err(Self::errno)
    }

    pub(super) fn update_flags(&self, descriptor: i32, flags: DescriptorFlags) -> Result<(), Errno> {
        self.table.set_flags(descriptor, flags).map_err(Self::errno)
    }

    pub(super) fn status(&self, descriptor: i32) -> Result<StatusFlags, Errno> {
        self.table
            .pin(descriptor)
            .map(|lease| lease.status())
            .map_err(Self::errno)
    }

    pub(super) fn update_status(&self, descriptor: i32, requested: u32) -> Result<(), Errno> {
        let lease = self.table.pin(descriptor).map_err(Self::errno)?;
        let status = lease.status().update_from_fcntl(requested);
        lease.set_status(status).map_err(Self::object_errno)
    }

    pub(super) fn snapshot(&self, descriptor: i32) -> Result<hl_descriptor::DescriptorSnapshot, Errno> {
        self.table.snapshot(descriptor).map_err(Self::errno)
    }

    pub(super) fn readiness(&self, descriptor: i32, interests: i16) -> Option<i16> {
        let lease = self.table.pin(descriptor).ok()?;
        let linux = interests as u16 as u32;
        let internal = (linux & !0x6) | ((linux & 0x2) << 1) | ((linux & 0x4) >> 1);
        let ready = lease.readiness(hl_descriptor::Readiness::from_bits(internal)).bits();
        Some(((ready & !0x6) | ((ready & 0x2) << 1) | ((ready & 0x4) >> 1)) as i16)
    }

    pub(super) fn errno(error: DescriptorError) -> Errno {
        match error {
            DescriptorError::BadDescriptor => Errno::EBADF,
            DescriptorError::InvalidArgument | DescriptorError::AlreadyExists => Errno::EINVAL,
            DescriptorError::TooManyOpenFiles => Errno::EMFILE,
            DescriptorError::CheckpointFrozen => Errno::EBUSY,
            DescriptorError::StaleReservation | DescriptorError::Corrupt => Errno::EIO,
        }
    }

    pub(super) fn object_errno(error: ObjectError) -> Errno {
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

enum StandardKind {
    Input(Arc<Mutex<Box<dyn Read + Send>>>),
    Output(Arc<Mutex<Box<dyn Write + Send>>>),
}

struct StandardIo {
    kind: StandardKind,
    inode: u64,
}

impl StandardIo {
    fn input(input: Arc<Mutex<Box<dyn Read + Send>>>, inode: u64) -> Self {
        Self {
            kind: StandardKind::Input(input),
            inode,
        }
    }

    fn output(output: Arc<Mutex<Box<dyn Write + Send>>>, inode: u64) -> Self {
        Self {
            kind: StandardKind::Output(output),
            inode,
        }
    }

    fn io_errno(error: &std::io::Error) -> ObjectError {
        match error.kind() {
            std::io::ErrorKind::WouldBlock => ObjectError::WouldBlock,
            std::io::ErrorKind::Interrupted => ObjectError::Interrupted,
            std::io::ErrorKind::BrokenPipe => ObjectError::BrokenPipe,
            std::io::ErrorKind::PermissionDenied => ObjectError::PermissionDenied,
            _ => ObjectError::Io,
        }
    }
}

impl fmt::Debug for StandardIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StandardIo")
    }
}

impl OpenFileDescription for StandardIo {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0,
            inode: self.inode,
            kind: 2,
            permissions: match self.kind {
                StandardKind::Input(_) => 0o400,
                _ => 0o200,
            },
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

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let StandardKind::Input(input) = &self.kind else {
            return Err(ObjectError::BadDescriptor);
        };
        input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read(output)
            .map_err(|error| Self::io_errno(&error))
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        let StandardKind::Output(output) = &self.kind else {
            return Err(ObjectError::BadDescriptor);
        };
        output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write(input)
            .map_err(|error| Self::io_errno(&error))
    }

    /// Writes every segment in one operation, as Linux does for a standard stream.
    ///
    /// The inherited default writes only the first non-empty segment, which turns
    /// each `writev` into a short write that the guest retries, splitting one
    /// process write across several output records.
    fn write_vector_context(
        &self,
        input: &[std::io::IoSlice<'_>],
        _: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let length = input
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        if length == 0 {
            return Ok(0);
        }
        let mut buffer = Vec::with_capacity(length);
        for part in input {
            buffer.extend_from_slice(part);
        }
        self.write(&buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct IgnoreSignals;

    impl hl_runtime::TerminalSignalSink for IgnoreSignals {
        fn publish(
            &self,
            _actor: Option<hl_descriptor::OperationActor>,
            _terminal: hl_runtime::TerminalId,
            _foreground: Option<hl_runtime::TerminalForegroundGroup>,
            _signal: hl_runtime::TerminalSignal,
        ) {
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn standard_descriptors_use_injected_process_streams() {
        let output = Capture::default();
        let error = Capture::default();
        let streams = StandardStreams::new(std::io::empty(), output.clone(), error.clone());
        let descriptors = Set::with_table(Arc::new(DescriptorTable::new(DESCRIPTOR_LIMIT).unwrap()), &streams).unwrap();

        assert_eq!(descriptors.pin(1).unwrap().write(b"out\0").unwrap(), 4);
        assert_eq!(descriptors.pin(2).unwrap().write(b"err\xff").unwrap(), 4);
        assert_eq!(&*output.0.lock().unwrap(), b"out\0");
        assert_eq!(&*error.0.lock().unwrap(), b"err\xff");
    }

    #[test]
    fn terminal_standard_descriptors_share_read_write_slave() {
        let catalog = Arc::new(hl_runtime::TerminalCatalog::default());
        let pair = catalog.allocate().unwrap();
        let signals: Arc<dyn hl_runtime::TerminalSignalSink> = Arc::new(IgnoreSignals);
        let master = Arc::new(hl_runtime::TerminalDescription::new(
            Arc::clone(&pair),
            hl_runtime::TerminalEndpoint::Master,
            Arc::downgrade(&catalog),
            Arc::clone(&signals),
        ));
        let slave = Arc::new(hl_runtime::TerminalDescription::new(
            pair,
            hl_runtime::TerminalEndpoint::Slave,
            Arc::downgrade(&catalog),
            signals,
        ));
        let table = Arc::new(DescriptorTable::new(DESCRIPTOR_LIMIT).unwrap());
        let bindings = Arc::new(hl_runtime::TerminalBindings::default());
        let descriptors = Set::with_terminal(table, slave, &bindings).unwrap();

        let identity = descriptors.pin(0).unwrap().description_identity();
        for descriptor in 0..=2 {
            let lease = descriptors.pin(descriptor).unwrap();
            assert_eq!(lease.description_identity(), identity);
            assert_eq!(lease.status().bits() & StatusFlags::ACCESS_MODE_MASK, 2);
            let terminal = bindings.get(lease.description_identity()).unwrap();
            assert_eq!(terminal.endpoint, hl_runtime::TerminalEndpoint::Slave);
        }

        master.write(b"input\n").unwrap();
        let mut input = [0_u8; 6];
        assert_eq!(descriptors.pin(0).unwrap().read(&mut input).unwrap(), input.len());
        assert_eq!(&input, b"input\n");
        let mut echoed = [0_u8; 6];
        assert_eq!(master.read(&mut echoed).unwrap(), echoed.len());
        assert_eq!(&echoed, b"input\n");

        assert_eq!(descriptors.pin(1).unwrap().write(b"output\n").unwrap(), 7);
        let mut output = [0_u8; 8];
        assert_eq!(master.read(&mut output).unwrap(), output.len());
        assert_eq!(&output, b"output\r\n");
    }
}
