use hl_ipc::{IpcKey, SemaphoreId, SemaphoreLimits, SemaphoreMetadata, SemaphoreSetSnapshot, SemaphoreSnapshot};
use serde::{Deserialize, Serialize};

use super::metadata;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    limits: [u64; 6],
    generations: Vec<u32>,
    sets: Vec<Set>,
    undo: Vec<Adjustment>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Set {
    metadata: Detail,
    values: Vec<u16>,
    last_pids: Vec<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    ipc: metadata::Record,
    last_pid: u32,
    operated_at: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Adjustment {
    process: u32,
    slot: u32,
    generation: u32,
    index: u16,
    adjustment: i32,
}

impl Image {
    pub(super) fn from_values(limits: SemaphoreLimits, snapshot: &SemaphoreSnapshot) -> Result<Self, ()> {
        Ok(Self {
            limits: [
                limits.sets.try_into().map_err(|_| ())?,
                limits.set_semaphores.try_into().map_err(|_| ())?,
                limits.total_semaphores.try_into().map_err(|_| ())?,
                u64::from(limits.maximum_value),
                limits.operations.try_into().map_err(|_| ())?,
                limits.undo_entries.try_into().map_err(|_| ())?,
            ],
            generations: snapshot.generations.clone(),
            sets: snapshot.sets.iter().map(Set::from_value).collect(),
            undo: snapshot
                .undo
                .iter()
                .map(|(process, id, index, adjustment)| Adjustment {
                    process: *process,
                    slot: id.slot,
                    generation: id.generation,
                    index: *index,
                    adjustment: *adjustment,
                })
                .collect(),
        })
    }

    pub(super) fn into_values(self) -> Result<(SemaphoreLimits, SemaphoreSnapshot), ()> {
        Ok((
            SemaphoreLimits {
                sets: self.limits[0].try_into().map_err(|_| ())?,
                set_semaphores: self.limits[1].try_into().map_err(|_| ())?,
                total_semaphores: self.limits[2].try_into().map_err(|_| ())?,
                maximum_value: self.limits[3].try_into().map_err(|_| ())?,
                operations: self.limits[4].try_into().map_err(|_| ())?,
                undo_entries: self.limits[5].try_into().map_err(|_| ())?,
            },
            SemaphoreSnapshot {
                generations: self.generations,
                sets: self.sets.into_iter().map(Set::into_value).collect(),
                undo: self
                    .undo
                    .into_iter()
                    .map(|value| {
                        (
                            value.process,
                            SemaphoreId {
                                slot: value.slot,
                                generation: value.generation,
                            },
                            value.index,
                            value.adjustment,
                        )
                    })
                    .collect(),
            },
        ))
    }
}

impl Set {
    fn from_value(value: &SemaphoreSetSnapshot) -> Self {
        Self {
            metadata: Detail::from_value(&value.metadata),
            values: value.values.clone(),
            last_pids: value.last_pids.clone(),
        }
    }

    fn into_value(self) -> SemaphoreSetSnapshot {
        SemaphoreSetSnapshot {
            metadata: self.metadata.into_value(),
            values: self.values,
            last_pids: self.last_pids,
        }
    }
}

impl Detail {
    fn from_value(value: &SemaphoreMetadata) -> Self {
        Self {
            ipc: metadata::Record::new(
                value.id.slot,
                value.id.generation,
                value.key,
                value.owner,
                value.creator_uid,
                value.creator_gid,
                value.mode,
                value.created_at,
                value.changed_at,
            ),
            last_pid: value.last_pid,
            operated_at: value.operated_at,
        }
    }

    fn into_value(self) -> SemaphoreMetadata {
        SemaphoreMetadata {
            id: SemaphoreId {
                slot: self.ipc.slot,
                generation: self.ipc.generation,
            },
            key: self.ipc.key.map(IpcKey),
            owner: self.ipc.owner(),
            creator_uid: self.ipc.creator[0],
            creator_gid: self.ipc.creator[1],
            mode: self.ipc.mode,
            last_pid: self.last_pid,
            created_at: self.ipc.created_at,
            operated_at: self.operated_at,
            changed_at: self.ipc.changed_at,
        }
    }
}
