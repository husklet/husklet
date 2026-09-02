//! Carrying frames over a byte stream.
//!
//! The protocol is defined without a socket, but a session eventually runs on
//! one. This is the whole of that translation: blocking reads and writes over
//! anything that is a [`std::io::Read`] or [`std::io::Write`], which in a host
//! is the extension's private `UnixStream` and in a test is a pair of vectors.
//!
//! A peer controls how bytes arrive, so the read buffer is the one thing that
//! must never be peer-controlled. It is capped at a single largest frame and
//! reclaimed as frames are taken, which makes a hostile peer unable to grow it
//! whatever it splits, concatenates, or declares.

use std::io::{Read, Write};

use crate::frame::{Frame, Malformed};

/// The most bytes taken from the stream in one read.
const CHUNK: usize = 16 * 1024;

/// A frame stream over a blocking byte stream.
#[derive(Debug)]
pub struct Wire<S> {
    stream: S,
    buffer: Vec<u8>,
}

impl<S> Wire<S> {
    /// The most bytes ever held at once: one whole frame of the largest size
    /// the protocol admits. A declared length above this is refused by
    /// [`Frame::decode`] before anything is reserved for it.
    const CAPACITY: usize = Frame::HEADER + Frame::PAYLOAD_LIMIT;

    /// Wraps a stream that has not been read from or written to yet.
    #[must_use]
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    /// Bytes read from the peer but not yet returned as a frame.
    ///
    /// This is bounded by one largest frame for the life of the connection,
    /// and is exposed so that bound can be asserted rather than assumed.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Returns the wrapped stream, discarding any partially read frame.
    #[must_use]
    pub fn into_stream(self) -> S {
        self.stream
    }
}

impl<S: Write> Wire<S> {
    /// Writes one frame and flushes it, so a request is not left sitting in a
    /// buffer while its sender waits for the reply.
    ///
    /// # Errors
    /// Returns `Transit::Malformed` when the payload exceeds the limit, and
    /// `Transit::Io` when the stream refuses the write.
    pub fn send(&mut self, frame: &Frame) -> Result<(), Transit> {
        let bytes = frame.encode().map_err(Transit::Malformed)?;
        self.stream.write_all(&bytes).map_err(Transit::from)?;
        self.stream.flush().map_err(Transit::from)
    }
}

impl<S: Read> Wire<S> {
    /// Reads until one whole frame is available and returns it, leaving any
    /// bytes that followed for the next call.
    ///
    /// # Errors
    /// Returns `Transit::Closed` at end of stream, `Transit::Malformed` when
    /// the peer's bytes are not a frame, and `Transit::Io` when the read fails.
    pub fn receive(&mut self) -> Result<Frame, Transit> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(frame);
            }
            self.fill()?;
        }
    }

    /// Takes the frame at the front of the buffer, if a whole one is there.
    fn take(&mut self) -> Result<Option<Frame>, Transit> {
        let Some((frame, consumed)) = Frame::decode(&self.buffer).map_err(Transit::Malformed)? else {
            return Ok(None);
        };
        // Dropping the consumed prefix is what keeps a long-lived connection
        // from growing a buffer the size of everything it has ever carried.
        self.buffer.drain(..consumed);
        Ok(Some(frame))
    }

    /// Reads one chunk, never past the single-frame ceiling.
    fn fill(&mut self) -> Result<(), Transit> {
        let room = Self::CAPACITY.saturating_sub(self.buffer.len());
        if room == 0 {
            return Err(Transit::Malformed(Malformed::Oversize {
                declared: Self::CAPACITY,
            }));
        }
        let mut chunk = [0_u8; CHUNK];
        let wanted = room.min(chunk.len());
        let count = self.stream.read(&mut chunk[..wanted]).map_err(Transit::from)?;
        if count == 0 {
            return Err(Transit::Closed);
        }
        self.buffer.extend_from_slice(&chunk[..count]);
        Ok(())
    }
}

/// Why a frame did not cross the wire.
///
/// A peer hanging up is `Closed` rather than an error, because an extension
/// exiting is the ordinary end of a session and must not be reported as a
/// fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transit {
    Closed,
    /// A deadline-bound stream has no bytes ready yet. Any partial frame stays buffered.
    Pending,
    Malformed(Malformed),
    Io(String),
}

impl From<std::io::Error> for Transit {
    fn from(error: std::io::Error) -> Self {
        if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            return Self::Pending;
        }
        // The message is kept rather than the error, so a `Transit` can be
        // compared, cloned, and carried across a channel like every other
        // outcome in this crate.
        Self::Io(error.to_string())
    }
}

impl std::fmt::Display for Transit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(formatter, "the peer closed the connection"),
            Self::Pending => write!(formatter, "the connection has no frame ready"),
            Self::Malformed(reason) => write!(formatter, "the peer sent {reason}"),
            Self::Io(message) => write!(formatter, "the connection failed: {message}"),
        }
    }
}

impl std::error::Error for Transit {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(reason) => Some(reason),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Read};

    use super::{Transit, Wire};
    use crate::{Frame, Kind};

    struct TimedRead(VecDeque<io::Result<Vec<u8>>>);

    impl Read for TimedRead {
        fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
            let bytes = self.0.pop_front().expect("scripted read")?;
            target[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    #[test]
    fn a_deadline_preserves_the_partial_frame_for_the_next_receive() {
        let expected = Frame::control(Kind::Credit, b"4".to_vec());
        let bytes = expected.encode().expect("frame");
        let split = 5;
        let reader = TimedRead(
            [
                Ok(bytes[..split].to_vec()),
                Err(io::Error::new(io::ErrorKind::TimedOut, "tick")),
                Ok(bytes[split..].to_vec()),
            ]
            .into_iter()
            .collect(),
        );
        let mut wire = Wire::new(reader);

        assert_eq!(wire.receive(), Err(Transit::Pending));
        assert_eq!(wire.buffered(), split);
        assert_eq!(wire.receive(), Ok(expected));
    }
}
