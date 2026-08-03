//! The host-side serve loop: accept connections, advertise capabilities, read framed submits, decode them
//! via `protocol::codec`, hand the decoded batch to a handler, and write the ack.
//!
//! Generic over the handler so the transport never links a concrete executor — the composition root passes
//! the runtime (or, for tests, a plain closure / recording sink). The transport decodes and frames; it does
//! not execute. Ported from the host executor's reader (`hl-display`'s `run_executor`) — same 16-byte
//! header, same 1-byte ack.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::Cmd;
use crate::transport::adapter::unix;
use crate::transport::adapter::unix::FrameOutcome;
use crate::transport::model::header::{RefusalKind, SubmitHeader, ACK_FAIL, ACK_OK, ACK_PARTIAL};
use crate::transport::model::readback::{
    readback_kind, ReadbackRequest, READBACK_FAIL, READBACK_MAGIC, READBACK_OK,
};

/// A handler's verdict for one decoded frame, mapped straight to the wire ack.
#[derive(Clone, PartialEq, Debug)]
pub enum Verdict {
    /// The frame was accepted (replayed/committed) → [`ACK_OK`].
    Ack,
    /// The frame was refused, carrying as much of the reason as one byte can hold. `Nack` is
    /// [`RefusalKind::Unstated`] — a host that has a typed error should use [`Verdict::for_error`]
    /// instead, because the reason is otherwise destroyed here and the guest can only guess.
    Refused(RefusalKind),
    /// The named operation was refused, but successful commands in the frame were committed.
    Partial { kind: RefusalKind, commands: Vec<Cmd>, replayable: bool },
}

#[allow(non_upper_case_globals)]
impl Verdict {
    /// The unclassified refusal, kept as a name so existing hosts and tests read unchanged.
    pub const Nack: Verdict = Verdict::Refused(RefusalKind::Unstated);

    /// Refuse a frame with the class of the error that caused it.
    pub fn for_error(error: &crate::protocol::model::error::GpuError) -> Self {
        match error {
            crate::GpuError::Partial(error) if !error.is_fatal() => {
                Verdict::Partial { kind: RefusalKind::for_error(error), commands: Vec::new(), replayable: false }
            }
            error => Verdict::Refused(RefusalKind::for_error(error)),
        }
    }
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
            Verdict::Refused(kind) => kind.ack(),
            Verdict::Partial { kind, .. } => kind.ack() | ACK_PARTIAL,
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

    fn poll_fence(&mut self, req: &ReadbackRequest) -> Option<bool> {
        let _ = req;
        None
    }

    fn wait_fence(&mut self, req: &ReadbackRequest) -> Option<crate::FenceWait> {
        let _ = req;
        None
    }

    fn export_buffer(
        &mut self,
        req: &ReadbackRequest,
    ) -> Option<crate::runtime::model::sharing::ExportId> {
        let _ = req;
        None
    }

    fn import_buffer(&mut self, req: &ReadbackRequest) -> Option<u64> {
        let _ = req;
        None
    }

    fn export_texture(
        &mut self,
        req: &ReadbackRequest,
    ) -> Option<crate::runtime::model::sharing::ExportId> {
        let _ = req;
        None
    }

    fn import_texture(&mut self, req: &ReadbackRequest) -> Option<u64> {
        let _ = req;
        None
    }

    fn map_texture(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }
    fn unmap_texture(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }

    fn map_buffer(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }

    fn unmap_buffer(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }

    fn export_sync(&mut self, req: &ReadbackRequest) -> Option<crate::SyncExportId> {
        let _ = req;
        None
    }
    fn import_sync(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }
    fn release_sync(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }
    fn signal_sync(&mut self, req: &ReadbackRequest) -> Option<()> {
        let _ = req;
        None
    }
    fn wait_sync(&mut self, req: &ReadbackRequest) -> Option<crate::TimelineWait> {
        let _ = req;
        None
    }
    fn query_sync(&mut self, req: &ReadbackRequest) -> Option<u64> {
        let _ = req;
        None
    }
}

/// Adapts a bare submit closure into a [`ConnectionHandler`] whose readback half always fails — the
/// back-compat shim behind [`serve_connection`].
struct SubmitHandler<H>(H);

impl<H, V> ConnectionHandler for SubmitHandler<H>
where
    H: FnMut(&SubmitHeader, &[Cmd]) -> V,
    V: Into<Verdict>,
{
    fn submit(&mut self, header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        (self.0)(header, batch).into()
    }
}

/// Contains backend panics to one protocol operation so the connection remains usable.
struct HandlerBoundary;

impl HandlerBoundary {
    /// Run one handler operation across the unwind boundary. On panic, the caller discards the operation's
    /// result and emits a failure response without inspecting potentially partial backend state.
    ///
    /// `Err` carries the panic's own message. A contained panic and a clean rejection produce the SAME
    /// wire byte, so the message is the only thing that can tell them apart afterwards — dropping it (as
    /// this boundary used to) makes a crashing backend indistinguishable from a refused batch.
    fn call<R>(op: impl FnOnce() -> R) -> Result<R, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(op)).map_err(|payload| {
            payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned())
        })
    }
}

/// Once-per-connection latch for the error-level diagnostics on the per-frame serve path. A backend that
/// fails every frame must produce a handful of legible lines, not one per frame at 60 Hz; the first
/// occurrence of each distinct cause is the one that explains the failure.
#[derive(Default)]
struct Reported {
    panicked: bool,
    nacked: bool,
    decode_failed: bool,
    too_large: bool,
    readback_failed: bool,
}

impl Reported {
    /// True the first time `flag` is raised on this connection.
    fn first(flag: &mut bool) -> bool {
        !std::mem::replace(flag, true)
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
    unix::Connection::new(stream).write_handshake(caps)?;
    let mut reported = Reported::default();
    loop {
        let frame = match unix::Connection::new(stream).read_frame_outcome()? {
            FrameOutcome::Frame(f) => f,
            FrameOutcome::Eof => return Ok(()), // peer closed the connection
            // An over-cap frame must NOT tear down the persistent connection (that drops the host's warm
            // per-connection caches AND every subsequent frame — the guest sees `Broken pipe`). Drain the
            // oversized payload to resync the stream, NACK it, and keep serving. A truncated drain means the
            // peer is genuinely gone, which propagates as an error (the connection really did end).
            FrameOutcome::TooLarge(header) => {
                hl_log::hl_count!(hl_log::tag::TRANSPORT, "nacks");
                if Reported::first(&mut reported.too_large) {
                    hl_log::hl_error!(
                        hl_log::tag::TRANSPORT,
                        "over-cap frame refused len={} cap={} surface={} (first on this connection)",
                        header.len,
                        unix::MAX_FRAME_BYTES,
                        header.surface_id
                    );
                }
                unix::Connection::new(stream).drain_payload(header.len)?;
                unix::Connection::new(stream).write_ack(ACK_FAIL)?;
                continue;
            }
        };
        if frame.header.surface_id == READBACK_MAGIC {
            // Device→host readback: decode the fixed request, serve it, and reply with the disjoint
            // length-prefixed response (never the 1-byte submit ack). A panicking readback op is contained
            // and failed rather than allowed to unwind the connection thread.
            let started = std::time::Instant::now();
            let bytes = ReadbackRequest::from_bytes(&frame.payload)
                .filter(|req| req.version == crate::transport::model::readback::READBACK_VERSION)
                .filter(|req| req.kind >= readback_kind::EXPORT_SYNC || (req.authenticity == 0 && req.arg == 0))
                .filter(|req| match req.kind {
                    readback_kind::BUFFER => true,
                    readback_kind::FENCE => req.len == 0,
                    readback_kind::FENCE_WAIT => true,
                    readback_kind::EXPORT_BUFFER => req.offset == 0 && req.len == 0,
                    readback_kind::IMPORT_BUFFER => req.len == 0,
                    readback_kind::EXPORT_TEXTURE => req.offset == 0 && req.len == 0,
                    readback_kind::IMPORT_TEXTURE => req.len == 0,
                    readback_kind::MAP_TEXTURE | readback_kind::UNMAP_TEXTURE => req.offset == 0 && req.len == 0,
                    readback_kind::MAP_BUFFER | readback_kind::UNMAP_BUFFER => {
                        req.offset == 0 && req.len == 0
                    }
                    readback_kind::EXPORT_SYNC => req.id == 0 && req.offset == 0 && req.authenticity == 0 && req.arg == 0,
                    readback_kind::IMPORT_SYNC | readback_kind::RELEASE_SYNC => req.id == 0 && req.len == 0 && req.arg == 0,
                    readback_kind::SIGNAL_SYNC => req.id == 0 && req.arg == 0,
                    readback_kind::WAIT_SYNC => req.id == 0,
                    readback_kind::QUERY_SYNC => req.id == 0 && req.len == 0 && req.arg == 0,
                    _ => false,
                })
                .and_then(|req| {
                let kind = req.kind;
                match HandlerBoundary::call(|| match req.kind {
                    readback_kind::BUFFER => handler.read_buffer(&req),
                    readback_kind::FENCE => handler.poll_fence(&req).map(|done| vec![done as u8]),
                    readback_kind::FENCE_WAIT => handler
                        .wait_fence(&req)
                        .map(|status| vec![matches!(status, crate::FenceWait::Complete) as u8]),
                    readback_kind::EXPORT_BUFFER => {
                        handler.export_buffer(&req).map(|id| id.0.to_le_bytes().to_vec())
                    }
                    readback_kind::IMPORT_BUFFER => handler
                        .import_buffer(&req)
                        .map(|bytes| bytes.to_le_bytes().to_vec()),
                    readback_kind::EXPORT_TEXTURE => handler
                        .export_texture(&req).map(|id| id.0.to_le_bytes().to_vec()),
                    readback_kind::IMPORT_TEXTURE => handler
                        .import_texture(&req).map(|bytes| bytes.to_le_bytes().to_vec()),
                    readback_kind::MAP_TEXTURE => handler.map_texture(&req).map(|()| Vec::new()),
                    readback_kind::UNMAP_TEXTURE => handler.unmap_texture(&req).map(|()| Vec::new()),
                    readback_kind::MAP_BUFFER => handler.map_buffer(&req).map(|()| Vec::new()),
                    readback_kind::UNMAP_BUFFER => handler.unmap_buffer(&req).map(|()| Vec::new()),
                    readback_kind::EXPORT_SYNC => handler.export_sync(&req).map(|id| {
                        let mut bytes = Vec::with_capacity(24);
                        bytes.extend_from_slice(&id.serial().to_le_bytes());
                        bytes.extend_from_slice(&id.authenticity().to_le_bytes());
                        bytes
                    }),
                    readback_kind::IMPORT_SYNC => handler.import_sync(&req).map(|()| Vec::new()),
                    readback_kind::RELEASE_SYNC => handler.release_sync(&req).map(|()| Vec::new()),
                    readback_kind::SIGNAL_SYNC => handler.signal_sync(&req).map(|()| Vec::new()),
                    readback_kind::WAIT_SYNC => handler.wait_sync(&req).map(|status| vec![matches!(status, crate::TimelineWait::Reached) as u8]),
                    readback_kind::QUERY_SYNC => handler.query_sync(&req).map(|value| value.to_le_bytes().to_vec()),
                    _ => None,
                }) {
                    Ok(bytes) => bytes,
                    Err(panic) => {
                        // A contained panic and a refused readback both answer READBACK_FAIL. Say which.
                        if Reported::first(&mut reported.panicked) {
                            hl_log::hl_error!(
                                hl_log::tag::TRANSPORT,
                                "readback handler PANICKED kind={} (readback failed, connection kept): {}",
                                kind,
                                panic
                            );
                        }
                        None
                    }
                }
            });
            // A readback the handler refused is a failure the guest sees as a rejected request with no
            // host-side explanation, so name it once per connection.
            if bytes.is_none() && Reported::first(&mut reported.readback_failed) {
                hl_log::hl_error!(
                    hl_log::tag::TRANSPORT,
                    "readback refused request_bytes={} (first on this connection)",
                    frame.payload.len()
                );
            }
            hl_log::hl_debug!(
                hl_log::tag::TRANSPORT,
                "readback complete request_bytes={} response_bytes={} elapsed_us={} ok={}",
                frame.payload.len(),
                bytes.as_ref().map_or(0, Vec::len),
                started.elapsed().as_micros(),
                bytes.is_some()
            );
            match bytes {
                Some(bytes) => {
                    unix::Connection::new(stream).write_readback_response(READBACK_OK, &bytes)?
                }
                None => {
                    unix::Connection::new(stream).write_readback_response(READBACK_FAIL, &[])?
                }
            }
            continue;
        }
        // Decode + execute one submit. A malformed payload is NACKed at the boundary (never handed to the
        // handler); a handler that PANICS on some op is caught and NACKed rather than allowed to unwind the
        // connection thread (which would drop the socket = `Broken pipe` for every later frame). Either way
        // the connection stays alive and serves the next frame.
        let diagnostics = hl_log::Logging::global().enabled(
            hl_log::Tags::from(hl_log::tag::TRANSPORT),
            hl_log::Level::Debug,
        );
        let total_started = diagnostics.then(Instant::now);
        let decode_started = diagnostics.then(Instant::now);
        let (verdict, commands, decode_us, handler_us) =
            match crate::protocol::codec::Decoder::stream(&frame.payload) {
                Ok(batch) => {
                    let decode_us = decode_started
                        .map(|started| started.elapsed().as_micros())
                        .unwrap_or_default();
                    let handler_started = diagnostics.then(Instant::now);
                    let verdict =
                        match HandlerBoundary::call(|| handler.submit(&frame.header, &batch)) {
                            Ok(verdict) => verdict,
                            Err(panic) => {
                                // Both outcomes write ACK_FAIL, so without this line a crashing backend looks
                                // exactly like a cleanly-refused batch and the panic message is lost entirely.
                                if Reported::first(&mut reported.panicked) {
                                    hl_log::hl_error!(
                                        hl_log::tag::TRANSPORT,
                                        "submit handler PANICKED surface={} commands={} bytes={} \
                                     (frame NACKed, connection kept): {}",
                                        frame.header.surface_id,
                                        batch.len(),
                                        frame.payload.len(),
                                        panic
                                    );
                                }
                                Verdict::Nack
                            }
                        };
                    let handler_us = handler_started
                        .map(|started| started.elapsed().as_micros())
                        .unwrap_or_default();
                    (verdict, batch.len(), decode_us, handler_us)
                }
                Err(error) => {
                    let decode_us = decode_started
                        .map(|started| started.elapsed().as_micros())
                        .unwrap_or_default();
                    // A payload this side cannot decode is a protocol violation by the guest; the frame is
                    // NACKed without ever reaching the handler. `error` names the command index, byte
                    // offset and tag, which is the whole diagnosis.
                    if Reported::first(&mut reported.decode_failed) {
                        hl_log::hl_error!(
                            hl_log::tag::WIRE,
                            "frame decode failed bytes={} surface={} (first on this connection): {}",
                            frame.payload.len(),
                            frame.header.surface_id,
                            error
                        );
                    }
                    (Verdict::Nack, 0, decode_us, 0)
                }
            };
        if matches!(verdict, Verdict::Refused(_) | Verdict::Partial { .. }) {
            hl_log::hl_count!(hl_log::tag::TRANSPORT, "nacks");
            if matches!(verdict, Verdict::Partial { .. }) {
                hl_log::hl_count!(hl_log::tag::TRANSPORT, "partial_refusals");
            }
            // The guest turns this ack into DEVICE_LOST. It is the host's only chance to say a frame was
            // refused; the counter alone is invisible in a shipped build. Latched: a backend that refuses
            // every frame prints once, not at frame rate.
            if Reported::first(&mut reported.nacked) {
                hl_log::hl_error!(
                    hl_log::tag::TRANSPORT,
                    "frame REFUSED surface={} commands={} bytes={} partial={} (first on this connection)",
                    frame.header.surface_id,
                    commands,
                    frame.payload.len(),
                    matches!(verdict, Verdict::Partial { .. })
                );
            }
        }
        let ack = verdict.ack_byte();
        let ack_started = diagnostics.then(Instant::now);
        unix::Connection::new(stream).write_ack(ack)?;
        if let Verdict::Partial { commands, replayable, .. } = &verdict {
            unix::Connection::new(stream).write_partial_delta(commands, *replayable)?;
        }
        let ack_write_us = ack_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        hl_log::hl_debug!(
            hl_log::tag::TRANSPORT,
            "frame_complete payload_bytes={} commands={} decode_us={} handler_us={} ack_write_us={} total_us={} ack={}",
            frame.payload.len(),
            commands,
            decode_us,
            handler_us,
            ack_write_us,
            total_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default(),
            ack
        );
    }
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
    serve_loop(stream, caps, &mut SubmitHandler(handler))
}

/// Serve one connection to completion driving a [`ConnectionHandler`], which serves BOTH submit and the
/// additive device→host readback path. Otherwise identical to [`serve_connection`]. This call owns the
/// protocol writer for `stream` until it returns and serializes every response in request order; callers
/// must not write protocol bytes through another clone of the same socket concurrently.
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
