use hl_ipc::{
    Credentials, IpcKey, SharedBackingCheckpoint, SharedBackingKey, SharedMemoryId, SharedMemoryLimits,
    SharedMemoryMetadata, SharedMemorySnapshot,
};
use hl_memory::SharedObjectId;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    limits: [u64; 4],
    generations: Vec<u32>,
    segments: Vec<Segment>,
    attachments: Vec<Attachment>,
    next_attachment: u64,
    backings: Vec<Backing>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    slot: u32,
    generation: u32,
    key: Option<i32>,
    object_slot: u32,
    object_generation: u32,
    size: u64,
    owner: [u32; 2],
    creator: [u32; 2],
    mode: u16,
    creator_pid: u32,
    last_pid: u32,
    attaches: u64,
    marked_for_removal: bool,
    created_at: u64,
    attached_at: Option<u64>,
    detached_at: Option<u64>,
    changed_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    token: u64,
    slot: u32,
    generation: u32,
    process: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Backing {
    segment_slot: u32,
    segment_generation: u32,
    object_slot: u32,
    object_generation: u32,
    resource: u64,
}

impl Image {
    pub(super) fn from_values(
        limits: SharedMemoryLimits,
        snapshot: &SharedMemorySnapshot,
        backings: &[SharedBackingCheckpoint],
    ) -> Result<Self, ()> {
        Ok(Self {
            limits: [
                limits.segments.try_into().map_err(|_| ())?,
                limits.segment_bytes.try_into().map_err(|_| ())?,
                limits.total_bytes.try_into().map_err(|_| ())?,
                limits.attachments.try_into().map_err(|_| ())?,
            ],
            generations: snapshot.generations.clone(),
            segments: snapshot
                .segments
                .iter()
                .copied()
                .map(Segment::from_value)
                .collect::<Result<_, _>>()?,
            attachments: snapshot
                .attachments
                .iter()
                .map(|(token, segment, process)| Attachment {
                    token: *token,
                    slot: segment.slot,
                    generation: segment.generation,
                    process: *process,
                })
                .collect(),
            next_attachment: snapshot.next_attachment,
            backings: backings.iter().copied().map(Backing::from_value).collect(),
        })
    }

    pub(super) fn into_values(
        self,
    ) -> Result<(SharedMemoryLimits, SharedMemorySnapshot, Vec<SharedBackingCheckpoint>), ()> {
        Ok((
            SharedMemoryLimits {
                segments: self.limits[0].try_into().map_err(|_| ())?,
                segment_bytes: self.limits[1].try_into().map_err(|_| ())?,
                total_bytes: self.limits[2].try_into().map_err(|_| ())?,
                attachments: self.limits[3].try_into().map_err(|_| ())?,
            },
            SharedMemorySnapshot {
                generations: self.generations,
                segments: self
                    .segments
                    .into_iter()
                    .map(Segment::into_value)
                    .collect::<Result<_, _>>()?,
                attachments: self
                    .attachments
                    .into_iter()
                    .map(|value| {
                        (
                            value.token,
                            SharedMemoryId {
                                slot: value.slot,
                                generation: value.generation,
                            },
                            value.process,
                        )
                    })
                    .collect(),
                next_attachment: self.next_attachment,
            },
            self.backings
                .into_iter()
                .map(Backing::into_value)
                .collect::<Result<_, _>>()?,
        ))
    }
}

impl Segment {
    fn from_value(value: SharedMemoryMetadata) -> Result<Self, ()> {
        Ok(Self {
            slot: value.id.slot,
            generation: value.id.generation,
            key: value.key.map(|key| key.0),
            object_slot: value.backing.slot,
            object_generation: value.backing.generation,
            size: value.size.try_into().map_err(|_| ())?,
            owner: [value.owner.uid, value.owner.gid],
            creator: [value.creator_uid, value.creator_gid],
            mode: value.mode,
            creator_pid: value.creator_pid,
            last_pid: value.last_pid,
            attaches: value.attaches.try_into().map_err(|_| ())?,
            marked_for_removal: value.marked_for_removal,
            created_at: value.created_at,
            attached_at: value.attached_at,
            detached_at: value.detached_at,
            changed_at: value.changed_at,
        })
    }

    fn into_value(self) -> Result<SharedMemoryMetadata, ()> {
        Ok(SharedMemoryMetadata {
            id: SharedMemoryId {
                slot: self.slot,
                generation: self.generation,
            },
            key: self.key.map(IpcKey),
            backing: SharedObjectId {
                slot: self.object_slot,
                generation: self.object_generation,
            },
            size: self.size.try_into().map_err(|_| ())?,
            owner: Credentials {
                uid: self.owner[0],
                gid: self.owner[1],
            },
            creator_uid: self.creator[0],
            creator_gid: self.creator[1],
            mode: self.mode,
            creator_pid: self.creator_pid,
            last_pid: self.last_pid,
            attaches: self.attaches.try_into().map_err(|_| ())?,
            marked_for_removal: self.marked_for_removal,
            created_at: self.created_at,
            attached_at: self.attached_at,
            detached_at: self.detached_at,
            changed_at: self.changed_at,
        })
    }
}

impl Backing {
    fn from_value(value: SharedBackingCheckpoint) -> Self {
        Self {
            segment_slot: value.segment.slot,
            segment_generation: value.segment.generation,
            object_slot: value.object.slot,
            object_generation: value.object.generation,
            resource: value.resource.get(),
        }
    }

    fn into_value(self) -> Result<SharedBackingCheckpoint, ()> {
        Ok(SharedBackingCheckpoint {
            segment: SharedMemoryId {
                slot: self.segment_slot,
                generation: self.segment_generation,
            },
            object: SharedObjectId {
                slot: self.object_slot,
                generation: self.object_generation,
            },
            resource: SharedBackingKey::new(self.resource).ok_or(())?,
        })
    }
}
