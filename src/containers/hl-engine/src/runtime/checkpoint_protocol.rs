use std::io::Write;

pub(super) const ABI: u32 = 2;
pub(super) const MAGIC_REQUEST: u32 = 0x484b_4351;
const MAGIC_REPLY: u32 = 0x484b_4353;
const NAME_MAX: usize = 512;
pub(super) const PAYLOAD_MAX: usize = 4 * 1024 * 1024;
pub(super) const REQUEST_BYTES: usize = 48;
const REPLY_BYTES: usize = 32;
pub(super) const STATUS_OK: i32 = 0;
pub(super) const STATUS_ERROR: i32 = -1;
pub(super) const STATUS_ALREADY: i32 = 1;

pub(super) const OBJECT_BEGIN: u32 = 1;
pub(super) const OBJECT_WRITE: u32 = 2;
pub(super) const OBJECT_WRITE_AT: u32 = 3;
pub(super) const OBJECT_TELL: u32 = 4;
pub(super) const OBJECT_FINISH: u32 = 5;
pub(super) const OBJECT_ABORT: u32 = 6;
pub(super) const GROUP_BEGIN: u32 = 7;
pub(super) const GROUP_COMMIT: u32 = 8;
pub(super) const GROUP_ABORT: u32 = 9;
pub(super) const CLAIM: u32 = 10;
pub(super) const UNCLAIM: u32 = 11;
pub(super) const COMMIT: u32 = 12;
pub(super) const GROUP_PRESENT: u32 = 13;
pub(super) const GROUP_COUNT: u32 = 14;
pub(super) const DIGEST: u32 = 15;
pub(super) const SOURCE_LIST: u32 = 16;
pub(super) const SOURCE_SIZE: u32 = 17;
pub(super) const SOURCE_READ: u32 = 18;
pub(super) const RECOVERY_COMPLETE: u32 = 19;

#[derive(Debug)]
pub(super) struct Request {
    pub(super) op: u32,
    pub(super) stream: u64,
    pub(super) offset: u64,
    pub(super) length: u64,
    pub(super) name_size: usize,
    pub(super) generation: u32,
}

impl Request {
    pub(super) fn decode(bytes: &[u8; REQUEST_BYTES]) -> Option<Self> {
        let word = |at| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("fixed request layout"));
        let long = |at| u64::from_ne_bytes(bytes[at..at + 8].try_into().expect("fixed request layout"));
        if word(0) != MAGIC_REQUEST || word(4) != ABI {
            return None;
        }
        let name_size = usize::try_from(word(40)).ok()?;
        let length = long(32);
        if name_size > NAME_MAX || length > PAYLOAD_MAX as u64 {
            return None;
        }
        Some(Self {
            op: word(8),
            stream: long(16),
            offset: long(24),
            length,
            name_size,
            generation: word(44),
        })
    }

    pub(super) fn carries_payload(&self) -> bool {
        self.length != 0 && self.op != SOURCE_READ
    }
}

pub(super) struct Reply {
    pub(super) status: i32,
    value: u64,
    payload: Vec<u8>,
}

impl Reply {
    pub(super) const fn status(status: i32) -> Self {
        Self {
            status,
            value: 0,
            payload: Vec::new(),
        }
    }

    pub(super) const fn ok() -> Self {
        Self::status(STATUS_OK)
    }

    pub(super) const fn error() -> Self {
        Self::status(STATUS_ERROR)
    }

    pub(super) const fn value(value: u64) -> Self {
        Self {
            status: STATUS_OK,
            value,
            payload: Vec::new(),
        }
    }

    pub(super) const fn payload(payload: Vec<u8>) -> Self {
        Self {
            status: STATUS_OK,
            value: 0,
            payload,
        }
    }

    pub(super) const fn counted_payload(value: u64, payload: Vec<u8>) -> Self {
        Self {
            status: STATUS_OK,
            value,
            payload,
        }
    }

    pub(super) fn write(&self, channel: &mut impl Write) -> std::io::Result<()> {
        let mut header = [0_u8; REPLY_BYTES];
        header[0..4].copy_from_slice(&MAGIC_REPLY.to_ne_bytes());
        header[4..8].copy_from_slice(&ABI.to_ne_bytes());
        header[8..12].copy_from_slice(&self.status.to_ne_bytes());
        header[16..24].copy_from_slice(&self.value.to_ne_bytes());
        header[24..32].copy_from_slice(&(self.payload.len() as u64).to_ne_bytes());
        channel.write_all(&header)?;
        channel.write_all(&self.payload)?;
        channel.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_capture_generation_survives_wire_decode() {
        let mut bytes = [0_u8; REQUEST_BYTES];
        bytes[0..4].copy_from_slice(&MAGIC_REQUEST.to_ne_bytes());
        bytes[4..8].copy_from_slice(&ABI.to_ne_bytes());
        bytes[8..12].copy_from_slice(&COMMIT.to_ne_bytes());
        bytes[44..48].copy_from_slice(&37_u32.to_ne_bytes());
        let request = Request::decode(&bytes).expect("ABI-2 checkpoint request");
        assert_eq!(request.generation, 37);
    }
}
