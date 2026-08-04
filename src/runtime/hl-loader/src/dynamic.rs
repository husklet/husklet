use crate::{InspectError, RelocationWrite};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicEntry {
    pub tag: i64,
    pub value: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicTable {
    link_address: u64,
    entries: Vec<DynamicEntry>,
    relocation_writes: Vec<RelocationWrite>,
}

impl DynamicTable {
    const fn new(link_address: u64, entries: Vec<DynamicEntry>) -> Self {
        Self {
            link_address,
            entries,
            relocation_writes: Vec::new(),
        }
    }

    #[must_use]
    pub const fn link_address(&self) -> u64 {
        self.link_address
    }

    #[must_use]
    pub fn entries(&self) -> &[DynamicEntry] {
        &self.entries
    }

    /// Empty under retained-C parity: ld.so or static-PIE startup owns these writes.
    #[must_use]
    pub fn relocation_writes(&self) -> &[RelocationWrite] {
        &self.relocation_writes
    }

    pub(crate) fn parse(image: &[u8], offset: u64, size: u64, link_address: u64) -> Result<Self, InspectError> {
        if size == 0 || size % 16 != 0 {
            return Err(InspectError::InvalidDynamicTable);
        }
        let end = offset.checked_add(size).ok_or(InspectError::InvalidDynamicTable)?;
        if end > image.len() as u64 {
            return Err(InspectError::InvalidDynamicTable);
        }
        let mut entries = Vec::new();
        for entry_offset in (offset..end).step_by(16) {
            let entry_offset = usize::try_from(entry_offset).map_err(|_| InspectError::InvalidDynamicTable)?;
            let tag = i64::from_le_bytes(
                image[entry_offset..entry_offset + 8]
                    .try_into()
                    .expect("validated dynamic entry"),
            );
            let value = u64::from_le_bytes(
                image[entry_offset + 8..entry_offset + 16]
                    .try_into()
                    .expect("validated dynamic value"),
            );
            if tag == 0 {
                return Ok(Self::new(link_address, entries));
            }
            if Self::singleton_tag(tag) && Self::contains_tag(&entries, tag) {
                return Err(InspectError::DuplicateDynamicTag);
            }
            entries.push(DynamicEntry { tag, value });
        }
        Err(InspectError::UnterminatedDynamicTable)
    }

    fn singleton_tag(tag: i64) -> bool {
        matches!(
            tag,
            2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 11 | 17 | 18 | 19 | 20 | 23 | 35 | 36 | 37
        )
    }

    fn contains_tag(entries: &[DynamicEntry], tag: i64) -> bool {
        for entry in entries {
            if entry.tag == tag {
                return true;
            }
        }
        false
    }
}
