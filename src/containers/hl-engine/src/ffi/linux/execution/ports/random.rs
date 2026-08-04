use hl_linux::{Errno, LinuxResult};

use super::DescriptorPort;

const CHUNK_LENGTH: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ffi::linux::execution) enum EntropyError {
    Interrupted,
    WouldBlock,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ffi::linux::execution) struct EntropyFlags(u32);

impl EntropyFlags {
    const NONBLOCK: u32 = 1;
    const RANDOM: u32 = 2;
    const INSECURE: u32 = 4;

    pub(in crate::ffi::linux::execution) fn parse(value: u64) -> Result<Self, Errno> {
        let valid = u64::from(Self::NONBLOCK | Self::RANDOM | Self::INSECURE);
        let incompatible = u64::from(Self::RANDOM | Self::INSECURE);
        if value & !valid != 0 || value & incompatible == incompatible {
            return Err(Errno::EINVAL);
        }
        Ok(Self(value as u32))
    }

    pub(in crate::ffi::linux::execution) const fn bits(self) -> u32 {
        self.0
    }
}

pub(in crate::ffi::linux::execution) trait EntropySource: Send + Sync {
    fn draw(&self, output: &mut [u8], flags: EntropyFlags) -> Result<usize, EntropyError>;
}

impl DescriptorPort {
    pub(super) fn random(&self, address: u64, length: u64, flags: u64) -> LinuxResult {
        let flags = match EntropyFlags::parse(flags) {
            Ok(flags) => flags,
            Err(error) => return LinuxResult::Error(error),
        };
        if length == 0 {
            return LinuxResult::Value(0);
        }
        let mut bytes = [0_u8; CHUNK_LENGTH];
        let mut done = 0_u64;
        while done < length {
            let requested = usize::try_from((length - done).min(CHUNK_LENGTH as u64)).expect("bounded chunk");
            let count = match self.entropy.draw(&mut bytes[..requested], flags) {
                Ok(0) => return Self::partial_or(done, Errno::EAGAIN),
                Ok(count) if count <= requested => count,
                Ok(_) => return Self::partial_or(done, Errno::EIO),
                Err(EntropyError::Interrupted) => return Self::partial_or(done, Errno::EINTR),
                Err(EntropyError::WouldBlock) => return Self::partial_or(done, Errno::EAGAIN),
                Err(EntropyError::Failed) => return Self::partial_or(done, Errno::EIO),
            };
            let destination = match address.checked_add(done) {
                Some(destination) => destination,
                None => return Self::partial_or(done, Errno::EFAULT),
            };
            let copied = match self.memory.writable_prefix(destination, count as u64) {
                Ok(available) => available.min(count),
                Err(_) => 0,
            };
            if copied != 0 && self.memory.write(destination, &bytes[..copied]).is_err() {
                return Self::partial_or(done, Errno::EFAULT);
            }
            done += copied as u64;
            if copied != count {
                return Self::partial_or(done, Errno::EFAULT);
            }
            if count != requested {
                return LinuxResult::Value(done);
            }
        }
        LinuxResult::Value(done)
    }

    const fn partial_or(done: u64, error: Errno) -> LinuxResult {
        if done == 0 {
            LinuxResult::Error(error)
        } else {
            LinuxResult::Value(done)
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use hl_isa::GuestAddress;
    use hl_memory::{Backing, MapRequest, MappingHost, Placement, Protection};

    use super::*;
    use crate::ffi::linux::execution::descriptor::Set;
    use crate::ffi::linux::{MappingHostAdapter, VirtualMemory};

    struct Script {
        results: Mutex<VecDeque<Result<usize, EntropyError>>>,
        requests: Mutex<Vec<usize>>,
    }

    impl Script {
        fn new(results: impl IntoIterator<Item = Result<usize, EntropyError>>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl EntropySource for Script {
        fn draw(&self, output: &mut [u8], _: EntropyFlags) -> Result<usize, EntropyError> {
            self.requests.lock().unwrap().push(output.len());
            output.fill(0xa5);
            self.results.lock().unwrap().pop_front().unwrap_or(Ok(output.len()))
        }
    }

    fn setup(pages: usize, mapped: usize, entropy: Arc<dyn EntropySource>) -> (DescriptorPort, Arc<VirtualMemory>) {
        let memory = Arc::new(VirtualMemory::reserve(pages * CHUNK_LENGTH).unwrap());
        let host = MappingHostAdapter::new(Arc::clone(&memory));
        let request = MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length: (mapped * CHUNK_LENGTH) as u64,
            alignment: CHUNK_LENGTH as u64,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: 1,
                shared: false,
            },
            backing_offset: 0,
        };
        let token = host.stage_map(GuestAddress::new(0), request).unwrap();
        host.commit(&[token]).unwrap();
        let descriptors = Arc::new(Set::new().unwrap());
        (DescriptorPort::new(Arc::clone(&memory), descriptors, entropy), memory)
    }

    #[test]
    fn flags_match_linux() {
        assert!(EntropyFlags::parse(4).is_ok());
        assert_eq!(EntropyFlags::parse(6), Err(Errno::EINVAL));
        assert_eq!(EntropyFlags::parse(8), Err(Errno::EINVAL));
    }

    #[test]
    fn request_chunks() {
        let source = Arc::new(Script::new([]));
        let (port, _) = setup(3, 3, source.clone());
        assert_eq!(port.random(0, 8193, 0), LinuxResult::Value(8193));
        assert_eq!(*source.requests.lock().unwrap(), [4096, 4096, 1]);
    }

    #[test]
    fn copy_progress() {
        let source = Arc::new(Script::new([]));
        let (port, memory) = setup(2, 1, source);
        assert_eq!(port.random(4088, 16, 0), LinuxResult::Value(8));
        let mut observed = [0_u8; 8];
        memory.read(4088, &mut observed).unwrap();
        assert_eq!(observed, [0xa5; 8]);
    }

    #[test]
    fn interruption_progress() {
        let source = Arc::new(Script::new([Ok(4096), Err(EntropyError::Interrupted)]));
        let (port, _) = setup(2, 2, source);
        assert_eq!(port.random(0, 4097, 0), LinuxResult::Value(4096));
        let source = Arc::new(Script::new([Err(EntropyError::Interrupted)]));
        let (port, _) = setup(1, 1, source);
        assert_eq!(port.random(0, 1, 0), LinuxResult::Error(Errno::EINTR));
    }

    #[test]
    fn nonblocking_progress() {
        let source = Arc::new(Script::new([Ok(4096), Err(EntropyError::WouldBlock)]));
        let (port, _) = setup(2, 2, source);
        assert_eq!(port.random(0, 4097, 1), LinuxResult::Value(4096));
        let source = Arc::new(Script::new([Err(EntropyError::WouldBlock)]));
        let (port, _) = setup(1, 1, source);
        assert_eq!(port.random(0, 1, 1), LinuxResult::Error(Errno::EAGAIN));
    }
}
