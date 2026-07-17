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
use crate::transport::adapter::unix::FrameOutcome;
use crate::transport::model::header::{SubmitHeader, ACK_FAIL, ACK_OK};
use crate::transport::model::readback::{
    ReadbackRequest, READBACK_FAIL, READBACK_MAGIC, READBACK_OK,
};

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

/// A per-connection host handler that serves BOTH the submit path and the additive device→host readback
/// path over one connection. The submit half mirrors the `serve_connection` closure; the readback half has
/// a default that fails every request, so a submit-only host never has to implement it.
///
/// A single `&mut H` drives both halves, so a host that owns a runtime `Session` + executor can mutate on
/// submit and read on readback without the double-mutable-borrow two separate closures would require.
pub trait ConnectionHandler {
    /// Handle a decoded submit batch, returning the ack verdict.
    fn submit(&mut self, header: &SubmitHeader, batch: &[Cmd]) -> Verdict;

    /// Serve a readback request, returning the bytes on success or `None` to fail the readback. Default:
    /// unsupported (fail).
    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        let _ = req;
        None
    }
}

/// Adapts a bare submit closure into a [`ConnectionHandler`] whose readback half always fails — the
/// back-compat shim behind [`serve_connection`].
struct SubmitOnly<H>(H);

impl<H, V> ConnectionHandler for SubmitOnly<H>
where
    H: FnMut(&SubmitHeader, &[Cmd]) -> V,
    V: Into<Verdict>,
{
    fn submit(&mut self, header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        (self.0)(header, batch).into()
    }
}

/// The shared serve loop behind both entry points: advertise `caps`, then read frames and route each by its
/// header. A frame carrying the reserved [`READBACK_MAGIC`] sentinel in `surface_id` is a readback request
/// (answered with a length-prefixed byte payload); every other frame is a submit (decoded and answered with
/// the 1-byte ack). Returns on a clean EOF at a frame boundary.
fn serve_loop<H: ConnectionHandler>(
    stream: &UnixStream,
    caps: &Capabilities,
    handler: &mut H,
) -> io::Result<()> {
    // The guest reads this off the connection and negotiates before advertising any API feature.
    unix::write_handshake(stream, caps)?;
    loop {
        let frame = match unix::read_frame_outcome(stream)? {
            FrameOutcome::Frame(f) => f,
            FrameOutcome::Eof => return Ok(()), // peer closed the connection
            // An over-cap frame must NOT tear down the persistent connection (that drops the host's warm
            // per-connection caches AND every subsequent frame — the guest sees `Broken pipe`). Drain the
            // oversized payload to resync the stream, NACK it, and keep serving. A truncated drain means the
            // peer is genuinely gone, which propagates as an error (the connection really did end).
            FrameOutcome::TooLarge(header) => {
                hl_log::hl_count!(hl_log::tag::TRANSPORT, "nacks");
                unix::drain_payload(stream, header.len)?;
                unix::write_ack(stream, ACK_FAIL)?;
                continue;
            }
        };
        if frame.header.surface_id == READBACK_MAGIC {
            // Device→host readback: decode the fixed request, serve it, and reply with the disjoint
            // length-prefixed response (never the 1-byte submit ack). A panicking readback op is contained
            // and failed rather than allowed to unwind the connection thread.
            let bytes = ReadbackRequest::from_bytes(&frame.payload)
                .and_then(|req| catch_handler(|| handler.read_buffer(&req)).unwrap_or(None));
            match bytes {
                Some(bytes) => unix::write_readback_response(stream, READBACK_OK, &bytes)?,
                None => unix::write_readback_response(stream, READBACK_FAIL, &[])?,
            }
            continue;
        }
        // Decode + execute one submit. A malformed payload is NACKed at the boundary (never handed to the
        // handler); a handler that PANICS on some op is caught and NACKed rather than allowed to unwind the
        // connection thread (which would drop the socket = `Broken pipe` for every later frame). Either way
        // the connection stays alive and serves the next frame.
        let verdict = match decode_stream(&frame.payload) {
            Ok(batch) => {
                hl_log::hl_debug!(
                    hl_log::tag::TRANSPORT,
                    "frame bytes={} cmds={}",
                    frame.payload.len(),
                    batch.len()
                );
                catch_handler(|| handler.submit(&frame.header, &batch)).unwrap_or(Verdict::Nack)
            }
            Err(_error) => {
                hl_log::hl_warn!(
                    hl_log::tag::WIRE,
                    "decode failed bytes={}: {}",
                    frame.payload.len(),
                    _error
                );
                Verdict::Nack
            }
        };
        if matches!(verdict, Verdict::Nack) {
            hl_log::hl_count!(hl_log::tag::TRANSPORT, "nacks");
        }
        unix::write_ack(stream, verdict.ack_byte())?;
    }
}

/// Run one handler op under [`catch_unwind`](std::panic::catch_unwind) so a panicking executor op (an
/// `unwrap`/`expect`/index-out-of-bounds deep in a backend) is CONTAINED to that one frame — the serve loop
/// turns it into a NACK and keeps the connection alive — instead of unwinding the connection thread and
/// dropping the socket (which the guest observes as `Broken pipe` on every subsequent frame). `Err(())` means
/// the op panicked. `AssertUnwindSafe` is sound here: on a panic we abandon the in-flight op's result and
/// only ever NACK, never observe partially-mutated state.
fn catch_handler<R>(op: impl FnOnce() -> R) -> Result<R, ()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(op)).map_err(|_| ())
}

/// Serve one connection to completion: advertise `caps`, then loop reading framed submits, decoding each
/// via the protocol codec, handing the decoded `(header, batch)` to `handler`, and writing its verdict as
/// the ack. A frame whose payload fails to decode is NACKed without invoking the handler. Returns when the
/// peer closes the connection (clean EOF at a frame boundary).
///
/// The handler is anything callable as `FnMut(&SubmitHeader, &[Cmd]) -> V` where `V: Into<Verdict>` — a
/// plain `|_, batch| true` closure, or a wrapper delegating to a runtime / `CommandSink`. This entry point
/// serves the submit path only; a readback request on the connection is failed. Use
/// [`serve_connection_with_handler`] to serve readback too.
pub fn serve_connection<H, V>(
    stream: &UnixStream,
    caps: &Capabilities,
    handler: H,
) -> io::Result<()>
where
    H: FnMut(&SubmitHeader, &[Cmd]) -> V,
    V: Into<Verdict>,
{
    serve_loop(stream, caps, &mut SubmitOnly(handler))
}

/// Serve one connection to completion driving a [`ConnectionHandler`], which serves BOTH submit and the
/// additive device→host readback path. Otherwise identical to [`serve_connection`].
pub fn serve_connection_with_handler<H: ConnectionHandler>(
    stream: &UnixStream,
    caps: &Capabilities,
    handler: &mut H,
) -> io::Result<()> {
    serve_loop(stream, caps, handler)
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
