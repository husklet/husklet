use hl_network::AuthoritySocketKey;

use crate::engine::EngineError;

const MAGIC: u32 = 0x574e_4c48;
const VERSION: u16 = 1;
const BYTES: usize = 86;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum Operation {
    CaptureBegin = 1,
    RetainListener = 2,
    CapturePublish = 3,
    CaptureAbort = 4,
    CaptureFinish = 5,
    RestoreBegin = 6,
    RestoreStage = 7,
    RestoreCommit = 8,
    RestoreAbort = 9,
    RestoreResume = 10,
    Release = 11,
}

impl Operation {
    fn from_raw(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::CaptureBegin,
            2 => Self::RetainListener,
            3 => Self::CapturePublish,
            4 => Self::CaptureAbort,
            5 => Self::CaptureFinish,
            6 => Self::RestoreBegin,
            7 => Self::RestoreStage,
            8 => Self::RestoreCommit,
            9 => Self::RestoreAbort,
            10 => Self::RestoreResume,
            11 => Self::Release,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Message {
    pub operation: Operation,
    pub status: u8,
    pub transaction: u64,
    pub digest: [u8; 32],
    pub slot: u32,
    pub generation: u64,
    pub resource: u64,
    pub nonce: [u8; 16],
    pub count: u16,
}

impl Message {
    pub(super) fn request(operation: Operation) -> Self {
        Self {
            operation,
            status: 0,
            transaction: 0,
            digest: [0; 32],
            slot: 0,
            generation: 0,
            resource: 0,
            nonce: [0; 16],
            count: 0,
        }
    }

    pub(super) fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(BYTES);
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.push(self.operation as u8);
        bytes.push(self.status);
        bytes.extend_from_slice(&self.transaction.to_le_bytes());
        bytes.extend_from_slice(&self.digest);
        bytes.extend_from_slice(&self.slot.to_le_bytes());
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.extend_from_slice(&self.resource.to_le_bytes());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.count.to_le_bytes());
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        if bytes.len() != BYTES
            || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC
            || u16::from_le_bytes(bytes[4..6].try_into().unwrap()) != VERSION
        {
            return Err(EngineError::AuthorityFailed);
        }
        Ok(Self {
            operation: Operation::from_raw(bytes[6]).ok_or(EngineError::AuthorityFailed)?,
            status: bytes[7],
            transaction: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            digest: bytes[16..48].try_into().unwrap(),
            slot: u32::from_le_bytes(bytes[48..52].try_into().unwrap()),
            generation: u64::from_le_bytes(bytes[52..60].try_into().unwrap()),
            resource: u64::from_le_bytes(bytes[60..68].try_into().unwrap()),
            nonce: bytes[68..84].try_into().unwrap(),
            count: u16::from_le_bytes(bytes[84..86].try_into().unwrap()),
        })
    }

    pub(super) fn reply(self, status: u8) -> Self {
        Self { status, ..self }
    }

    pub(super) fn key(self) -> Result<AuthoritySocketKey, EngineError> {
        AuthoritySocketKey::new(self.slot, self.generation).ok_or(EngineError::AuthorityFailed)
    }
}
