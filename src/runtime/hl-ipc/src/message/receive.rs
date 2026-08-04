use crate::{MessageError, MessageQueueId, MessageQueueNamespace, MessageReceive};

pub struct PreparedMessageReceive<'a> {
    pub(crate) namespace: &'a MessageQueueNamespace,
    pub(crate) id: MessageQueueId,
    pub(crate) reservation: Option<u64>,
    pub(crate) result: MessageReceive,
    pub(crate) pid: u32,
    pub(crate) now: u64,
    pub(crate) committed: bool,
}

impl PreparedMessageReceive<'_> {
    #[must_use]
    pub fn message(&self) -> &MessageReceive {
        &self.result
    }

    pub fn commit(mut self) -> Result<MessageReceive, MessageError> {
        if let Some(reservation) = self.reservation {
            self.namespace
                .commit_reservation(self.id, reservation, self.pid, self.now)?;
        }
        self.committed = true;
        Ok(self.result.clone())
    }
}

impl Drop for PreparedMessageReceive<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(reservation) = self.reservation {
            self.namespace.abort_reservation(self.id, reservation);
        }
    }
}
