use hl_isa::{AddressRange, GuestAddress};

use crate::{Backing, MapRequest, MappingHost, MemoryError, Placement, Protection, SharedBackingRef, SharedObjectId};

#[derive(Clone, Copy, Debug, Default)]
pub struct TestMappingHost;

impl TestMappingHost {
    pub fn shared_request(object: SharedObjectId) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0x1000)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::Shared(SharedBackingRef {
                object,
                offset: 0,
                length: 4096,
                write_shared: true,
            }),
            backing_offset: 0,
        }
    }
}

impl MappingHost for TestMappingHost {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
}
