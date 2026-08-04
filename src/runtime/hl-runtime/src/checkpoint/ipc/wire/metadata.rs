use hl_ipc::{Credentials, IpcKey};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) slot: u32,
    pub(super) generation: u32,
    pub(super) key: Option<i32>,
    pub(super) owner: [u32; 2],
    pub(super) creator: [u32; 2],
    pub(super) mode: u16,
    pub(super) created_at: u64,
    pub(super) changed_at: u64,
}

impl Record {
    pub(super) fn new(
        slot: u32,
        generation: u32,
        key: Option<IpcKey>,
        owner: Credentials,
        creator_uid: u32,
        creator_gid: u32,
        mode: u16,
        created_at: u64,
        changed_at: u64,
    ) -> Self {
        Self {
            slot,
            generation,
            key: key.map(|key| key.0),
            owner: [owner.uid, owner.gid],
            creator: [creator_uid, creator_gid],
            mode,
            created_at,
            changed_at,
        }
    }

    pub(super) fn owner(&self) -> Credentials {
        Credentials {
            uid: self.owner[0],
            gid: self.owner[1],
        }
    }
}
