use hl_ipc::{
    IpcKey, MessageLimits, MessageQueueId, MessageQueueMetadata, MessageQueueSnapshot, MessageSnapshot, QueueSnapshot,
};
use serde::{Deserialize, Serialize};

use super::metadata;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    limits: [u64; 6],
    generations: Vec<u32>,
    queues: Vec<Queue>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    metadata: Detail,
    messages: Vec<Message>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    ipc: metadata::Record,
    maximum_bytes: u64,
    bytes: u64,
    messages: u64,
    last_send_pid: u32,
    last_receive_pid: u32,
    sent_at: Option<u64>,
    received_at: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Message {
    message_type: i64,
    bytes: Vec<u8>,
}

impl Image {
    pub(super) fn from_values(limits: MessageLimits, snapshot: &MessageQueueSnapshot) -> Result<Self, ()> {
        Ok(Self {
            limits: [
                limits.queues.try_into().map_err(|_| ())?,
                limits.queue_bytes.try_into().map_err(|_| ())?,
                limits.queue_messages.try_into().map_err(|_| ())?,
                limits.total_bytes.try_into().map_err(|_| ())?,
                limits.total_messages.try_into().map_err(|_| ())?,
                limits.message_bytes.try_into().map_err(|_| ())?,
            ],
            generations: snapshot.generations.clone(),
            queues: snapshot
                .queues
                .iter()
                .map(Queue::from_value)
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn into_values(self) -> Result<(MessageLimits, MessageQueueSnapshot), ()> {
        Ok((
            MessageLimits {
                queues: self.limits[0].try_into().map_err(|_| ())?,
                queue_bytes: self.limits[1].try_into().map_err(|_| ())?,
                queue_messages: self.limits[2].try_into().map_err(|_| ())?,
                total_bytes: self.limits[3].try_into().map_err(|_| ())?,
                total_messages: self.limits[4].try_into().map_err(|_| ())?,
                message_bytes: self.limits[5].try_into().map_err(|_| ())?,
            },
            MessageQueueSnapshot {
                generations: self.generations,
                queues: self
                    .queues
                    .into_iter()
                    .map(Queue::into_value)
                    .collect::<Result<_, _>>()?,
            },
        ))
    }
}

impl Queue {
    fn from_value(value: &QueueSnapshot) -> Result<Self, ()> {
        Ok(Self {
            metadata: Detail::from_value(&value.metadata)?,
            messages: value
                .messages
                .iter()
                .map(|message| Message {
                    message_type: message.message_type,
                    bytes: message.bytes.clone(),
                })
                .collect(),
        })
    }

    fn into_value(self) -> Result<QueueSnapshot, ()> {
        Ok(QueueSnapshot {
            metadata: self.metadata.into_value()?,
            messages: self
                .messages
                .into_iter()
                .map(|message| MessageSnapshot {
                    message_type: message.message_type,
                    bytes: message.bytes,
                })
                .collect(),
        })
    }
}

impl Detail {
    fn from_value(value: &MessageQueueMetadata) -> Result<Self, ()> {
        Ok(Self {
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
            maximum_bytes: value.maximum_bytes.try_into().map_err(|_| ())?,
            bytes: value.bytes.try_into().map_err(|_| ())?,
            messages: value.messages.try_into().map_err(|_| ())?,
            last_send_pid: value.last_send_pid,
            last_receive_pid: value.last_receive_pid,
            sent_at: value.sent_at,
            received_at: value.received_at,
        })
    }

    fn into_value(self) -> Result<MessageQueueMetadata, ()> {
        Ok(MessageQueueMetadata {
            id: MessageQueueId {
                slot: self.ipc.slot,
                generation: self.ipc.generation,
            },
            key: self.ipc.key.map(IpcKey),
            owner: self.ipc.owner(),
            creator_uid: self.ipc.creator[0],
            creator_gid: self.ipc.creator[1],
            mode: self.ipc.mode,
            maximum_bytes: self.maximum_bytes.try_into().map_err(|_| ())?,
            bytes: self.bytes.try_into().map_err(|_| ())?,
            messages: self.messages.try_into().map_err(|_| ())?,
            last_send_pid: self.last_send_pid,
            last_receive_pid: self.last_receive_pid,
            created_at: self.ipc.created_at,
            sent_at: self.sent_at,
            received_at: self.received_at,
            changed_at: self.ipc.changed_at,
        })
    }
}
