use super::abi::Error;
use crate::{GuestMarshaller, GuestMemory};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceUsage {
    pub user_seconds: i64,
    pub user_microseconds: i64,
    pub system_seconds: i64,
    pub system_microseconds: i64,
    pub maximum_resident_set: i64,
    pub minor_faults: i64,
    pub major_faults: i64,
    pub voluntary_switches: i64,
    pub involuntary_switches: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedProcessCopyout {
    pub(super) destination: u64,
    pub(super) bytes: Vec<u8>,
}

impl StagedProcessCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), Error> {
        let progress = marshaller.copy_to(self.destination, &self.bytes);
        progress.fault.map_or(Ok(()), |_| Err(Error::Fault))
    }
}
