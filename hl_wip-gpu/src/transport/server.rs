//! The host-side serve loop: accept connections, advertise capabilities, read framed submits, decode them
//! via `protocol::codec`, hand the decoded batch to a handler, and write the ack.
//!
//! Generic over the handler so the transport never links a concrete executor — the composition root passes
//! the runtime (or, for tests, a plain closure / recording sink). The transport decodes and frames; it does
//! not execute. Ported from the host executor's reader (`hl-display`'s `run_executor`) — same 16-byte
//! header, same 1-byte ack.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};

use crate::protocol::codec::decode_stream;
use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::Cmd;
use crate::transport::adapter::unix;
use crate::transport::model::header::{SubmitHeader, ACK_FAIL, ACK_OK};

/// A handler's verdict for one decoded frame, mapped straight to the wire ack.
pub enum Verdict {
    /// The frame was accepted (replayed/committed) → [`ACK_OK`].
    Ack,
    /// The frame was rejected (replay error / missing surface) → [`ACK_FAIL`].
    Nack,
}

impl From<bool> for Verdict {
    fn from(ok: bool) -> Self {
        if ok {
            Verdict::Ack
        } else {
            Verdict::Nack
        }
    }
}

impl Verdict {
    fn ack_byte(&self) -> u8 {
        match self {
            Verdict::Ack => ACK_OK,
            Verdict::Nack => ACK_FAIL,
        }
    }
}

/// Serve one connection to completion: advertise `caps`, then loop reading framed submits, decoding each
/// via the protocol codec, handing the decoded `(header, batch)` to `handler`, and writing its verdict as
/// the ack. A frame whose payload fails to decode is NACKed without invoking the handler. Returns when the
/// peer closes the connection (clean EOF at a frame boundary).
///
/// The handler is anything callable as `FnMut(&SubmitHeader, &[Cmd]) -> V` where `V: Into<Verdict>` — a
/// plain `|_, batch| true` closure, or a wrapper delegating to a runtime / `CommandSink`.
pub fn serve_connection<H, V>(stream: &UnixStream, caps: &Capabilities, mut handler: H) -> io::Result<()>
where
    H: FnMut(&SubmitHeader, &[Cmd]) -> V,
    V: Into<Verdict>,
{
    // The guest reads this off the connection and negotiates before advertising any API feature.
    unix::write_handshake(stream, caps)?;
    loop {
        let frame = match unix::read_frame(stream)? {
            Some(f) => f,
            None => return Ok(()), // peer closed the connection
        };
        let verdict = match decode_stream(&frame.payload) {
            Ok(batch) => handler(&frame.header, &batch).into(),
            // A malformed payload is rejected at the boundary, never handed to the handler.
            Err(_) => Verdict::Nack,
        };
        unix::write_ack(stream, verdict.ack_byte())?;
    }
}

/// Accept connections on `listener` forever, serving each to completion with a fresh borrow of `handler`.
/// Connections are handled sequentially (one warm per-client backend at a time), matching the single
/// persistent guest connection the current transport uses.
pub fn serve<H, V>(listener: &UnixListener, caps: &Capabilities, mut handler: H) -> io::Result<()>
where
    H: FnMut(&SubmitHeader, &[Cmd]) -> V,
    V: Into<Verdict>,
{
    for stream in listener.incoming() {
        let stream = stream?;
        serve_connection(&stream, caps, &mut handler)?;
    }
    Ok(())
}
