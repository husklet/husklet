use std::collections::BTreeSet;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;

use crate::engine::EngineError;
use hl_network::AuthoritySocketKey;

mod client;
mod wire;

pub use client::Client;
use wire::{Message, Operation};

const LEASE_MAXIMUM: usize = 4096;

struct Lease {
    generation: u64,
    descriptor: Option<File>,
    digest: Option<[u8; 32]>,
}

struct Capture {
    transaction: u64,
    slots: Vec<usize>,
}

struct Restoration {
    transaction: u64,
    digest: [u8; 32],
    expected: usize,
    staged: BTreeSet<AuthoritySocketKey>,
    committed: bool,
}

pub(super) struct NetworkAuthority {
    transfer: UnixDatagram,
    leases: Vec<Lease>,
    capture: Option<Capture>,
    restore: Option<Restoration>,
    next_transaction: u64,
}

impl NetworkAuthority {
    pub(super) fn new(transfer: UnixDatagram) -> Self {
        Self {
            transfer,
            leases: Vec::new(),
            capture: None,
            restore: None,
            next_transaction: 1,
        }
    }

    fn transaction(&mut self) -> Result<u64, EngineError> {
        let value = self.next_transaction;
        self.next_transaction = value.checked_add(1).ok_or(EngineError::AuthorityFailed)?;
        Ok(value)
    }

    pub(super) fn dispatch(&mut self, bytes: &[u8]) -> Result<Vec<u8>, EngineError> {
        let request = Message::decode(bytes)?;
        let reply = match request.operation {
            Operation::CaptureBegin => self.capture_begin(request),
            Operation::RetainListener => self.retain(request),
            Operation::CapturePublish => self.capture_publish(request),
            Operation::CaptureAbort => self.capture_abort(request),
            Operation::CaptureFinish => self.capture_finish(request),
            Operation::RestoreBegin => self.restore_begin(request),
            Operation::RestoreStage => self.restore_stage(request),
            Operation::RestoreCommit => self.restore_commit(request),
            Operation::RestoreAbort => self.restore_abort(request),
            Operation::RestoreResume => self.restore_resume(request),
            Operation::Release => self.release(request),
        };
        Ok(reply.unwrap_or_else(|_| request.reply(1)).encode())
    }

    fn capture_begin(&mut self, request: Message) -> Result<Message, EngineError> {
        if self.capture.is_some() || self.restore.is_some() {
            return Err(EngineError::AuthorityFailed);
        }
        let transaction = self.transaction()?;
        self.capture = Some(Capture {
            transaction,
            slots: Vec::new(),
        });
        Ok(Message { transaction, ..request }.reply(0))
    }

    fn retain(&mut self, request: Message) -> Result<Message, EngineError> {
        let capture = self.capture.as_ref().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != capture.transaction || request.resource == 0 || request.nonce == [0; 16] {
            return Err(EngineError::AuthorityFailed);
        }
        let mut nonce = [0; 16];
        let (count, rights, _) = crate::ffi::linux::transfer::receive(self.transfer.as_raw_fd(), &mut nonce, 1)
            .map_err(|_| EngineError::AuthorityFailed)?;
        if count != nonce.len() || nonce != request.nonce || rights.len() != 1 {
            Self::discard(rights)?;
            return Err(EngineError::AuthorityFailed);
        }
        let descriptor =
            crate::ffi::linux::InheritedListener::adopt(rights[0]).map_err(|_| EngineError::AuthorityFailed)?;
        let (slot, generation) = self.allocate(descriptor)?;
        self.capture
            .as_mut()
            .ok_or(EngineError::AuthorityFailed)?
            .slots
            .push(slot);
        Ok(Message {
            slot: u32::try_from(slot + 1).map_err(|_| EngineError::AuthorityFailed)?,
            generation,
            ..request
        }
        .reply(0))
    }

    fn discard(rights: Vec<i32>) -> Result<(), EngineError> {
        for descriptor in rights {
            let descriptor =
                crate::ffi::linux::InheritedFile::adopt(descriptor).map_err(|_| EngineError::AuthorityFailed)?;
            drop(descriptor);
        }
        Ok(())
    }

    fn allocate(&mut self, descriptor: File) -> Result<(usize, u64), EngineError> {
        if let Some((slot, lease)) = self
            .leases
            .iter_mut()
            .enumerate()
            .find(|(_, lease)| lease.descriptor.is_none())
        {
            lease.generation = lease.generation.checked_add(1).ok_or(EngineError::AuthorityFailed)?;
            lease.descriptor = Some(descriptor);
            lease.digest = None;
            return Ok((slot, lease.generation));
        }
        if self.leases.len() >= LEASE_MAXIMUM {
            return Err(EngineError::AuthorityFailed);
        }
        let slot = self.leases.len();
        self.leases.push(Lease {
            generation: 1,
            descriptor: Some(descriptor),
            digest: None,
        });
        Ok((slot, 1))
    }

    fn capture_publish(&mut self, request: Message) -> Result<Message, EngineError> {
        if request.digest == [0; 32] {
            return Err(EngineError::AuthorityFailed);
        }
        let capture = self.capture.as_ref().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != capture.transaction {
            return Err(EngineError::AuthorityFailed);
        }
        for slot in &capture.slots {
            self.leases.get_mut(*slot).ok_or(EngineError::AuthorityFailed)?.digest = Some(request.digest);
        }
        Ok(request.reply(0))
    }

    fn capture_abort(&mut self, request: Message) -> Result<Message, EngineError> {
        let capture = self.capture.take().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != capture.transaction {
            self.capture = Some(capture);
            return Err(EngineError::AuthorityFailed);
        }
        for slot in capture.slots {
            if let Some(lease) = self.leases.get_mut(slot) {
                lease.descriptor = None;
                lease.digest = None;
            }
        }
        Ok(request.reply(0))
    }

    fn capture_finish(&mut self, request: Message) -> Result<Message, EngineError> {
        let capture = self.capture.take().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != capture.transaction {
            self.capture = Some(capture);
            return Err(EngineError::AuthorityFailed);
        }
        if !self.capture_published(&capture) {
            self.capture = Some(capture);
            return Err(EngineError::AuthorityFailed);
        }
        Ok(request.reply(0))
    }

    fn capture_published(&self, capture: &Capture) -> bool {
        for slot in &capture.slots {
            let Some(lease) = self.leases.get(*slot) else {
                return false;
            };
            if lease.digest.is_none() {
                return false;
            }
        }
        true
    }

    fn restore_begin(&mut self, request: Message) -> Result<Message, EngineError> {
        if self.capture.is_some()
            || self.restore.is_some()
            || request.digest == [0; 32]
            || usize::from(request.count) > LEASE_MAXIMUM
        {
            return Err(EngineError::AuthorityFailed);
        }
        let transaction = self.transaction()?;
        self.restore = Some(Restoration {
            transaction,
            digest: request.digest,
            expected: usize::from(request.count),
            staged: BTreeSet::new(),
            committed: false,
        });
        Ok(Message { transaction, ..request }.reply(0))
    }

    fn restore_stage(&mut self, request: Message) -> Result<Message, EngineError> {
        let key = request.key()?;
        let restore = self.restore.as_mut().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != restore.transaction
            || request.digest != restore.digest
            || request.nonce == [0; 16]
            || restore.committed
            || !restore.staged.insert(key)
            || restore.staged.len() > restore.expected
        {
            return Err(EngineError::AuthorityFailed);
        }
        let lease = self
            .leases
            .get(usize::try_from(key.slot() - 1).map_err(|_| EngineError::AuthorityFailed)?)
            .ok_or(EngineError::AuthorityFailed)?;
        if lease.generation != key.generation() || lease.digest != Some(restore.digest) {
            restore.staged.remove(&key);
            return Err(EngineError::AuthorityFailed);
        }
        let descriptor = lease
            .descriptor
            .as_ref()
            .ok_or(EngineError::AuthorityFailed)?
            .as_raw_fd();
        let count = crate::ffi::linux::transfer::send(self.transfer.as_raw_fd(), &request.nonce, &[descriptor])
            .map_err(|_| EngineError::AuthorityFailed)?;
        if count != request.nonce.len() {
            return Err(EngineError::AuthorityFailed);
        }
        Ok(request.reply(0))
    }

    fn restore_commit(&mut self, request: Message) -> Result<Message, EngineError> {
        let restore = self.restore.as_mut().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != restore.transaction
            || request.digest != restore.digest
            || restore.staged.len() != restore.expected
            || restore.committed
        {
            return Err(EngineError::AuthorityFailed);
        }
        restore.committed = true;
        Ok(request.reply(0))
    }

    fn restore_abort(&mut self, request: Message) -> Result<Message, EngineError> {
        let restore = self.restore.take().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != restore.transaction || request.digest != restore.digest {
            self.restore = Some(restore);
            return Err(EngineError::AuthorityFailed);
        }
        Ok(request.reply(0))
    }

    fn restore_resume(&mut self, request: Message) -> Result<Message, EngineError> {
        let restore = self.restore.take().ok_or(EngineError::AuthorityFailed)?;
        if request.transaction != restore.transaction || request.digest != restore.digest || !restore.committed {
            self.restore = Some(restore);
            return Err(EngineError::AuthorityFailed);
        }
        Ok(request.reply(0))
    }

    fn release(&mut self, request: Message) -> Result<Message, EngineError> {
        if self.capture.is_some() || self.restore.is_some() {
            return Err(EngineError::AuthorityFailed);
        }
        let key = request.key()?;
        let lease = self
            .leases
            .get_mut(usize::try_from(key.slot() - 1).map_err(|_| EngineError::AuthorityFailed)?)
            .ok_or(EngineError::AuthorityFailed)?;
        if lease.generation != key.generation() || lease.digest != Some(request.digest) {
            return Err(EngineError::AuthorityFailed);
        }
        lease.descriptor = None;
        lease.digest = None;
        Ok(request.reply(0))
    }
}
