use ring::hmac;
use std::io::{Read, Write};
use zeroize::Zeroize;

const MAGIC: &[u8; 4] = b"HLSF";
const VERSION: u16 = 1;
const HEADER: usize = 36;
const TAG: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Direction {
    Worker = 1,
    Authority = 2,
}

impl Direction {
    const fn peer(self) -> Self {
        match self {
            Self::Worker => Self::Authority,
            Self::Authority => Self::Worker,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Ping = 1,
    Close = 2,
    Ready = 3,
    Provider = 4,
    Network = 5,
}

impl FrameKind {
    fn parse(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Ping),
            2 => Ok(Self::Close),
            3 => Ok(Self::Ready),
            4 => Ok(Self::Provider),
            5 => Ok(Self::Network),
            _ => Err(FrameError::Kind),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    Io,
    Closed,
    Truncated,
    Magic,
    Version,
    Direction,
    Kind,
    Session,
    Sequence,
    Oversized,
    Authentication,
}

pub struct Session {
    key: [u8; 32],
    identity: [u8; 16],
    direction: Direction,
    send_sequence: u64,
    receive_sequence: u64,
    maximum: usize,
}

impl Session {
    pub(crate) const fn new(key: [u8; 32], identity: [u8; 16], direction: Direction, maximum: usize) -> Self {
        Self {
            key,
            identity,
            direction,
            send_sequence: 1,
            receive_sequence: 1,
            maximum,
        }
    }

    pub fn send<W: Write>(&mut self, output: &mut W, kind: FrameKind, payload: &[u8]) -> Result<(), FrameError> {
        if payload.len() > self.maximum {
            return Err(FrameError::Oversized);
        }
        let size = u32::try_from(payload.len()).map_err(|_| FrameError::Oversized)?;
        let mut header = [0_u8; HEADER];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        header[6] = self.direction as u8;
        header[7] = kind as u8;
        header[8..12].copy_from_slice(&size.to_le_bytes());
        header[12..28].copy_from_slice(&self.identity);
        header[28..36].copy_from_slice(&self.send_sequence.to_le_bytes());
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.key);
        let mut context = hmac::Context::with_key(&key);
        context.update(&header);
        context.update(payload);
        output.write_all(&header).map_err(|_| FrameError::Io)?;
        output.write_all(payload).map_err(|_| FrameError::Io)?;
        output.write_all(context.sign().as_ref()).map_err(|_| FrameError::Io)?;
        self.send_sequence = self.send_sequence.checked_add(1).ok_or(FrameError::Sequence)?;
        Ok(())
    }

    pub fn receive<R: Read>(&mut self, input: &mut R) -> Result<Frame, FrameError> {
        let mut header = [0_u8; HEADER];
        let count = input.read(&mut header).map_err(|_| FrameError::Io)?;
        if count == 0 {
            return Err(FrameError::Closed);
        }
        input
            .read_exact(&mut header[count..])
            .map_err(|_| FrameError::Truncated)?;
        if &header[0..4] != MAGIC {
            return Err(FrameError::Magic);
        }
        if u16::from_le_bytes(header[4..6].try_into().unwrap()) != VERSION {
            return Err(FrameError::Version);
        }
        if header[6] != self.direction.peer() as u8 {
            return Err(FrameError::Direction);
        }
        let kind = FrameKind::parse(header[7])?;
        let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if size > self.maximum {
            return Err(FrameError::Oversized);
        }
        if header[12..28] != self.identity {
            return Err(FrameError::Session);
        }
        if u64::from_le_bytes(header[28..36].try_into().unwrap()) != self.receive_sequence {
            return Err(FrameError::Sequence);
        }
        let mut payload = vec![0_u8; size];
        input.read_exact(&mut payload).map_err(|_| FrameError::Truncated)?;
        let mut tag = [0_u8; TAG];
        input.read_exact(&mut tag).map_err(|_| FrameError::Truncated)?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.key);
        let mut signed = Vec::with_capacity(HEADER + size);
        signed.extend_from_slice(&header);
        signed.extend_from_slice(&payload);
        hmac::verify(&key, &signed, &tag).map_err(|_| FrameError::Authentication)?;
        self.receive_sequence = self.receive_sequence.checked_add(1).ok_or(FrameError::Sequence)?;
        Ok(Frame { kind, payload })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn encoded() -> Vec<u8> {
        let mut session = Session::new([3; 32], [4; 16], Direction::Worker, 64);
        let mut bytes = Vec::new();
        session.send(&mut bytes, FrameKind::Ping, b"data").unwrap();
        bytes
    }

    fn authority() -> Session {
        Session::new([3; 32], [4; 16], Direction::Authority, 64)
    }

    #[test]
    fn reflection() {
        let mut bytes = encoded();
        bytes[6] = Direction::Authority as u8;
        assert_eq!(authority().receive(&mut Cursor::new(bytes)), Err(FrameError::Direction));
    }

    #[test]
    fn unknown_version() {
        let mut bytes = encoded();
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(authority().receive(&mut Cursor::new(bytes)), Err(FrameError::Version));
    }

    #[test]
    fn unknown_kind() {
        let mut bytes = encoded();
        bytes[7] = 99;
        assert_eq!(authority().receive(&mut Cursor::new(bytes)), Err(FrameError::Kind));
    }

    #[test]
    fn truncation() {
        let mut bytes = encoded();
        bytes.truncate(HEADER + 2);
        assert_eq!(authority().receive(&mut Cursor::new(bytes)), Err(FrameError::Truncated));
    }

    #[test]
    fn bad_tag() {
        let mut bytes = encoded();
        *bytes.last_mut().unwrap() ^= 1;
        assert_eq!(
            authority().receive(&mut Cursor::new(bytes)),
            Err(FrameError::Authentication)
        );
    }

    #[test]
    fn empty_stream() {
        assert_eq!(
            authority().receive(&mut Cursor::new(Vec::new())),
            Err(FrameError::Closed)
        );
    }
}
