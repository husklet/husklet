//! Fixed, bounded control wire between the Rust product frontend and one C engine worker.
//!
//! Every message occupies exactly one frame. Keeping framing outside the payload makes a
//! truncated stream, an oversized datagram, and an ABI mismatch fail closed before any command
//! reaches the retained engine.

use crate::activation::GuestIsa;
use crate::engine::{EngineExit, ExitKind, FaultAccess, FaultDiagnostic, FaultReason, StopRequest};

const MAGIC: u32 = u32::from_le_bytes(*b"HLCW");
const VERSION: u16 = 1;
const HEADER_SIZE: usize = 16;
const PAYLOAD_SIZE: usize = 64;
pub(crate) const FRAME_SIZE: usize = HEADER_SIZE + PAYLOAD_SIZE;

const READY: u16 = 1;
const START: u16 = 2;
const STOP: u16 = 3;
const RESIZE: u16 = 4;
const EXIT: u16 = 5;
const ERROR: u16 = 6;
const STARTED: u16 = 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureStage {
    Decode,
    Create,
    Start,
    Control,
    Destroy,
}

impl FailureStage {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Create => "create",
            Self::Start => "start",
            Self::Control => "control",
            Self::Destroy => "destroy",
        }
    }

    const fn wire_value(self) -> u8 {
        match self {
            Self::Decode => 1,
            Self::Create => 2,
            Self::Start => 3,
            Self::Control => 4,
            Self::Destroy => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Message {
    Ready,
    Start,
    Started,
    Stop(StopRequest),
    Resize { rows: u16, columns: u16 },
    Exit(EngineExit),
    Error { stage: FailureStage, code: i32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WireError {
    Size,
    Magic,
    Version,
    Kind,
    Length,
    Reserved,
    Value,
}

impl Message {
    pub(crate) fn encode(self) -> Result<[u8; FRAME_SIZE], WireError> {
        let mut frame = [0_u8; FRAME_SIZE];
        put_u32(&mut frame, 0, MAGIC);
        put_u16(&mut frame, 4, VERSION);
        let (kind, length) = match self {
            Self::Ready => (READY, 0),
            Self::Start => (START, 0),
            Self::Started => (STARTED, 0),
            Self::Stop(request) => {
                let (tag, signal) = match request {
                    StopRequest::Interrupt => (1, 2),
                    StopRequest::Force => (2, 9),
                    StopRequest::Signal(signal) if (1..=64).contains(&signal) => (3, signal),
                    StopRequest::Signal(_) => return Err(WireError::Value),
                };
                frame[HEADER_SIZE] = tag;
                put_i32(&mut frame, HEADER_SIZE + 4, signal);
                (STOP, 8)
            }
            Self::Resize { rows, columns } => {
                if rows == 0 || columns == 0 {
                    return Err(WireError::Value);
                }
                put_u16(&mut frame, HEADER_SIZE, rows);
                put_u16(&mut frame, HEADER_SIZE + 2, columns);
                (RESIZE, 4)
            }
            Self::Exit(exit) => {
                encode_exit(&mut frame[HEADER_SIZE..], exit)?;
                (EXIT, PAYLOAD_SIZE)
            }
            Self::Error { stage, code } => {
                frame[HEADER_SIZE] = stage.wire_value();
                put_i32(&mut frame, HEADER_SIZE + 4, code);
                (ERROR, 8)
            }
        };
        put_u16(&mut frame, 6, kind);
        put_u32(&mut frame, 8, length as u32);
        Ok(frame)
    }

    pub(crate) fn decode(frame: &[u8]) -> Result<Self, WireError> {
        if frame.len() != FRAME_SIZE {
            return Err(WireError::Size);
        }
        if get_u32(frame, 0) != MAGIC {
            return Err(WireError::Magic);
        }
        if get_u16(frame, 4) != VERSION {
            return Err(WireError::Version);
        }
        if frame[12..HEADER_SIZE].iter().any(|byte| *byte != 0) {
            return Err(WireError::Reserved);
        }
        let kind = get_u16(frame, 6);
        let length = usize::try_from(get_u32(frame, 8)).map_err(|_| WireError::Length)?;
        if length > PAYLOAD_SIZE || frame[HEADER_SIZE + length..].iter().any(|byte| *byte != 0) {
            return Err(if length > PAYLOAD_SIZE {
                WireError::Length
            } else {
                WireError::Reserved
            });
        }
        let payload = &frame[HEADER_SIZE..HEADER_SIZE + length];
        match kind {
            READY if length == 0 => Ok(Self::Ready),
            START if length == 0 => Ok(Self::Start),
            STARTED if length == 0 => Ok(Self::Started),
            STOP if length == 8 => decode_stop(payload).map(Self::Stop),
            RESIZE if length == 4 => {
                let rows = get_u16(payload, 0);
                let columns = get_u16(payload, 2);
                if rows == 0 || columns == 0 {
                    Err(WireError::Value)
                } else {
                    Ok(Self::Resize { rows, columns })
                }
            }
            EXIT if length == PAYLOAD_SIZE => decode_exit(payload).map(Self::Exit),
            ERROR if length == 8 => {
                if payload[1..4].iter().any(|byte| *byte != 0) {
                    return Err(WireError::Reserved);
                }
                Ok(Self::Error {
                    stage: decode_stage(payload[0])?,
                    code: get_i32(payload, 4),
                })
            }
            READY | START | STOP | RESIZE | EXIT | ERROR | STARTED => Err(WireError::Length),
            _ => Err(WireError::Kind),
        }
    }
}

fn decode_stop(payload: &[u8]) -> Result<StopRequest, WireError> {
    if payload[1..4].iter().any(|byte| *byte != 0) {
        return Err(WireError::Reserved);
    }
    let signal = get_i32(payload, 4);
    match (payload[0], signal) {
        (1, 2) => Ok(StopRequest::Interrupt),
        (2, 9) => Ok(StopRequest::Force),
        (3, signal) if (1..=64).contains(&signal) => Ok(StopRequest::Signal(signal)),
        _ => Err(WireError::Value),
    }
}

fn encode_exit(payload: &mut [u8], exit: EngineExit) -> Result<(), WireError> {
    payload[0] = encode_exit_kind(exit.kind);
    put_i32(payload, 4, exit.guest_status);
    put_u64(payload, 8, exit.detail);
    let Some(fault) = exit.fault else { return Ok(()) };
    if fault.opcode_len > 15
        || fault.opcode[usize::from(fault.opcode_len)..]
            .iter()
            .any(|byte| *byte != 0)
        || fault.address.is_some() != fault.access.is_some()
    {
        return Err(WireError::Value);
    }
    payload[1] = 1;
    payload[16] = encode_isa(fault.isa);
    payload[17] = encode_reason(fault.reason);
    payload[18] = fault.opcode_len;
    payload[19] = fault.access.map_or(0, encode_access);
    payload[20] = u8::from(fault.address.is_some());
    put_u64(payload, 24, fault.pc);
    payload[32..47].copy_from_slice(&fault.opcode);
    put_u64(payload, 48, fault.address.unwrap_or(0));
    Ok(())
}

fn decode_exit(payload: &[u8]) -> Result<EngineExit, WireError> {
    if payload[2..4].iter().any(|byte| *byte != 0) || payload[56..].iter().any(|byte| *byte != 0) {
        return Err(WireError::Reserved);
    }
    let fault = match payload[1] {
        0 => {
            if payload[16..56].iter().any(|byte| *byte != 0) {
                return Err(WireError::Reserved);
            }
            None
        }
        1 => {
            if payload[21..24].iter().any(|byte| *byte != 0) || payload[47] != 0 {
                return Err(WireError::Reserved);
            }
            let opcode_len = payload[18];
            if opcode_len > 15 || payload[32 + usize::from(opcode_len)..47].iter().any(|byte| *byte != 0) {
                return Err(WireError::Value);
            }
            let address = match payload[20] {
                0 if get_u64(payload, 48) == 0 => None,
                1 => Some(get_u64(payload, 48)),
                _ => return Err(WireError::Value),
            };
            let access = decode_access(payload[19])?;
            if address.is_some() != access.is_some() {
                return Err(WireError::Value);
            }
            Some(FaultDiagnostic {
                isa: decode_isa(payload[16])?,
                pc: get_u64(payload, 24),
                opcode: payload[32..47].try_into().map_err(|_| WireError::Length)?,
                opcode_len,
                reason: decode_reason(payload[17])?,
                address,
                access,
            })
        }
        _ => return Err(WireError::Value),
    };
    Ok(EngineExit {
        kind: decode_exit_kind(payload[0])?,
        guest_status: get_i32(payload, 4),
        detail: get_u64(payload, 8),
        fault,
    })
}

fn decode_stage(value: u8) -> Result<FailureStage, WireError> {
    match value {
        1 => Ok(FailureStage::Decode),
        2 => Ok(FailureStage::Create),
        3 => Ok(FailureStage::Start),
        4 => Ok(FailureStage::Control),
        5 => Ok(FailureStage::Destroy),
        _ => Err(WireError::Value),
    }
}

fn encode_exit_kind(value: ExitKind) -> u8 {
    match value {
        ExitKind::Code => 1,
        ExitKind::Signal => 2,
        ExitKind::Fault => 3,
        ExitKind::EngineError => 4,
    }
}

fn decode_exit_kind(value: u8) -> Result<ExitKind, WireError> {
    match value {
        1 => Ok(ExitKind::Code),
        2 => Ok(ExitKind::Signal),
        3 => Ok(ExitKind::Fault),
        4 => Ok(ExitKind::EngineError),
        _ => Err(WireError::Value),
    }
}

fn encode_isa(value: GuestIsa) -> u8 {
    match value {
        GuestIsa::Aarch64 => 1,
        GuestIsa::X86_64 => 2,
    }
}

fn decode_isa(value: u8) -> Result<GuestIsa, WireError> {
    match value {
        1 => Ok(GuestIsa::Aarch64),
        2 => Ok(GuestIsa::X86_64),
        _ => Err(WireError::Value),
    }
}

fn encode_reason(value: FaultReason) -> u8 {
    match value {
        FaultReason::Fetch => 1,
        FaultReason::Memory => 2,
        FaultReason::Decode => 3,
        FaultReason::Unsupported => 4,
        FaultReason::Frozen => 5,
        FaultReason::CacheEpoch => 6,
        FaultReason::Protocol => 7,
        FaultReason::NativeFatal => 8,
    }
}

fn decode_reason(value: u8) -> Result<FaultReason, WireError> {
    match value {
        1 => Ok(FaultReason::Fetch),
        2 => Ok(FaultReason::Memory),
        3 => Ok(FaultReason::Decode),
        4 => Ok(FaultReason::Unsupported),
        5 => Ok(FaultReason::Frozen),
        6 => Ok(FaultReason::CacheEpoch),
        7 => Ok(FaultReason::Protocol),
        8 => Ok(FaultReason::NativeFatal),
        _ => Err(WireError::Value),
    }
}

fn encode_access(value: FaultAccess) -> u8 {
    match value {
        FaultAccess::Read => 1,
        FaultAccess::Write => 2,
        FaultAccess::Execute => 3,
    }
}

fn decode_access(value: u8) -> Result<Option<FaultAccess>, WireError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(FaultAccess::Read)),
        2 => Ok(Some(FaultAccess::Write)),
        3 => Ok(Some(FaultAccess::Execute)),
        _ => Err(WireError::Value),
    }
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("fixed wire field"))
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed wire field"))
}
fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn get_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed wire field"))
}
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed wire field"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fault() -> EngineExit {
        EngineExit {
            kind: ExitKind::Fault,
            guest_status: -7,
            detail: 0x8877_6655_4433_2211,
            fault: Some(FaultDiagnostic {
                isa: GuestIsa::Aarch64,
                pc: 0x1020_3040_5060_7080,
                opcode: [1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                opcode_len: 5,
                reason: FaultReason::Memory,
                address: Some(u64::MAX),
                access: Some(FaultAccess::Write),
            }),
        }
    }

    #[test]
    fn every_message_round_trips() {
        let messages = [
            Message::Ready,
            Message::Start,
            Message::Started,
            Message::Stop(StopRequest::Interrupt),
            Message::Stop(StopRequest::Force),
            Message::Stop(StopRequest::Signal(1)),
            Message::Stop(StopRequest::Signal(64)),
            Message::Resize {
                rows: 1,
                columns: u16::MAX,
            },
            Message::Exit(EngineExit {
                kind: ExitKind::Code,
                guest_status: i32::MIN,
                detail: u64::MAX,
                fault: None,
            }),
            Message::Exit(fault()),
            Message::Error {
                stage: FailureStage::Decode,
                code: i32::MIN,
            },
            Message::Error {
                stage: FailureStage::Destroy,
                code: i32::MAX,
            },
        ];
        for message in messages {
            assert_eq!(Message::decode(&message.encode().unwrap()), Ok(message));
        }
    }

    #[test]
    fn every_exit_discriminant_round_trips() {
        for kind in [ExitKind::Code, ExitKind::Signal, ExitKind::Fault, ExitKind::EngineError] {
            let exit = EngineExit {
                kind,
                guest_status: -1,
                detail: 1,
                fault: None,
            };
            assert_eq!(
                Message::decode(&Message::Exit(exit).encode().unwrap()),
                Ok(Message::Exit(exit))
            );
        }
        for isa in [GuestIsa::Aarch64, GuestIsa::X86_64] {
            for reason in [
                FaultReason::Fetch,
                FaultReason::Memory,
                FaultReason::Decode,
                FaultReason::Unsupported,
                FaultReason::Frozen,
                FaultReason::CacheEpoch,
                FaultReason::Protocol,
                FaultReason::NativeFatal,
            ] {
                for access in [FaultAccess::Read, FaultAccess::Write, FaultAccess::Execute] {
                    let mut exit = fault();
                    let diagnostic = exit.fault.as_mut().unwrap();
                    diagnostic.isa = isa;
                    diagnostic.reason = reason;
                    diagnostic.access = Some(access);
                    assert_eq!(
                        Message::decode(&Message::Exit(exit).encode().unwrap()),
                        Ok(Message::Exit(exit))
                    );
                }
            }
        }
    }

    #[test]
    fn every_failure_stage_round_trips() {
        for stage in [
            FailureStage::Decode,
            FailureStage::Create,
            FailureStage::Start,
            FailureStage::Control,
            FailureStage::Destroy,
        ] {
            let message = Message::Error { stage, code: -125 };
            assert_eq!(Message::decode(&message.encode().unwrap()), Ok(message));
        }
    }

    #[test]
    fn every_truncation_and_oversize_is_rejected() {
        let frame = Message::Exit(fault()).encode().unwrap();
        for length in 0..FRAME_SIZE {
            assert_eq!(Message::decode(&frame[..length]), Err(WireError::Size));
        }
        let mut oversized = frame.to_vec();
        oversized.push(0);
        assert_eq!(Message::decode(&oversized), Err(WireError::Size));
    }

    #[test]
    fn header_and_payload_bounds_fail_closed() {
        let frame = Message::Ready.encode().unwrap();
        for (offset, error) in [(0, WireError::Magic), (4, WireError::Version), (6, WireError::Kind)] {
            let mut corrupt = frame;
            corrupt[offset] ^= 0xff;
            assert_eq!(Message::decode(&corrupt), Err(error));
        }
        let mut too_large = frame;
        put_u32(&mut too_large, 8, (PAYLOAD_SIZE + 1) as u32);
        assert_eq!(Message::decode(&too_large), Err(WireError::Length));
        let mut header_reserved = frame;
        header_reserved[15] = 1;
        assert_eq!(Message::decode(&header_reserved), Err(WireError::Reserved));
        let mut trailing = frame;
        trailing[FRAME_SIZE - 1] = 1;
        assert_eq!(Message::decode(&trailing), Err(WireError::Reserved));
    }

    #[test]
    fn stop_and_resize_enforce_linux_and_terminal_bounds() {
        for signal in [i32::MIN, -1, 0, 65, i32::MAX] {
            assert_eq!(
                Message::Stop(StopRequest::Signal(signal)).encode(),
                Err(WireError::Value)
            );
        }
        for message in [
            Message::Resize { rows: 0, columns: 1 },
            Message::Resize { rows: 1, columns: 0 },
        ] {
            assert_eq!(message.encode(), Err(WireError::Value));
        }
        let mut stop = Message::Stop(StopRequest::Interrupt).encode().unwrap();
        put_i32(&mut stop, HEADER_SIZE + 4, 9);
        assert_eq!(Message::decode(&stop), Err(WireError::Value));
        stop[HEADER_SIZE + 1] = 1;
        assert_eq!(Message::decode(&stop), Err(WireError::Reserved));
    }

    #[test]
    fn exit_fault_invariants_are_enforced() {
        let mut invalid = fault();
        invalid.fault.as_mut().unwrap().opcode_len = 16;
        assert_eq!(Message::Exit(invalid).encode(), Err(WireError::Value));

        let mut noncanonical = fault();
        noncanonical.fault.as_mut().unwrap().opcode[14] = 1;
        assert_eq!(Message::Exit(noncanonical).encode(), Err(WireError::Value));

        let mut mismatched_operand = fault();
        mismatched_operand.fault.as_mut().unwrap().access = None;
        assert_eq!(Message::Exit(mismatched_operand).encode(), Err(WireError::Value));

        let mut frame = Message::Exit(fault()).encode().unwrap();
        frame[HEADER_SIZE + 20] = 2;
        assert_eq!(Message::decode(&frame), Err(WireError::Value));
        let mut frame = Message::Exit(fault()).encode().unwrap();
        frame[HEADER_SIZE + 19] = 4;
        assert_eq!(Message::decode(&frame), Err(WireError::Value));
        let mut frame = Message::Exit(fault()).encode().unwrap();
        frame[HEADER_SIZE + 19] = 0;
        assert_eq!(Message::decode(&frame), Err(WireError::Value));
        let mut frame = Message::Exit(fault()).encode().unwrap();
        frame[HEADER_SIZE + 47] = 1;
        assert_eq!(Message::decode(&frame), Err(WireError::Reserved));
    }

    #[test]
    fn every_declared_kind_rejects_the_wrong_length() {
        for kind in [READY, START, STOP, RESIZE, EXIT, ERROR] {
            let mut frame = Message::Ready.encode().unwrap();
            put_u16(&mut frame, 6, kind);
            put_u32(&mut frame, 8, 1);
            frame[HEADER_SIZE] = 0;
            assert_eq!(Message::decode(&frame), Err(WireError::Length));
        }
    }
}
