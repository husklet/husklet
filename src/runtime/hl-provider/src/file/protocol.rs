//! C provider-files operation payloads and strict reply decoding.

use crate::{FileError, FileMetadata, RemoteId, Reply, ReplyOperation};

const OPEN: u8 = 1;
const READ: u8 = 2;
const WRITE: u8 = 3;
const SEEK: u8 = 4;
const STAT: u8 = 5;
const POLL: u8 = 6;
const CLOSE: u8 = 7;

pub(crate) struct Protocol;
pub(crate) type FileProtocol = Protocol;

// Each decoder consumes the reply it decodes, so the caller cannot reuse a spent one.
#[allow(clippy::needless_pass_by_value)]
impl FileProtocol {
    pub(crate) fn open(service: u64, access: u8) -> Vec<u8> {
        let mut payload = vec![OPEN];
        Bytes::u64(&mut payload, service);
        payload.push(access);
        payload
    }

    pub(crate) fn open_reply(reply: Reply) -> Result<RemoteId, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() != 9 || reply.payload[0] != OPEN {
            return Err(FileError::MalformedReply(ReplyOperation::Open));
        }
        RemoteId::new(Bytes::read_u64(&reply.payload, 1, ReplyOperation::Open)?)
            .ok_or(FileError::MalformedReply(ReplyOperation::Open))
    }

    pub(crate) fn read(remote: RemoteId, offset: u64, size: usize) -> Vec<u8> {
        let mut payload = vec![READ];
        Bytes::u64(&mut payload, remote.get());
        Bytes::u64(&mut payload, offset);
        Bytes::u32(&mut payload, size as u32);
        payload
    }

    pub(crate) fn read_reply(reply: Reply, maximum: usize) -> Result<Vec<u8>, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() < 5 || reply.payload[0] != READ {
            return Err(FileError::MalformedReply(ReplyOperation::Read));
        }
        let size = Bytes::read_u32(&reply.payload, 1, ReplyOperation::Read)? as usize;
        if size > maximum || reply.payload.len() != 5 + size {
            return Err(FileError::MalformedReply(ReplyOperation::Read));
        }
        Ok(reply.payload[5..].to_vec())
    }

    pub(crate) fn write(remote: RemoteId, offset: u64, bytes: &[u8]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(21 + bytes.len());
        payload.push(WRITE);
        Bytes::u64(&mut payload, remote.get());
        Bytes::u64(&mut payload, offset);
        Bytes::u32(&mut payload, bytes.len() as u32);
        payload.extend_from_slice(bytes);
        payload
    }

    pub(crate) fn write_reply(reply: Reply, maximum: usize) -> Result<usize, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() != 5 || reply.payload[0] != WRITE {
            return Err(FileError::MalformedReply(ReplyOperation::Write));
        }
        let count = Bytes::read_u32(&reply.payload, 1, ReplyOperation::Write)? as usize;
        if count > maximum {
            return Err(FileError::MalformedReply(ReplyOperation::Write));
        }
        Ok(count)
    }

    pub(crate) fn seek(remote: RemoteId, offset: i64, whence: u8) -> Vec<u8> {
        let mut payload = vec![SEEK];
        Bytes::u64(&mut payload, remote.get());
        Bytes::u64(&mut payload, offset as u64);
        payload.push(whence);
        payload
    }

    pub(crate) fn seek_reply(reply: Reply) -> Result<u64, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() != 9 || reply.payload[0] != SEEK {
            return Err(FileError::MalformedReply(ReplyOperation::Seek));
        }
        Bytes::read_u64(&reply.payload, 1, ReplyOperation::Seek)
    }

    pub(crate) fn stat(remote: RemoteId) -> Vec<u8> {
        let mut payload = vec![STAT];
        Bytes::u64(&mut payload, remote.get());
        payload
    }

    pub(crate) fn stat_reply(reply: Reply, remote: RemoteId) -> Result<FileMetadata, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() != 21 || reply.payload[0] != STAT {
            return Err(FileError::MalformedReply(ReplyOperation::Stat));
        }
        Ok(FileMetadata {
            permissions: Bytes::read_u32(&reply.payload, 1, ReplyOperation::Stat)?,
            user: Bytes::read_u32(&reply.payload, 5, ReplyOperation::Stat)?,
            group: Bytes::read_u32(&reply.payload, 9, ReplyOperation::Stat)?,
            size: Bytes::read_u64(&reply.payload, 13, ReplyOperation::Stat)?,
            stable_object: remote.get(),
        })
    }

    pub(crate) fn poll(remote: RemoteId, interests: u8) -> Vec<u8> {
        let mut payload = vec![POLL];
        Bytes::u64(&mut payload, remote.get());
        payload.push(interests);
        payload
    }

    pub(crate) fn poll_reply(reply: Reply) -> Result<u8, FileError> {
        Self::success(&reply)?;
        if reply.payload.len() != 2 || reply.payload[0] != POLL {
            return Err(FileError::MalformedReply(ReplyOperation::Poll));
        }
        Ok(reply.payload[1])
    }

    pub(crate) fn close(remote: RemoteId) -> Vec<u8> {
        let mut payload = vec![CLOSE];
        Bytes::u64(&mut payload, remote.get());
        payload
    }

    pub(crate) fn close_reply(reply: Reply) -> Result<(), FileError> {
        Self::success(&reply)?;
        if reply.payload != [CLOSE] {
            return Err(FileError::MalformedReply(ReplyOperation::Close));
        }
        Ok(())
    }

    fn success(reply: &Reply) -> Result<(), FileError> {
        if reply.linux_errno != 0 {
            return Err(FileError::Linux(reply.linux_errno));
        }
        Ok(())
    }
}

struct Bytes;

impl Bytes {
    fn u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn read_u32(input: &[u8], offset: usize, operation: ReplyOperation) -> Result<u32, FileError> {
        let bytes = input
            .get(offset..offset + 4)
            .ok_or(FileError::MalformedReply(operation))?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64(input: &[u8], offset: usize, operation: ReplyOperation) -> Result<u64, FileError> {
        let bytes = input
            .get(offset..offset + 8)
            .ok_or(FileError::MalformedReply(operation))?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }
}
