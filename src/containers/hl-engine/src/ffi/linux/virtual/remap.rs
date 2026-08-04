use hl_isa::AddressRange;
use hl_memory::MapRequest;

use super::virtual_memory::{Memory, MemoryError};

impl Memory {
    pub(super) fn remap_host(
        &self,
        source: AddressRange,
        destination: u64,
        request: MapRequest,
        keep: bool,
    ) -> Result<(), MemoryError> {
        let (source, old_length) = self.host_range(source.start().get(), source.length())?;
        let (destination, _) = self.host_range(destination, request.length)?;
        self.host
            .remap(
                source as usize,
                old_length,
                destination as usize,
                request.length as usize,
                keep,
            )
            .map_err(|()| MemoryError::Host)
    }
}
