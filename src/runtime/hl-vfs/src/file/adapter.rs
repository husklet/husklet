use hl_descriptor::{OfdMetadata, OfdTimestamp};

use crate::{Kind, Metadata};

pub(super) struct MetadataAdapter;

impl MetadataAdapter {
    // Takes the metadata by value so callers hand over the snapshot they built.
    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn descriptor(value: Metadata) -> OfdMetadata {
        OfdMetadata {
            device: value.identity.device,
            inode: value.identity.inode,
            kind: match value.kind {
                Kind::Regular => 8,
                Kind::Directory => 4,
                Kind::Symlink => 10,
                Kind::Character => 2,
                Kind::Block => 6,
                Kind::Fifo => 1,
                Kind::Socket => 12,
            },
            permissions: value.permissions.bits(),
            links: value.links,
            user: value.user,
            group: value.group,
            special_device: value.special_device,
            size: value.size,
            blocks_512: value.blocks_512,
            block_size: value.block_size,
            accessed: OfdTimestamp {
                seconds: value.accessed.seconds,
                nanoseconds: value.accessed.nanoseconds,
            },
            modified: OfdTimestamp {
                seconds: value.modified.seconds,
                nanoseconds: value.modified.nanoseconds,
            },
            changed: OfdTimestamp {
                seconds: value.changed.seconds,
                nanoseconds: value.changed.nanoseconds,
            },
        }
    }
}
