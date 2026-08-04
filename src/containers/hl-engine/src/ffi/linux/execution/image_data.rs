use hl_runtime::RuntimeExecError;

use super::ports::random::{EntropyError, EntropyFlags, EntropySource};

pub(super) struct Entropy;

impl Entropy {
    pub(super) fn read(&self) -> Result<[u8; 16], RuntimeExecError> {
        Self::read_from(self)
    }

    pub(super) fn read_from(source: &dyn EntropySource) -> Result<[u8; 16], RuntimeExecError> {
        let mut bytes = [0_u8; 16];
        let mut offset = 0;
        while offset < bytes.len() {
            match source.draw(
                &mut bytes[offset..],
                EntropyFlags::parse(0).expect("zero flags are valid"),
            ) {
                Ok(0) => return Err(RuntimeExecError::Failed),
                Ok(count) if count <= bytes.len() - offset => offset += count,
                Ok(_) => return Err(RuntimeExecError::Failed),
                Err(EntropyError::Interrupted) => {}
                Err(_) => return Err(RuntimeExecError::Failed),
            }
        }
        Ok(bytes)
    }

    fn read_once(output: &mut [u8], flags: u32) -> Result<usize, std::io::Error> {
        // SAFETY: output is writable for its reported length, the kernel does
        // not retain it, and the scalar flags are passed through unchanged.
        let count = unsafe { libc::syscall(libc::SYS_getrandom, output.as_mut_ptr(), output.len(), flags) };
        usize::try_from(count).map_err(|_| std::io::Error::last_os_error())
    }
}

impl EntropySource for Entropy {
    fn draw(&self, output: &mut [u8], flags: EntropyFlags) -> Result<usize, EntropyError> {
        Self::read_once(output, flags.bits()).map_err(|error| match error.kind() {
            std::io::ErrorKind::Interrupted => EntropyError::Interrupted,
            std::io::ErrorKind::WouldBlock => EntropyError::WouldBlock,
            _ => EntropyError::Failed,
        })
    }
}

pub(super) struct AuxiliaryImage;

impl AuxiliaryImage {
    pub(super) fn encode(stack: &hl_loader::InitialStack) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(stack.auxiliary().len() * 16);
        for entry in stack.auxiliary() {
            bytes.extend_from_slice(&(entry.kind() as u64).to_ne_bytes());
            bytes.extend_from_slice(&entry.value().to_ne_bytes());
        }
        bytes
    }
}
