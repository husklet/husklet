use hl_descriptor::{ObjectError, OfdDirectoryEntry};

pub(super) struct Entries;

impl Entries {
    pub(super) fn parse(bytes: &[u8]) -> Result<Vec<OfdDirectoryEntry>, ObjectError> {
        let mut entries = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let header = bytes.get(offset..offset + 19).ok_or(ObjectError::Io)?;
            let length = usize::from(u16::from_ne_bytes([header[16], header[17]]));
            let end = offset.checked_add(length).ok_or(ObjectError::Io)?;
            if length < 20 || end > bytes.len() {
                return Err(ObjectError::Io);
            }
            let record = &bytes[offset..end];
            let names = &record[19..];
            let name_end = names.iter().position(|byte| *byte == 0).ok_or(ObjectError::Io)?;
            entries.push(OfdDirectoryEntry {
                inode: u64::from_ne_bytes(header[0..8].try_into().map_err(|_| ObjectError::Io)?),
                cookie: i64::from_ne_bytes(header[8..16].try_into().map_err(|_| ObjectError::Io)?),
                file_type: header[18],
                name: names[..name_end].to_vec(),
            });
            offset = end;
        }
        Ok(entries)
    }
}
