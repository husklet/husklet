use super::abi::AbiError;
use crate::{GuestMarshaller, GuestMemory, MarshalError};

#[derive(Clone, Debug, Eq, PartialEq)]
struct WriteEntry {
    address: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedMemoryCopyout {
    writes: Vec<WriteEntry>,
}

impl StagedMemoryCopyout {
    pub(crate) fn single(address: u64, bytes: Vec<u8>) -> Self {
        Self {
            writes: vec![WriteEntry { address, bytes }],
        }
    }

    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), AbiError> {
        for write in self.writes {
            let progress = marshaller.copy_to(write.address, &write.bytes);
            if let Some(fault) = progress.fault {
                return Err(MarshalError::Fault(fault).into());
            }
        }
        Ok(())
    }
}
