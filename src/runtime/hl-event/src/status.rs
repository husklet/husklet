/// Filesystem-style metadata shared by pollable event objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventStatus {
    pub mode: u32,
    pub size: u64,
    pub link_count: u64,
}

impl EventStatus {
    pub(crate) fn metadata(self) -> hl_descriptor::OfdMetadata {
        let timestamp = hl_descriptor::OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        hl_descriptor::OfdMetadata {
            device: 0,
            inode: 0,
            kind: ((self.mode >> 12) & 0xf) as u8,
            permissions: (self.mode & 0o7777) as u16,
            links: self.link_count,
            user: 0,
            group: 0,
            special_device: 0,
            size: self.size,
            blocks_512: self.size.saturating_add(511) / 512,
            block_size: 4096,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        }
    }
}
