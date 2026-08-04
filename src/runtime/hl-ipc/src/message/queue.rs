use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};

use super::model::{
    MESSAGE_FLAGS, MSG_COPY, MSG_EXCEPT, MSG_NOERROR, MSG_NOWAIT, MessageError, MessageLimits, MessageQueueId,
    MessageQueueMetadata, MessageReceive, MsgGetRequest,
};
use crate::{Credentials, IPC_PRIVATE, IpcKey, PreparedMessageReceive};
use hl_sync::WaitQueue;

#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub(crate) message_type: i64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) reservation: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct Queue {
    pub(crate) metadata: MessageQueueMetadata,
    pub(crate) messages: VecDeque<Message>,
}

#[derive(Debug)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) queue: Option<Queue>,
}

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) slots: Vec<Slot>,
    pub(crate) bytes: usize,
    pub(crate) messages: usize,
    pub(crate) next_reservation: u64,
}

#[derive(Debug)]
pub struct QueueNamespace {
    pub(crate) limits: MessageLimits,
    state: Mutex<State>,
    pub(super) changed: WaitQueue,
    pub(super) waiters: AtomicUsize,
}
pub type MessageQueueNamespace = QueueNamespace;

impl MessageQueueNamespace {
    pub fn new(limits: MessageLimits) -> Result<Self, MessageError> {
        if limits.queues == 0
            || limits.queue_bytes > limits.total_bytes
            || limits.queue_messages > limits.total_messages
            || limits.message_bytes > limits.queue_bytes
        {
            return Err(MessageError::InvalidArgument);
        }
        Ok(Self {
            limits,
            state: Mutex::new(State {
                slots: Vec::new(),
                bytes: 0,
                messages: 0,
                next_reservation: 1,
            }),
            changed: WaitQueue::new(),
            waiters: AtomicUsize::new(0),
        })
    }

    pub fn msgget(&self, request: MsgGetRequest) -> Result<MessageQueueId, MessageError> {
        if request.mode & !0o777 != 0 {
            return Err(MessageError::InvalidArgument);
        }
        let mut state = self.lock();
        if request.key != IPC_PRIVATE {
            return self.keyed_get(&mut state, request);
        }
        self.create(&mut state, request)
    }

    pub fn send(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        bytes: &[u8],
        flags: u32,
        now: u64,
    ) -> Result<(), MessageError> {
        Self::validate_send(message_type, bytes.len(), flags, self.limits.message_bytes)?;
        let mut state = self.lock();
        let queue = Self::queue(&state, id)?;
        Self::require(&queue.metadata, actor, 0o2)?;
        if !self.has_capacity(&state, queue, bytes.len()) {
            return Err(MessageError::Again);
        }
        Self::commit_send(&mut state, id, pid, message_type, bytes, now)?;
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub fn receive(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        maximum: usize,
        flags: u32,
        now: u64,
    ) -> Result<MessageReceive, MessageError> {
        self.prepare_receive(id, actor, pid, message_type, maximum, flags, now)?
            .commit()
    }

    pub fn prepare_receive(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        maximum: usize,
        flags: u32,
        now: u64,
    ) -> Result<PreparedMessageReceive<'_>, MessageError> {
        Self::validate_receive(message_type, flags)?;
        let mut state = self.lock();
        let queue = Self::queue(&state, id)?;
        Self::require(&queue.metadata, actor, 0o4)?;
        let index = Self::select(queue, message_type, flags).ok_or(MessageError::NoMessage)?;
        let message = &queue.messages[index];
        if message.bytes.len() > maximum && flags & MSG_NOERROR == 0 {
            return Err(MessageError::TooBig);
        }
        let result = MessageReceive {
            message_type: message.message_type,
            bytes: message.bytes[..message.bytes.len().min(maximum)].to_vec(),
            truncated: message.bytes.len() > maximum,
        };
        let reservation = if flags & MSG_COPY == 0 {
            let reservation = state.next_reservation;
            state.next_reservation = state.next_reservation.wrapping_add(1).max(1);
            Self::queue_mut(&mut state, id)?.messages[index].reservation = Some(reservation);
            Some(reservation)
        } else {
            None
        };
        Ok(PreparedMessageReceive {
            namespace: self,
            id,
            reservation,
            result,
            pid,
            now,
            committed: false,
        })
    }

    pub(crate) fn commit_reservation(
        &self,
        id: MessageQueueId,
        reservation: u64,
        pid: u32,
        now: u64,
    ) -> Result<(), MessageError> {
        let mut state = self.lock();
        let queue = Self::queue(&state, id)?;
        let index = queue
            .messages
            .iter()
            .position(|message| message.reservation == Some(reservation))
            .ok_or(MessageError::Removed)?;
        Self::commit_receive(&mut state, id, index, pid, now)?;
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn abort_reservation(&self, id: MessageQueueId, reservation: u64) {
        let mut state = self.lock();
        let Ok(queue) = Self::queue_mut(&mut state, id) else {
            return;
        };
        if let Some(message) = queue
            .messages
            .iter_mut()
            .find(|message| message.reservation == Some(reservation))
        {
            message.reservation = None;
        }
    }

    pub fn remove(&self, id: MessageQueueId, actor: Credentials, pid: u32, now: u64) -> Result<(), MessageError> {
        let mut state = self.lock();
        let queue = Self::queue(&state, id)?;
        if actor.uid != 0 && actor.uid != queue.metadata.owner.uid && actor.uid != queue.metadata.creator_uid {
            return Err(MessageError::Permission);
        }
        let (bytes, messages) = {
            let slot = state.slots.get_mut(id.slot as usize).ok_or(MessageError::NotFound)?;
            let mut queue = slot.queue.take().ok_or(MessageError::NotFound)?;
            queue.metadata.last_send_pid = pid;
            queue.metadata.changed_at = now;
            slot.generation = slot.generation.wrapping_add(1).max(1);
            (queue.metadata.bytes, queue.metadata.messages)
        };
        state.bytes -= bytes;
        state.messages -= messages;
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    pub fn metadata(&self, id: MessageQueueId) -> Result<MessageQueueMetadata, MessageError> {
        Ok(Self::queue(&self.lock(), id)?.metadata.clone())
    }

    pub fn set_permissions(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        owner: Credentials,
        mode: u16,
        now: u64,
    ) -> Result<(), MessageError> {
        if mode & !0o777 != 0 {
            return Err(MessageError::InvalidArgument);
        }
        let mut state = self.lock();
        let queue = Self::queue_mut(&mut state, id)?;
        if actor.uid != 0 && actor.uid != queue.metadata.owner.uid && actor.uid != queue.metadata.creator_uid {
            return Err(MessageError::Permission);
        }
        queue.metadata.owner = owner;
        queue.metadata.mode = mode;
        queue.metadata.changed_at = now;
        Ok(())
    }

    pub fn set_control(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        owner: Credentials,
        mode: u16,
        maximum_bytes: usize,
        now: u64,
    ) -> Result<(), MessageError> {
        if mode & !0o777 != 0 || maximum_bytes == 0 {
            return Err(MessageError::InvalidArgument);
        }
        let mut state = self.lock();
        let queue = Self::queue_mut(&mut state, id)?;
        if actor.uid != 0 && actor.uid != queue.metadata.owner.uid && actor.uid != queue.metadata.creator_uid {
            return Err(MessageError::Permission);
        }
        if actor.uid != 0 && maximum_bytes > self.limits.queue_bytes {
            return Err(MessageError::Permission);
        }
        let capacity_increased = maximum_bytes > queue.metadata.maximum_bytes;
        queue.metadata.owner = owner;
        queue.metadata.mode = mode;
        queue.metadata.maximum_bytes = maximum_bytes;
        queue.metadata.changed_at = now;
        drop(state);
        if capacity_increased {
            self.changed.notify_all();
        }
        Ok(())
    }

    pub const fn fork(&self, _parent: u32, _child: u32) {}
    pub const fn exit(&self, _pid: u32) {}

    fn create(&self, state: &mut State, request: MsgGetRequest) -> Result<MessageQueueId, MessageError> {
        let index = state.slots.iter().position(|slot| slot.queue.is_none());
        let index = match index {
            Some(index) => index,
            None if state.slots.len() < self.limits.queues => {
                state.slots.push(Slot {
                    generation: 1,
                    queue: None,
                });
                state.slots.len() - 1
            }
            None => return Err(MessageError::ResourceLimit),
        };
        let id = MessageQueueId {
            slot: index as u32,
            generation: state.slots[index].generation,
        };
        state.slots[index].queue = Some(Queue {
            metadata: MessageQueueMetadata {
                id,
                key: (request.key != IPC_PRIVATE).then_some(request.key),
                owner: request.actor,
                creator_uid: request.actor.uid,
                creator_gid: request.actor.gid,
                mode: request.mode,
                maximum_bytes: self.limits.queue_bytes,
                bytes: 0,
                messages: 0,
                last_send_pid: 0,
                last_receive_pid: 0,
                created_at: request.now,
                sent_at: None,
                received_at: None,
                changed_at: request.now,
            },
            messages: VecDeque::new(),
        });
        Ok(id)
    }

    fn validate_send(message_type: i64, length: usize, flags: u32, maximum: usize) -> Result<(), MessageError> {
        if message_type <= 0 || length > maximum || flags & !MSG_NOWAIT != 0 {
            return Err(MessageError::InvalidArgument);
        }
        Ok(())
    }

    fn validate_receive(message_type: i64, flags: u32) -> Result<(), MessageError> {
        if flags & !MESSAGE_FLAGS != 0
            || flags & MSG_COPY != 0 && (flags & MSG_NOWAIT == 0 || flags & MSG_EXCEPT != 0 || message_type < 0)
            || flags & MSG_EXCEPT != 0 && message_type <= 0
        {
            return Err(MessageError::InvalidArgument);
        }
        Ok(())
    }

    fn select(queue: &Queue, message_type: i64, flags: u32) -> Option<usize> {
        if flags & MSG_COPY != 0 {
            return usize::try_from(message_type)
                .ok()
                .filter(|index| *index < queue.messages.len());
        }
        if message_type == 0 {
            return queue.messages.iter().position(|message| message.reservation.is_none());
        }
        if message_type > 0 {
            return Self::select_positive(queue, message_type, flags & MSG_EXCEPT != 0);
        }
        let maximum = message_type.checked_abs()?;
        queue
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.reservation.is_none() && message.message_type <= maximum)
            .min_by_key(|(_, message)| message.message_type)
            .map(|(index, _)| index)
    }

    fn select_positive(queue: &Queue, message_type: i64, except: bool) -> Option<usize> {
        queue
            .messages
            .iter()
            .position(|message| message.reservation.is_none() && (message.message_type == message_type) != except)
    }

    fn has_capacity(&self, state: &State, queue: &Queue, length: usize) -> bool {
        queue
            .metadata
            .bytes
            .checked_add(length)
            .is_some_and(|v| v <= queue.metadata.maximum_bytes)
            && queue.metadata.messages < self.limits.queue_messages
            && state
                .bytes
                .checked_add(length)
                .is_some_and(|v| v <= self.limits.total_bytes)
            && state.messages < self.limits.total_messages
    }

    fn keyed_get(&self, state: &mut State, request: MsgGetRequest) -> Result<MessageQueueId, MessageError> {
        let Some(id) = Self::key_id(state, request.key) else {
            if request.create {
                return self.create(state, request);
            }
            return Err(MessageError::NotFound);
        };
        if request.create && request.exclusive {
            return Err(MessageError::Exists);
        }
        let queue = Self::queue(state, id)?;
        Self::require(&queue.metadata, request.actor, (request.mode >> 6) & 0o6)?;
        Ok(id)
    }

    fn commit_send(
        state: &mut State,
        id: MessageQueueId,
        pid: u32,
        message_type: i64,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), MessageError> {
        let queue = Self::queue_mut(state, id)?;
        queue.messages.push_back(Message {
            message_type,
            bytes: bytes.to_vec(),
            reservation: None,
        });
        queue.metadata.bytes += bytes.len();
        queue.metadata.messages += 1;
        queue.metadata.last_send_pid = pid;
        queue.metadata.sent_at = Some(now);
        state.bytes += bytes.len();
        state.messages += 1;
        Ok(())
    }

    fn commit_receive(
        state: &mut State,
        id: MessageQueueId,
        index: usize,
        pid: u32,
        now: u64,
    ) -> Result<(), MessageError> {
        let queue = Self::queue_mut(state, id)?;
        let message = queue.messages.remove(index).ok_or(MessageError::NoMessage)?;
        queue.metadata.bytes -= message.bytes.len();
        queue.metadata.messages -= 1;
        queue.metadata.last_receive_pid = pid;
        queue.metadata.received_at = Some(now);
        state.bytes -= message.bytes.len();
        state.messages -= 1;
        Ok(())
    }

    fn require(metadata: &MessageQueueMetadata, actor: Credentials, requested: u16) -> Result<(), MessageError> {
        if actor.uid == 0 {
            return Ok(());
        }
        let shift = if actor.uid == metadata.owner.uid || actor.uid == metadata.creator_uid {
            6
        } else if actor.gid == metadata.owner.gid || actor.gid == metadata.creator_gid {
            3
        } else {
            0
        };
        ((metadata.mode >> shift) & requested == requested)
            .then_some(())
            .ok_or(MessageError::Permission)
    }

    pub(crate) fn key_id(state: &State, key: IpcKey) -> Option<MessageQueueId> {
        state.slots.iter().find_map(|slot| {
            let queue = slot.queue.as_ref()?;
            (queue.metadata.key == Some(key)).then_some(queue.metadata.id)
        })
    }

    fn queue(state: &State, id: MessageQueueId) -> Result<&Queue, MessageError> {
        let slot = state.slots.get(id.slot as usize).ok_or(MessageError::NotFound)?;
        if slot.generation != id.generation {
            return Err(MessageError::Removed);
        }
        slot.queue.as_ref().ok_or(MessageError::Removed)
    }

    fn queue_mut(state: &mut State, id: MessageQueueId) -> Result<&mut Queue, MessageError> {
        let slot = state.slots.get_mut(id.slot as usize).ok_or(MessageError::NotFound)?;
        if slot.generation != id.generation {
            return Err(MessageError::Removed);
        }
        slot.queue.as_mut().ok_or(MessageError::Removed)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}
