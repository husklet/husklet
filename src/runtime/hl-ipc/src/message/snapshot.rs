use super::model::{MessageError, MessageLimits, MessageQueueSnapshot, MessageSnapshot, QueueSnapshot};
use super::queue::{Message, MessageQueueNamespace, Queue, Slot, State};

impl MessageQueueNamespace {
    pub fn snapshot(&self) -> MessageQueueSnapshot {
        let state = self.lock();
        MessageQueueSnapshot {
            generations: state.slots.iter().map(|slot| slot.generation).collect(),
            queues: state
                .slots
                .iter()
                .filter_map(|slot| {
                    let queue = slot.queue.as_ref()?;
                    Some(QueueSnapshot {
                        metadata: queue.metadata.clone(),
                        messages: queue
                            .messages
                            .iter()
                            .map(|message| MessageSnapshot {
                                message_type: message.message_type,
                                bytes: message.bytes.clone(),
                            })
                            .collect(),
                    })
                })
                .collect(),
        }
    }

    pub fn restore(limits: MessageLimits, snapshot: MessageQueueSnapshot) -> Result<Self, MessageError> {
        let namespace = Self::new(limits)?;
        let mut state = namespace.lock();
        if snapshot.generations.len() > limits.queues || snapshot.generations.contains(&0) {
            return Err(MessageError::ResourceLimit);
        }
        state.slots = snapshot
            .generations
            .iter()
            .map(|generation| Slot {
                generation: *generation,
                queue: None,
            })
            .collect();
        for item in snapshot.queues {
            namespace.restore_queue(&mut state, item)?;
        }
        drop(state);
        Ok(namespace)
    }

    fn restore_queue(&self, state: &mut State, item: QueueSnapshot) -> Result<(), MessageError> {
        let index = item.metadata.id.slot as usize;
        if index >= self.limits.queues
            || item.metadata.id.generation == 0
            || state.slots.get(index).map(|slot| slot.generation) != Some(item.metadata.id.generation)
            || item.metadata.mode & !0o777 != 0
            || state.slots.get(index).is_some_and(|slot| slot.queue.is_some())
        {
            return Err(MessageError::InvalidArgument);
        }
        let bytes = item.messages.iter().try_fold(0usize, |total, message| {
            (message.message_type > 0 && message.bytes.len() <= self.limits.message_bytes)
                .then(|| total.checked_add(message.bytes.len()))
                .flatten()
                .ok_or(MessageError::InvalidArgument)
        })?;
        if item.metadata.maximum_bytes == 0
            || bytes != item.metadata.bytes
            || item.messages.len() != item.metadata.messages
            || item.messages.len() > self.limits.queue_messages
            || state
                .bytes
                .checked_add(bytes)
                .is_none_or(|value| value > self.limits.total_bytes)
            || state
                .messages
                .checked_add(item.messages.len())
                .is_none_or(|value| value > self.limits.total_messages)
            || item.metadata.key.is_some_and(|key| Self::key_id(state, key).is_some())
        {
            return Err(MessageError::ResourceLimit);
        }
        state.bytes += bytes;
        state.messages += item.messages.len();
        state.slots[index] = Slot {
            generation: state.slots[index].generation,
            queue: Some(Queue {
                metadata: item.metadata,
                messages: item
                    .messages
                    .into_iter()
                    .map(|message| Message {
                        message_type: message.message_type,
                        bytes: message.bytes,
                        reservation: None,
                    })
                    .collect(),
            }),
        };
        Ok(())
    }
}
