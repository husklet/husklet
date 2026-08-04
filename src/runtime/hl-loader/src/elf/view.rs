use crate::elf::ProgramHeader;
use crate::{FileRegion, InspectError, ProgramHeaderTable};

pub(crate) struct View<'a> {
    pub(crate) image: &'a [u8],
}
pub(crate) type ElfView<'a> = View<'a>;

impl<'a> ElfView<'a> {
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    pub(crate) const fn byte(&self, offset: usize) -> u8 {
        self.image[offset]
    }

    pub(crate) fn bytes(&self, offset: usize, size: usize) -> &[u8] {
        &self.image[offset..offset + size]
    }

    pub(crate) fn u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.bytes(offset, 2).try_into().expect("fixed ELF field"))
    }

    pub(crate) fn u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.bytes(offset, 4).try_into().expect("fixed ELF field"))
    }

    pub(crate) fn u64(&self, offset: usize) -> u64 {
        u64::from_le_bytes(self.bytes(offset, 8).try_into().expect("fixed ELF field"))
    }

    pub(crate) fn checked_region(
        &self,
        offset: u64,
        size: u64,
        error: InspectError,
    ) -> Result<FileRegion, InspectError> {
        let end = offset.checked_add(size).ok_or(error)?;
        if end > self.image.len() as u64 {
            return Err(error);
        }
        Ok(FileRegion::new(offset, size))
    }

    pub(crate) fn program_header(&self, table: ProgramHeaderTable, index: u16) -> Result<ProgramHeader, InspectError> {
        let offset = table
            .source()
            .offset()
            .checked_add(u64::from(index) * u64::from(table.entry_size()))
            .ok_or(InspectError::TruncatedProgramHeaders)?;
        let base = usize::try_from(offset).map_err(|_| InspectError::TruncatedProgramHeaders)?;
        Ok(ProgramHeader {
            kind: self.u32(base),
            flags: self.u32(base + 4),
            offset: self.u64(base + 8),
            virtual_address: self.u64(base + 16),
            file_size: self.u64(base + 32),
            memory_size: self.u64(base + 40),
            alignment: self.u64(base + 48),
        })
    }
}
