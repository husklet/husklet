use std::fs::File;
use std::os::fd::AsRawFd;

use hl_network::AuthoritySocketKey;
use hl_session::FrameKind;

use super::{Message, NetworkAuthority, Operation};
use crate::engine::EngineError;
use crate::native::AuthorityWorker;

pub struct Client<'a>(&'a mut AuthorityWorker);

impl AuthorityWorker {
    pub fn network(&mut self) -> Client<'_> {
        Client(self)
    }
}

impl Client<'_> {
    fn nonce(&mut self) -> Result<[u8; 16], EngineError> {
        let value = self.0.network_nonce;
        self.0.network_nonce = value.checked_add(1).ok_or(EngineError::AuthorityFailed)?;
        let mut nonce = [0; 16];
        nonce[..8].copy_from_slice(&value.to_le_bytes());
        nonce[8..].copy_from_slice(&(!value).to_le_bytes());
        Ok(nonce)
    }

    fn send(&mut self, request: Message) -> Result<Message, EngineError> {
        self.0
            .session
            .send(&mut self.0.stream, FrameKind::Network, &request.encode())
            .map_err(|_| EngineError::AuthorityFailed)?;
        self.reply(request)
    }

    fn reply(&mut self, request: Message) -> Result<Message, EngineError> {
        let frame = self
            .0
            .session
            .receive(&mut self.0.stream)
            .map_err(|_| EngineError::AuthorityFailed)?;
        if frame.kind != FrameKind::Network {
            return Err(EngineError::AuthorityFailed);
        }
        let reply = Message::decode(&frame.payload)?;
        if reply.operation != request.operation || reply.status != 0 {
            return Err(EngineError::AuthorityFailed);
        }
        Ok(reply)
    }

    pub fn capture_prepare(&mut self) -> Result<(), EngineError> {
        if self.0.network_capture.is_some() {
            return Err(EngineError::AuthorityFailed);
        }
        let reply = self.send(Message::request(Operation::CaptureBegin))?;
        if reply.transaction == 0 {
            return Err(EngineError::AuthorityFailed);
        }
        self.0.network_capture = Some(reply.transaction);
        Ok(())
    }

    pub fn retain_listener(&mut self, descriptor: i32, resource: u64) -> Result<AuthoritySocketKey, EngineError> {
        let transaction = self.0.network_capture.ok_or(EngineError::AuthorityFailed)?;
        let nonce = self.nonce()?;
        let mut request = Message::request(Operation::RetainListener);
        request.transaction = transaction;
        request.resource = resource;
        request.nonce = nonce;
        self.0
            .session
            .send(&mut self.0.stream, FrameKind::Network, &request.encode())
            .map_err(|_| EngineError::AuthorityFailed)?;
        let transfer = self.0.transfer.as_ref().ok_or(EngineError::AuthorityFailed)?;
        let count = crate::ffi::linux::transfer::send(transfer.as_raw_fd(), &nonce, &[descriptor])
            .map_err(|_| EngineError::AuthorityFailed)?;
        if count != nonce.len() {
            return Err(EngineError::AuthorityFailed);
        }
        self.reply(request)?.key()
    }

    pub fn capture_publish(&mut self, digest: [u8; 32]) -> Result<(), EngineError> {
        let mut request = Message::request(Operation::CapturePublish);
        request.transaction = self.0.network_capture.ok_or(EngineError::AuthorityFailed)?;
        request.digest = digest;
        self.send(request).map(|_| ())
    }

    pub fn capture_abort(&mut self) -> Result<(), EngineError> {
        let Some(transaction) = self.0.network_capture.take() else {
            return Ok(());
        };
        let mut request = Message::request(Operation::CaptureAbort);
        request.transaction = transaction;
        self.send(request).map(|_| ())
    }

    pub fn capture_finish(&mut self) -> Result<(), EngineError> {
        let transaction = self.0.network_capture.take().ok_or(EngineError::AuthorityFailed)?;
        let mut request = Message::request(Operation::CaptureFinish);
        request.transaction = transaction;
        self.send(request).map(|_| ())
    }

    pub fn restore_begin(&mut self, digest: [u8; 32], count: usize) -> Result<(), EngineError> {
        if self.0.network_restore.is_some() {
            return Err(EngineError::AuthorityFailed);
        }
        let mut request = Message::request(Operation::RestoreBegin);
        request.digest = digest;
        request.count = u16::try_from(count).map_err(|_| EngineError::AuthorityFailed)?;
        let reply = self.send(request)?;
        if reply.transaction == 0 {
            return Err(EngineError::AuthorityFailed);
        }
        self.0.network_restore = Some((reply.transaction, digest));
        Ok(())
    }

    pub fn restore_stage(&mut self, key: AuthoritySocketKey) -> Result<File, EngineError> {
        let (transaction, digest) = self.0.network_restore.ok_or(EngineError::AuthorityFailed)?;
        let nonce = self.nonce()?;
        let mut request = Message::request(Operation::RestoreStage);
        request.transaction = transaction;
        request.digest = digest;
        request.slot = key.slot();
        request.generation = key.generation();
        request.nonce = nonce;
        self.send(request)?;
        let transfer = self.0.transfer.as_ref().ok_or(EngineError::AuthorityFailed)?;
        let mut received_nonce = [0; 16];
        let (count, rights, _) = crate::ffi::linux::transfer::receive(transfer.as_raw_fd(), &mut received_nonce, 1)
            .map_err(|_| EngineError::AuthorityFailed)?;
        if count != nonce.len() || received_nonce != nonce || rights.len() != 1 {
            NetworkAuthority::discard(rights)?;
            return Err(EngineError::AuthorityFailed);
        }
        crate::ffi::linux::InheritedFile::adopt(rights[0]).map_err(|_| EngineError::AuthorityFailed)
    }

    fn restore_transition(&mut self, operation: Operation) -> Result<(), EngineError> {
        let (transaction, digest) = self.0.network_restore.ok_or(EngineError::AuthorityFailed)?;
        let mut request = Message::request(operation);
        request.transaction = transaction;
        request.digest = digest;
        self.send(request)?;
        if matches!(operation, Operation::RestoreAbort | Operation::RestoreResume) {
            self.0.network_restore = None;
        }
        Ok(())
    }

    pub fn restore_commit(&mut self) -> Result<(), EngineError> {
        self.restore_transition(Operation::RestoreCommit)
    }

    pub fn restore_abort(&mut self) -> Result<(), EngineError> {
        self.restore_transition(Operation::RestoreAbort)
    }

    pub fn restore_resume(&mut self) -> Result<(), EngineError> {
        self.restore_transition(Operation::RestoreResume)
    }
}
