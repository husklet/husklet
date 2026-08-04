use std::sync::atomic::Ordering;

use hl_sync::{Interruption, WaitOutcome};
use hl_time::{Deadline, MonotonicClock};

use super::model::{MSG_NOWAIT, MessageError, MessageQueueId, MessageReceive};
use super::queue::MessageQueueNamespace;
use crate::{Credentials, PreparedMessageReceive};

impl MessageQueueNamespace {
    pub fn send_wait<C: MonotonicClock + ?Sized>(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        bytes: &[u8],
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
        now: u64,
    ) -> Result<(), MessageError> {
        let mut waited = false;
        loop {
            let observed = self.changed.observation();
            match self.send(id, actor, pid, message_type, bytes, 0, now) {
                Err(MessageError::Again) => {}
                Err(MessageError::NotFound) if waited => return Err(MessageError::Removed),
                result => return result,
            }
            self.wait(observed, interruption, deadline, clock)?;
            waited = true;
        }
    }

    pub fn receive_wait<C: MonotonicClock + ?Sized>(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        maximum: usize,
        flags: u32,
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
        now: u64,
    ) -> Result<MessageReceive, MessageError> {
        loop {
            let observed = self.changed.observation();
            match self.receive(id, actor, pid, message_type, maximum, flags & !MSG_NOWAIT, now) {
                Err(MessageError::NoMessage) => {}
                result => return result,
            }
            self.wait(observed, interruption, deadline, clock)?;
        }
    }

    pub fn prepare_receive_wait<C: MonotonicClock + ?Sized>(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        message_type: i64,
        maximum: usize,
        flags: u32,
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
        now: u64,
    ) -> Result<PreparedMessageReceive<'_>, MessageError> {
        let mut waited = false;
        loop {
            let observed = self.changed.observation();
            match self.prepare_receive(id, actor, pid, message_type, maximum, flags & !MSG_NOWAIT, now) {
                Err(MessageError::NoMessage) => {}
                Err(MessageError::NotFound) if waited => return Err(MessageError::Removed),
                result => return result,
            }
            self.wait(observed, interruption, deadline, clock)?;
            waited = true;
        }
    }

    fn wait<C: MonotonicClock + ?Sized>(
        &self,
        observed: u64,
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
    ) -> Result<(), MessageError> {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let outcome = self
            .changed
            .wait(observed, interruption, deadline, clock)
            .map_err(|_| MessageError::Clock);
        self.waiters.fetch_sub(1, Ordering::AcqRel);
        match outcome? {
            WaitOutcome::Notified => Ok(()),
            WaitOutcome::Interrupted => Err(MessageError::Interrupted),
            WaitOutcome::TimedOut => Err(MessageError::TimedOut),
        }
    }

    pub(crate) fn checkpoint_waiters(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn active_waiters(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }
}
