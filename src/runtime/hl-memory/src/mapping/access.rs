use hl_isa::GuestAddress;

use crate::{Backing, MemoryError, Protection};

use super::{Coordinator, MemoryAccessHost};

const FETCH_STAGE: usize = 64;

impl<H: MemoryAccessHost> Coordinator<H> {
    /// Returns the first logically accessible representation span.
    ///
    /// The result deliberately stops at the current mapping/backing boundary.
    /// User-copy callers can publish this prefix before resolving the next span.
    pub fn access_prefix(&self, address: GuestAddress, length: u64, access: Protection) -> Result<u64, MemoryError> {
        if length == 0 {
            return Ok(0);
        }
        address.get().checked_add(length).ok_or(MemoryError::AddressOverflow)?;
        let resolution = self
            .ledger
            .resolve(address, access)
            .ok_or(MemoryError::NoAddressSpace)?;
        let mut available = resolution.contiguous.min(length);
        if let Backing::File { identity, .. } = resolution.region.backing() {
            available = self
                .host
                .file_prefix(identity, resolution.backing_offset, available, address)?
                .min(available);
        }
        Ok(available)
    }

    /// Copies one access across mapping and 4 KiB guest-page boundaries.
    ///
    /// Fetch-sized accesses up to 64 bytes are staged, so the destination is
    /// unchanged when any later span is inaccessible. Larger diagnostic reads are
    /// validated in a first pass before copying. An x86 instruction may cross two
    /// adjacent executable mappings without publishing a partial instruction to
    /// the decoder.
    pub fn read_spans(&self, address: GuestAddress, output: &mut [u8], access: Protection) -> Result<(), MemoryError> {
        if output.is_empty() {
            return Ok(());
        }
        let length = u64::try_from(output.len()).map_err(|_| MemoryError::AddressOverflow)?;
        address.get().checked_add(length).ok_or(MemoryError::AddressOverflow)?;
        if output.len() <= FETCH_STAGE {
            let mut staged = [0_u8; FETCH_STAGE];
            match self.read(address, &mut staged[..output.len()], access) {
                Ok(()) => {
                    output.copy_from_slice(&staged[..output.len()]);
                    return Ok(());
                }
                Err(MemoryError::NoAddressSpace) => {}
                Err(error) => return Err(error),
            }
            self.walk_spans(address, output.len(), Some(&mut staged[..output.len()]), access)?;
            output.copy_from_slice(&staged[..output.len()]);
            return Ok(());
        }
        self.walk_spans(address, output.len(), None, access)?;
        self.walk_spans(address, output.len(), Some(output), access)
    }

    fn walk_spans(
        &self,
        address: GuestAddress,
        length: usize,
        mut output: Option<&mut [u8]>,
        access: Protection,
    ) -> Result<(), MemoryError> {
        let mut copied = 0_usize;
        while copied < length {
            let current = address
                .get()
                .checked_add(copied as u64)
                .ok_or(MemoryError::AddressOverflow)?;
            let page = 4096_u64 - (current & 4095);
            let remaining = u64::try_from(length - copied).map_err(|_| MemoryError::AddressOverflow)?;
            let requested = remaining.min(page);
            let available = self.access_prefix(GuestAddress::new(current), requested, access)?;
            if available == 0 {
                return Err(MemoryError::NoAddressSpace);
            }
            let span = usize::try_from(available.min(requested)).map_err(|_| MemoryError::AddressOverflow)?;
            if let Some(bytes) = output.as_deref_mut() {
                self.read(GuestAddress::new(current), &mut bytes[copied..copied + span], access)?;
            }
            copied += span;
        }
        Ok(())
    }
}
