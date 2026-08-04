use hl_descriptor::OperationLease;
use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult};

use crate::filesystem::FilesystemErrno;

pub(crate) struct AtomicReadCopyout;

impl AtomicReadCopyout {
    pub(crate) fn execute_transactional<M: GuestMemory>(
        lease: &OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        address: u64,
        maximum: usize,
        offset: Option<u64>,
        nonblocking: bool,
        context: hl_descriptor::OperationContext<'_>,
    ) -> Option<LinuxResult> {
        let prepared = match lease.prepare_splice_read(offset, maximum, nonblocking, context.cancellation) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return None,
            Err(error) => return Some(LinuxResult::Error(FilesystemErrno::object(error))),
        };
        let count = prepared.bytes().len();
        let progress = marshaller.copy_to(address, prepared.bytes());
        if progress.copied == 0 && progress.fault.is_some() {
            return Some(LinuxResult::Error(Errno::EFAULT));
        }
        let copied = progress.copied.min(count);
        Some(match prepared.commit(copied) {
            Ok(()) => LinuxResult::Value(copied as u64),
            Err(error) => LinuxResult::Error(FilesystemErrno::object(error)),
        })
    }

    pub(crate) fn execute<M: GuestMemory>(
        lease: &OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        address: u64,
        maximum: usize,
        context: Option<hl_descriptor::OperationContext<'_>>,
    ) -> Option<LinuxResult> {
        let prepared = match context.map_or_else(
            || lease.prepare_atomic_read(maximum),
            |context| lease.prepare_atomic_context(maximum, context),
        ) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return None,
            Err(error) => return Some(LinuxResult::Error(FilesystemErrno::object(error))),
        };
        let count = prepared.bytes().len();
        let progress = marshaller.copy_to(address, prepared.bytes());
        if progress.copied == 0 && progress.fault.is_some() {
            return Some(LinuxResult::Error(Errno::EFAULT));
        }
        let copied = progress.copied.min(count);
        Some(match prepared.commit_prefix(copied) {
            Ok(true) => LinuxResult::Value(copied as u64),
            Ok(false) => LinuxResult::Error(Errno::EFAULT),
            Err(error) => LinuxResult::Error(FilesystemErrno::object(error)),
        })
    }
}

#[cfg(test)]
mod test {
    use std::fmt;
    use std::sync::{Arc, Mutex};

    use hl_descriptor::{
        DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, PreparedSpliceRead, StatusFlags,
    };
    use hl_linux::{GuestAccess, GuestArchitecture, GuestFault};

    use super::*;

    struct Memory {
        writable: usize,
        bytes: Mutex<Vec<u8>>,
    }

    impl GuestMemory for Memory {
        fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
            let available = self.writable.saturating_sub(address as usize).min(length);
            if available == 0 {
                Err(GuestFault { address, access })
            } else {
                Ok(available)
            }
        }

        fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
            let start = address as usize;
            output.copy_from_slice(&self.bytes.lock().unwrap()[start..start + output.len()]);
            Ok(output.len())
        }

        fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
            let available = self.writable.saturating_sub(address as usize).min(input.len());
            if available == 0 {
                return Err(GuestFault {
                    address,
                    access: GuestAccess::Write,
                });
            }
            let start = address as usize;
            self.bytes.lock().unwrap()[start..start + available].copy_from_slice(&input[..available]);
            Ok(available)
        }
    }

    struct Source(Arc<Mutex<Vec<u8>>>);

    impl fmt::Debug for Source {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("Source")
        }
    }

    struct Prepared {
        source: Arc<Mutex<Vec<u8>>>,
        bytes: Vec<u8>,
    }

    impl PreparedSpliceRead for Prepared {
        fn bytes(&self) -> &[u8] {
            &self.bytes
        }

        fn commit(self: Box<Self>, count: usize) -> Result<(), ObjectError> {
            self.source.lock().unwrap().drain(..count);
            Ok(())
        }
    }

    impl OpenFileDescription for Source {
        fn prepare_splice_read(
            &self,
            offset: Option<u64>,
            maximum: usize,
            _nonblocking: bool,
            _cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
        ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
            if offset.is_some() {
                return Err(ObjectError::InvalidArgument);
            }
            let source = self.0.lock().unwrap();
            let bytes = source[..maximum.min(source.len())].to_vec();
            drop(source);
            Ok(Some(Box::new(Prepared {
                source: Arc::clone(&self.0),
                bytes,
            })))
        }
    }

    fn lease(source: Arc<Mutex<Vec<u8>>>) -> hl_descriptor::OperationLease {
        let table = DescriptorTable::new(1).unwrap();
        let descriptor = table
            .commit(
                table.reserve(0).unwrap(),
                Arc::new(Source(source)),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        table.pin(descriptor).unwrap()
    }

    #[test]
    fn empty_transaction_ignores_bad_destination() {
        let source = Arc::new(Mutex::new(Vec::new()));
        let memory = Memory {
            writable: 0,
            bytes: Mutex::new(vec![0; 1]),
        };
        let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
        assert_eq!(
            AtomicReadCopyout::execute_transactional(
                &lease(source),
                &marshaller,
                0,
                1,
                None,
                false,
                hl_descriptor::OperationContext {
                    actor: None,
                    cancellation: None,
                },
            ),
            Some(LinuxResult::Value(0)),
        );
    }

    #[test]
    fn failed_copyout_does_not_consume_transaction() {
        let source = Arc::new(Mutex::new(b"x".to_vec()));
        let memory = Memory {
            writable: 0,
            bytes: Mutex::new(vec![0; 1]),
        };
        let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::X86_64);
        assert_eq!(
            AtomicReadCopyout::execute_transactional(
                &lease(Arc::clone(&source)),
                &marshaller,
                0,
                1,
                None,
                false,
                hl_descriptor::OperationContext {
                    actor: None,
                    cancellation: None,
                },
            ),
            Some(LinuxResult::Error(Errno::EFAULT)),
        );
        assert_eq!(&*source.lock().unwrap(), b"x");
    }

    #[test]
    fn partial_copyout_consumes_and_reports_accessible_prefix() {
        let source = Arc::new(Mutex::new(b"abcdefgh".to_vec()));
        let memory = Memory {
            writable: 4,
            bytes: Mutex::new(vec![0; 8]),
        };
        let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
        assert_eq!(
            AtomicReadCopyout::execute_transactional(
                &lease(Arc::clone(&source)),
                &marshaller,
                0,
                8,
                None,
                false,
                hl_descriptor::OperationContext {
                    actor: None,
                    cancellation: None,
                },
            ),
            Some(LinuxResult::Value(4)),
        );
        assert_eq!(&*memory.bytes.lock().unwrap(), b"abcd\0\0\0\0");
        assert_eq!(&*source.lock().unwrap(), b"efgh");
    }
}
