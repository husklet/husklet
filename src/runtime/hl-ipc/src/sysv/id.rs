use crate::{MessageQueueId, SemaphoreId, SharedMemoryId};

pub const SHARED_MEMORY_IDENTIFIERS: u32 = 4096;
pub const SEMAPHORE_IDENTIFIERS: u32 = 512;
pub const MESSAGE_QUEUE_IDENTIFIERS: u32 = 512;

macro_rules! linux_id {
    ($kind:ty, $capacity:ident) => {
        impl $kind {
            pub fn linux_id(self) -> Option<i32> {
                let sequence = self.generation.checked_sub(1)?;
                let value = sequence.checked_mul($capacity)?.checked_add(self.slot)?;
                i32::try_from(value).ok()
            }

            pub fn from_linux_id(value: i32) -> Option<Self> {
                let value = u32::try_from(value).ok()?;
                Some(Self {
                    slot: value % $capacity,
                    generation: (value / $capacity).checked_add(1)?,
                })
            }
        }
    };
}

linux_id!(SharedMemoryId, SHARED_MEMORY_IDENTIFIERS);
linux_id!(SemaphoreId, SEMAPHORE_IDENTIFIERS);
linux_id!(MessageQueueId, MESSAGE_QUEUE_IDENTIFIERS);
