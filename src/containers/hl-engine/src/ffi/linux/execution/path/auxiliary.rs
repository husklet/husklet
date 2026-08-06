use std::fmt;
use std::sync::{Arc, Mutex};

use hl_descriptor::{ObjectError, OpenFileDescription, SeekPosition};
use hl_runtime::{OpenIntent, PreparedPathOpen, RuntimePathError};

pub(super) struct AuxiliaryFile {
    bytes: Vec<u8>,
    cursor: Mutex<usize>,
}

impl AuxiliaryFile {
    pub(super) fn prepare(bytes: Vec<u8>, intent: OpenIntent) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        if intent.bits() & (OpenIntent::WRITE | OpenIntent::TRUNCATE) != 0 {
            return Err(RuntimePathError::Access);
        }
        Ok(Box::new(AuxiliaryOpen(Arc::new(Self {
            bytes,
            cursor: Mutex::new(0),
        }))))
    }
}

impl fmt::Debug for AuxiliaryFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuxiliaryFile")
    }
}

impl OpenFileDescription for AuxiliaryFile {
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let mut cursor = self.cursor.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = output.len().min(self.bytes.len().saturating_sub(*cursor));
        output[..count].copy_from_slice(&self.bytes[*cursor..*cursor + count]);
        *cursor += count;
        Ok(count)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        let offset = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let count = output.len().min(self.bytes.len().saturating_sub(offset));
        output[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        Ok(count)
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        let mut cursor = self.cursor.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let next = match position {
            SeekPosition::Start(value) => i128::from(value),
            SeekPosition::Current(value) => *cursor as i128 + i128::from(value),
            SeekPosition::End(value) => self.bytes.len() as i128 + i128::from(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => return Err(ObjectError::InvalidArgument),
        };
        *cursor = usize::try_from(next).map_err(|_| ObjectError::InvalidArgument)?;
        Ok(*cursor as u64)
    }
}

struct AuxiliaryOpen(Arc<AuxiliaryFile>);

impl fmt::Debug for AuxiliaryOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuxiliaryOpen")
    }
}

impl PreparedPathOpen for AuxiliaryOpen {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.0.clone()
    }
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        Ok(())
    }
    fn rollback(self: Box<Self>) {}
}
