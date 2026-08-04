use super::{Direction, Session};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use std::io::{Read, Write};
use zeroize::Zeroize;

const VERSION: u16 = 1;
const HELLO: usize = 42;
const PROOF: usize = 74;
const ACCEPTED: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub frame_bytes: u32,
    pub in_flight: u16,
}

impl Limits {
    pub const fn new(frame_bytes: u32, in_flight: u16) -> Result<Self, HandshakeError> {
        if frame_bytes == 0 || frame_bytes > 1024 * 1024 || in_flight == 0 || in_flight > 1024 {
            Err(HandshakeError::Limits)
        } else {
            Ok(Self { frame_bytes, in_flight })
        }
    }
}

pub struct Secret([u8; 32]);
impl Secret {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn pair() -> Result<(Self, Self), HandshakeError> {
        let mut bytes = [0_u8; 32];
        Handshake::random(&mut bytes)?;
        let pair = (Self(bytes), Self(bytes));
        bytes.zeroize();
        Ok(pair)
    }

    pub fn send<W: Write>(mut self, output: &mut W) -> Result<(), HandshakeError> {
        let result = output.write_all(&self.0).map_err(|_| HandshakeError::Io);
        self.0.zeroize();
        result
    }

    pub fn receive<R: Read>(input: &mut R) -> Result<Self, HandshakeError> {
        let mut bytes = [0_u8; 32];
        input.read_exact(&mut bytes).map_err(|_| HandshakeError::Truncated)?;
        Ok(Self(bytes))
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    Io,
    Truncated,
    Version,
    Limits,
    Authentication,
    Random,
}

pub fn accept<T: Read + Write>(stream: &mut T, mut secret: Secret, limits: Limits) -> Result<Session, HandshakeError> {
    let mut challenge = [0_u8; 32];
    Handshake::random(&mut challenge)?;
    let hello = Handshake::parameters(limits, &challenge);
    stream.write_all(&hello).map_err(|_| HandshakeError::Io)?;
    let mut proof = [0_u8; PROOF];
    stream.read_exact(&mut proof).map_err(|_| HandshakeError::Truncated)?;
    let selected = Handshake::parse(&proof[0..10], limits)?;
    let mut transcript = Vec::with_capacity(HELLO + 42);
    transcript.extend_from_slice(&hello);
    transcript.extend_from_slice(&proof[0..42]);
    Handshake::verify(&secret.0, b"worker", &transcript, &proof[42..74])?;
    let nonce: [u8; 32] = proof[10..42].try_into().unwrap();
    let mut identity = [0_u8; 16];
    Handshake::random(&mut identity)?;
    let key = Handshake::derive(&secret.0, &challenge, &nonce, &identity);
    let mut response = [0_u8; ACCEPTED];
    response[0..16].copy_from_slice(&identity);
    let mut acceptance = transcript.clone();
    acceptance.extend_from_slice(&identity);
    response[16..48].copy_from_slice(&Handshake::sign(&secret.0, b"authority", &acceptance));
    stream.write_all(&response).map_err(|_| HandshakeError::Io)?;
    secret.0.zeroize();
    challenge.zeroize();
    Ok(Session::new(
        key,
        identity,
        Direction::Authority,
        selected.frame_bytes as usize,
    ))
}

pub fn connect<T: Read + Write>(
    stream: &mut T,
    mut secret: Secret,
    requested: Limits,
) -> Result<Session, HandshakeError> {
    let mut hello = [0_u8; HELLO];
    stream.read_exact(&mut hello).map_err(|_| HandshakeError::Truncated)?;
    let offered = Handshake::parse(&hello[0..10], Limits::new(1024 * 1024, 1024).unwrap())?;
    let selected = Limits::new(
        requested.frame_bytes.min(offered.frame_bytes),
        requested.in_flight.min(offered.in_flight),
    )?;
    let challenge: [u8; 32] = hello[10..42].try_into().unwrap();
    let mut nonce = [0_u8; 32];
    Handshake::random(&mut nonce)?;
    let mut proof = [0_u8; PROOF];
    proof[0..10].copy_from_slice(&Handshake::parameters(selected, &nonce)[0..10]);
    proof[10..42].copy_from_slice(&nonce);
    let mut transcript = Vec::with_capacity(HELLO + 42);
    transcript.extend_from_slice(&hello);
    transcript.extend_from_slice(&proof[0..42]);
    proof[42..74].copy_from_slice(&Handshake::sign(&secret.0, b"worker", &transcript));
    stream.write_all(&proof).map_err(|_| HandshakeError::Io)?;
    let mut response = [0_u8; ACCEPTED];
    stream
        .read_exact(&mut response)
        .map_err(|_| HandshakeError::Truncated)?;
    let identity: [u8; 16] = response[0..16].try_into().unwrap();
    let mut acceptance = transcript.clone();
    acceptance.extend_from_slice(&identity);
    Handshake::verify(&secret.0, b"authority", &acceptance, &response[16..48])?;
    let key = Handshake::derive(&secret.0, &challenge, &nonce, &identity);
    secret.0.zeroize();
    nonce.zeroize();
    Ok(Session::new(
        key,
        identity,
        Direction::Worker,
        selected.frame_bytes as usize,
    ))
}

struct Handshake;

impl Handshake {
    fn parameters(limits: Limits, nonce: &[u8; 32]) -> [u8; HELLO] {
        let mut bytes = [0_u8; HELLO];
        bytes[0..2].copy_from_slice(&VERSION.to_le_bytes());
        bytes[2..6].copy_from_slice(&limits.frame_bytes.to_le_bytes());
        bytes[6..8].copy_from_slice(&limits.in_flight.to_le_bytes());
        bytes[10..42].copy_from_slice(nonce);
        bytes
    }

    fn parse(bytes: &[u8], maximum: Limits) -> Result<Limits, HandshakeError> {
        if u16::from_le_bytes(bytes[0..2].try_into().unwrap()) != VERSION {
            return Err(HandshakeError::Version);
        }
        let value = Limits::new(
            u32::from_le_bytes(bytes[2..6].try_into().unwrap()),
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
        )?;
        if bytes[8..10] != [0, 0] || value.frame_bytes > maximum.frame_bytes || value.in_flight > maximum.in_flight {
            Err(HandshakeError::Limits)
        } else {
            Ok(value)
        }
    }

    fn sign(secret: &[u8], label: &[u8], bytes: &[u8]) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let mut context = hmac::Context::with_key(&key);
        context.update(label);
        context.update(bytes);
        context.sign().as_ref().try_into().unwrap()
    }

    fn verify(secret: &[u8], label: &[u8], bytes: &[u8], tag: &[u8]) -> Result<(), HandshakeError> {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let mut signed = Vec::with_capacity(label.len() + bytes.len());
        signed.extend_from_slice(label);
        signed.extend_from_slice(bytes);
        hmac::verify(&key, &signed, tag).map_err(|_| HandshakeError::Authentication)
    }

    fn derive(secret: &[u8], challenge: &[u8], nonce: &[u8], identity: &[u8]) -> [u8; 32] {
        Self::sign(secret, b"session-key", &[challenge, nonce, identity].concat())
    }

    fn random(output: &mut [u8]) -> Result<(), HandshakeError> {
        SystemRandom::new().fill(output).map_err(|_| HandshakeError::Random)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{FrameError, FrameKind};
    use std::{io::Write, os::unix::net::UnixStream, thread};

    fn limits() -> Limits {
        Limits::new(4096, 8).unwrap()
    }
    fn secret() -> Secret {
        Secret::new([7; 32])
    }

    #[test]
    fn ping_close() {
        let (mut worker_io, mut authority_io) = UnixStream::pair().unwrap();
        let authority = thread::spawn(move || {
            let mut session = accept(&mut authority_io, secret(), limits()).unwrap();
            let frame = session.receive(&mut authority_io).unwrap();
            session
                .send(&mut authority_io, FrameKind::Ping, &frame.payload)
                .unwrap();
            assert_eq!(session.receive(&mut authority_io).unwrap().kind, FrameKind::Close);
        });
        let mut session = connect(&mut worker_io, secret(), limits()).unwrap();
        session.send(&mut worker_io, FrameKind::Ping, b"proof").unwrap();
        assert_eq!(session.receive(&mut worker_io).unwrap().payload, b"proof");
        session.send(&mut worker_io, FrameKind::Close, &[]).unwrap();
        authority.join().unwrap();
    }

    #[test]
    fn replay() {
        let (mut worker_io, mut authority_io) = UnixStream::pair().unwrap();
        let authority = thread::spawn(move || {
            let mut session = accept(&mut authority_io, secret(), limits()).unwrap();
            session.receive(&mut authority_io).unwrap();
            assert_eq!(session.receive(&mut authority_io), Err(FrameError::Sequence));
        });
        let mut session = connect(&mut worker_io, secret(), limits()).unwrap();
        let mut captured = Vec::new();
        session.send(&mut captured, FrameKind::Ping, b"once").unwrap();
        worker_io.write_all(&captured).unwrap();
        worker_io.write_all(&captured).unwrap();
        authority.join().unwrap();
    }

    #[test]
    fn oversized() {
        let (mut worker_io, mut authority_io) = UnixStream::pair().unwrap();
        let authority = thread::spawn(move || {
            let mut session = accept(&mut authority_io, secret(), limits()).unwrap();
            assert_eq!(session.receive(&mut authority_io), Err(FrameError::Oversized));
        });
        let mut session = connect(&mut worker_io, secret(), limits()).unwrap();
        let mut frame = Vec::new();
        session.send(&mut frame, FrameKind::Ping, b"x").unwrap();
        frame[8..12].copy_from_slice(&5000_u32.to_le_bytes());
        worker_io.write_all(&frame[..36]).unwrap();
        authority.join().unwrap();
    }

    #[test]
    fn wrong_secret() {
        let (mut worker_io, mut authority_io) = UnixStream::pair().unwrap();
        let authority = thread::spawn(move || accept(&mut authority_io, secret(), limits()));
        let worker = connect(&mut worker_io, Secret::new([8; 32]), limits());
        assert!(matches!(worker, Err(HandshakeError::Truncated)));
        assert!(matches!(authority.join().unwrap(), Err(HandshakeError::Authentication)));
    }
}
