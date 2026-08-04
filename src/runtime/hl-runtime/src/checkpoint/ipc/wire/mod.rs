use std::io::Write;

use hl_ipc::{IPC_CHECKPOINT_VERSION, IpcCheckpointImage};
use serde::{Deserialize, Serialize};

use super::IPC_CHECKPOINT_BYTES_MAXIMUM;

mod message;
mod metadata;
mod pipe;
mod semaphore;
mod shared_memory;
mod task;

#[cfg(test)]
mod test;

const MAGIC: u32 = 0x4950_4348;
const VERSION: u32 = 1;
const HEADER_LENGTH: usize = 24;

struct BoundedBytes(Vec<u8>);

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("IPC checkpoint overflow"))?;
        if length > IPC_CHECKPOINT_BYTES_MAXIMUM {
            return Err(std::io::Error::other("IPC checkpoint limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Image {
    ipc: u32,
    pipe_generations: Vec<u32>,
    pipes: Vec<pipe::Image>,
    shared_memory: shared_memory::Image,
    messages: message::Image,
    semaphores: semaphore::Image,
    tasks: Vec<task::Reference>,
}

pub(super) struct Codec;

impl Codec {
    pub(super) fn encode(image: &IpcCheckpointImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        if !Self::canonical(image) {
            return Err(());
        }
        let wire = Image::from_value(image)?;
        let mut payload = BoundedBytes(Vec::new());
        serde_json::to_writer(&mut payload, &wire).map_err(|_| ())?;
        let payload = payload.0;
        let length = u64::try_from(payload.len()).map_err(|_| ())?;
        let capacity = HEADER_LENGTH.checked_add(payload.len()).ok_or(())?;
        if capacity > IPC_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&Self::checksum(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<IpcCheckpointImage, ()> {
        if bytes.len() < HEADER_LENGTH || bytes.len() > IPC_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if word(0) != MAGIC || word(4) != VERSION {
            return Err(());
        }
        let length = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let length = usize::try_from(length).map_err(|_| ())?;
        if HEADER_LENGTH.checked_add(length) != Some(bytes.len()) {
            return Err(());
        }
        let expected = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload = &bytes[HEADER_LENGTH..];
        if expected != Self::checksum(payload) {
            return Err(());
        }
        let wire: Image = serde_json::from_slice(payload).map_err(|_| ())?;
        let image = wire.into_value()?;
        image.validate().map_err(|_| ())?;
        if !Self::canonical(&image) {
            return Err(());
        }
        Ok(image)
    }

    fn canonical(image: &IpcCheckpointImage) -> bool {
        image.pipes.windows(2).all(|pair| pair[0].id < pair[1].id)
            && image.shared.segments.windows(2).all(|pair| pair[0].id < pair[1].id)
            && image.shared.attachments.windows(2).all(|pair| pair[0].0 < pair[1].0)
            && image.backings.windows(2).all(|pair| pair[0].segment < pair[1].segment)
            && image
                .messages
                .queues
                .windows(2)
                .all(|pair| pair[0].metadata.id < pair[1].metadata.id)
            && image
                .semaphores
                .sets
                .windows(2)
                .all(|pair| pair[0].metadata.id < pair[1].metadata.id)
            && image.semaphores.undo.windows(2).all(|pair| pair[0] < pair[1])
            && image.tasks.windows(2).all(|pair| pair[0].process < pair[1].process)
    }

    fn checksum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    }
}

impl Image {
    fn from_value(value: &IpcCheckpointImage) -> Result<Self, ()> {
        Ok(Self {
            ipc: value.version,
            pipe_generations: value.pipe_generations.clone(),
            pipes: value
                .pipes
                .iter()
                .map(pipe::Image::from_value)
                .collect::<Result<_, _>>()?,
            shared_memory: shared_memory::Image::from_values(value.shared_limits, &value.shared, &value.backings)?,
            messages: message::Image::from_values(value.message_limits, &value.messages)?,
            semaphores: semaphore::Image::from_values(value.semaphore_limits, &value.semaphores)?,
            tasks: value.tasks.iter().copied().map(task::Reference::from_value).collect(),
        })
    }

    fn into_value(self) -> Result<IpcCheckpointImage, ()> {
        if self.ipc != IPC_CHECKPOINT_VERSION {
            return Err(());
        }
        let (shared_limits, shared, backings) = self.shared_memory.into_values()?;
        let (message_limits, messages) = self.messages.into_values()?;
        let (semaphore_limits, semaphores) = self.semaphores.into_values()?;
        Ok(IpcCheckpointImage {
            version: self.ipc,
            pipe_generations: self.pipe_generations,
            pipes: self
                .pipes
                .into_iter()
                .map(pipe::Image::into_value)
                .collect::<Result<_, _>>()?,
            shared_limits,
            shared,
            backings,
            message_limits,
            messages,
            semaphore_limits,
            semaphores,
            tasks: self
                .tasks
                .into_iter()
                .map(task::Reference::into_value)
                .collect::<Result<_, _>>()?,
        })
    }
}
