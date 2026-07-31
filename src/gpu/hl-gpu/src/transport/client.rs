//! [`RemoteCommandSink`] — a [`CommandSink`] that encodes protocol batches and writes them, framed, over
//! the Unix adapter. It owns connect/reconnect, the residency journal + replay-on-reconnect, and the
//! capability handshake driving. Ported from `hl-shim`'s `ExecConn` + `negotiate_host_capabilities`.
//!
//! Layering: this is the guest-side realization of the `CommandSink` port. It executes nothing and knows
//! no GPU semantics — it encodes via `protocol::codec`, frames via [`crate::transport::model`], and moves
//! bytes via [`crate::transport::adapter::unix`]. The 16-byte submit header + 1-byte ack framing is
//! transport-private and byte-identical to the shipped guest/host.
//!
//! One sink is the sole writer for its persistent stream: every request and response is serialized through
//! [`CommandSink`]'s `&mut self` methods. The sink is deliberately `!Sync`; use ownership transfer or a
//! mutex when a higher layer must coordinate access instead of writing concurrently through socket clones.

use std::cell::Cell;
use std::collections::HashSet;
use std::io;
use std::marker::PhantomData;
use std::os::unix::net::UnixStream;

use crate::protocol::model::capability::{Capabilities, FeatureRequest};
use crate::protocol::model::command::{Cmd, Enc};
use crate::protocol::model::descriptor::BindResource;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId};
use crate::protocol::port::sink::CommandSink;
use crate::runtime::model::resources::{
    KIND_BIND_GROUP, KIND_BUFFER, KIND_FENCE, KIND_PIPELINE, KIND_SAMPLER, KIND_SHADER,
    KIND_SURFACE, KIND_TEXTURE,
};

mod residency;
use residency::ResidencyJournal;
mod failure;

#[cfg(test)]
mod tests;
use crate::transport::adapter::unix;
use crate::transport::model::abi::Surface;
use crate::transport::model::header::{SubmitHeader, ACK_OK};
use crate::transport::model::readback::ReadbackRequest;
use crate::transport::model::{
    config::TransportConfig,
    error::{TransportError, TransportPhase},
};

const MAX_REPLAY_BYTES: usize = 64 << 20;

/// Commands acknowledged by the current executor and therefore required to reconstruct the next executor.
/// Keeping the ordered command history is deliberate: uploads and GPU copies/draws can mutate resources,
/// so a create-only cache is not authoritative. Presents and waits are observations, not residency, and
/// are never repeated.
pub struct RemoteCommandSink {
    path: String,
    sock: Option<UnixStream>,
    connects: u64,
    residency_reset: bool,
    generation: u64,
    submits: u64,
    residency: ResidencyJournal,
    /// Signature (handshake bytes) of the last capability descriptor read off the connection. A changed
    /// signature while objects are resident cannot be recovered and is reported as API loss.
    negotiated_capabilities: Option<Vec<u8>>,
    /// The most recent decoded host advertisement, returned by [`CommandSink::negotiate`].
    host_caps: Option<Capabilities>,
    /// The surface the submit header names (which output the host presents to).
    surface: Surface,
    /// Emit transport-boundary diagnostics to stderr. Applications and shims opt in explicitly so this
    /// reusable package never reads process environment or owns logging policy.
    trace: bool,
    config: TransportConfig,
    terminal: Option<TransportError>,
    /// Error-level diagnostics already emitted for the CURRENT connection generation, so a persistent
    /// failure on the per-frame submit path prints a handful of legible lines rather than one per frame.
    /// Keyed `(class, code)`; cleared whenever `generation` advances, because a fresh executor is a fresh
    /// story worth telling again.
    reported: HashSet<(u8, u8)>,
    reported_generation: u64,
    _single_writer: PhantomData<Cell<()>>,
}

/// `reported` classes.
const REPORT_NACK: u8 = 0;
const REPORT_RETRIES: u8 = 1;
const REPORT_TERMINAL: u8 = 2;

impl RemoteCommandSink {
    pub fn new(path: impl Into<String>) -> Self {
        RemoteCommandSink {
            path: path.into(),
            sock: None,
            connects: 0,
            residency_reset: false,
            generation: 0,
            submits: 0,
            residency: ResidencyJournal::default(),
            negotiated_capabilities: None,
            host_caps: None,
            surface: Surface::default(),
            trace: false,
            config: TransportConfig::default(),
            terminal: None,
            reported: HashSet::new(),
            reported_generation: 0,
            _single_writer: PhantomData,
        }
    }

    pub fn with_config(path: impl Into<String>, config: TransportConfig) -> Self {
        Self {
            config,
            ..Self::new(path)
        }
    }

    /// Connect to `path` targeting `surface` (the output the submit header names).
    pub fn with_surface(path: impl Into<String>, surface: Surface) -> Self {
        let mut s = Self::new(path);
        s.surface = surface;
        s
    }

    pub fn with_surface_config(
        path: impl Into<String>,
        surface: Surface,
        config: TransportConfig,
    ) -> Self {
        let mut sink = Self::with_config(path, config);
        sink.surface = surface;
        sink
    }

    /// Set the surface the submit header will name for subsequent submits.
    pub fn set_surface(&mut self, surface: Surface) {
        self.surface = surface;
    }

    /// Enable or disable synchronous transport diagnostics.
    pub fn set_trace(&mut self, trace: bool) {
        self.trace = trace;
    }

    /// The surface the submit header currently names.
    pub fn surface(&self) -> Surface {
        self.surface
    }

    /// Total successful connects over this channel's life; should be 1 for a healthy run.
    pub fn connects(&self) -> u64 {
        self.connects
    }

    /// Monotonic executor generation. It advances only after a successful socket connection.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Compatibility observer for callers predating internal replay. Successful reconnect recovery
    /// consumes this flag before `submit` returns, so producers normally observe `false`.
    pub fn take_residency_reset(&mut self) -> bool {
        core::mem::replace(&mut self.residency_reset, false)
    }

    /// Pin the negotiated backend profile to this connection. A changed profile while objects are resident
    /// cannot be recovered safely and is reported as API loss instead of replaying commands under
    /// different wire/shader/format semantics.
    pub fn set_negotiated_capabilities(&mut self, caps: &Capabilities) -> Result<()> {
        let signature = caps.to_handshake();
        if self
            .negotiated_capabilities
            .as_ref()
            .is_some_and(|old| old != &signature)
            && !self.residency.cmds.is_empty()
        {
            return Err(self.lose_api("executor capabilities changed with live residency"));
        }
        self.negotiated_capabilities = Some(signature);
        self.host_caps = Some(caps.clone());
        Ok(())
    }

    /// Ensure the socket is connected, reading + pinning the host's capability handshake on any fresh
    /// connection. A RE-connect (not the first) means a fresh host backend with an EMPTY resource cache,
    /// so the residency-reset flag is raised for the next submit to replay against.
    fn ensure(&mut self) -> Result<()> {
        if let Some(error) = &self.terminal {
            return Err(GpuError::Transport(TransportError::Poisoned {
                cause: error.to_string(),
            }));
        }
        if self.sock.is_some() {
            return Ok(());
        }
        if self.trace {
            eprintln!(
                "hl-gpu transport: connect begin path={} generation={}",
                self.path, self.generation
            );
        }
        let s = unix::Connection::connect(
            std::path::Path::new(&self.path),
            self.config.connect_timeout(),
        )
        .map_err(|error| {
            GpuError::Transport(Self::before_request(TransportPhase::Connect, error))
        })?;
        s.set_read_timeout(Some(self.config.handshake_timeout()))
            .map_err(|error| {
                GpuError::Transport(Self::unavailable(TransportPhase::Handshake, error))
            })?;
        s.set_write_timeout(Some(self.config.write_timeout()))
            .map_err(|error| {
                GpuError::Transport(Self::unavailable(TransportPhase::FrameWrite, error))
            })?;
        if self.trace {
            eprintln!(
                "hl-gpu transport: connect complete path={} generation={}",
                self.path, self.generation
            );
        }
        // The host advertises its capabilities first thing on every connection; read + pin them, which
        // also detects an incompatible profile change across a reconnect that has live residency.
        if self.trace {
            eprintln!("hl-gpu transport: handshake read begin path={}", self.path);
        }
        let caps = unix::Connection::new(&s)
            .read_handshake()
            .map_err(|error| {
                GpuError::Transport(Self::before_request(TransportPhase::Handshake, error))
            })?;
        if self.trace {
            eprintln!(
                "hl-gpu transport: handshake read complete path={}",
                self.path
            );
        }
        self.set_negotiated_capabilities(&caps)?;
        s.set_read_timeout(Some(self.config.response_timeout()))
            .map_err(|error| {
                GpuError::Transport(Self::unavailable(TransportPhase::ResponseRead, error))
            })?;
        if self.connects >= 1 {
            self.residency_reset = true;
        }
        self.connects += 1;
        self.generation += 1;
        self.sock = Some(s);
        Ok(())
    }

    /// Submit one already-encoded frame's IR (`ir`) whose decoded form is `current`, and block until the
    /// host acks the render. `current` drives residency recording without re-decoding the wire bytes.
    ///
    /// Wire (byte-identical to `gl_shim.c` `exec_stream` and the host executor's reader): a 16-byte
    /// little-endian header `[surface.id, surface.width, surface.height, payload.len()]` followed by the
    /// payload bytes; the host replies with a single ack byte. A failure is retried only when no request byte
    /// reached the peer. Any later failure has an ambiguous outcome and permanently retires this sink.
    fn submit_ir(&mut self, ir: &[u8], current: &[Cmd], encode_us: u128) -> Result<()> {
        let diagnostics = self.diagnostics();
        self.submits = self.submits.saturating_add(1);
        let submit = self.submits;
        let mut last_error = None;
        for _ in 0..2 {
            let submit_started = diagnostics.then(std::time::Instant::now);
            if let Err(error) = self.ensure() {
                if !matches!(
                    &error,
                    GpuError::Transport(transport) if transport.retryable_before_request()
                ) {
                    return Err(error);
                }
                last_error = Some(error);
                self.sock = None;
                continue;
            }
            let mut payload = if self.residency_reset {
                match self.residency.replay_bytes() {
                    Ok(payload) => payload,
                    Err(error) => return Err(self.lose(error)),
                }
            } else {
                Vec::new()
            };
            payload.extend_from_slice(ir);
            let peer_closed = {
                let socket = self.sock.as_ref().expect("ensure installed socket");
                unix::Connection::new(socket).peer_closed()
            };
            match peer_closed {
                Ok(true) => {
                    self.sock = None;
                    last_error = Some(GpuError::Transport(TransportError::Unavailable {
                        phase: TransportPhase::FrameWrite,
                        detail: "peer closed before request".into(),
                    }));
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.sock = None;
                    last_error = Some(GpuError::Transport(Self::before_request(
                        TransportPhase::FrameWrite,
                        error,
                    )));
                    continue;
                }
            }
            let header = SubmitHeader::for_frame(&self.surface, payload.len() as u32);
            let write_started = diagnostics.then(std::time::Instant::now);
            let write = {
                let socket = self.sock.as_ref().expect("ensure installed socket");
                unix::Connection::new(socket).write_frame_tracked(&header, &payload)
            };
            let write_us = write_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default();
            if let Err(failure) = write {
                if failure.accepted != 0 {
                    return Err(self.ambiguous(TransportPhase::FrameWrite, failure.error));
                }
                let error = GpuError::Transport(Self::before_request(
                    TransportPhase::FrameWrite,
                    failure.error,
                ));
                self.sock = None;
                last_error = Some(error);
                continue;
            }
            let ack_started = diagnostics.then(std::time::Instant::now);
            let ack = {
                let socket = self.sock.as_ref().expect("frame socket remains installed");
                unix::Connection::new(socket).read_ack()
            };
            let ack = match ack {
                Ok(ack) => ack,
                Err(error) => {
                    return Err(self.ambiguous(TransportPhase::Acknowledgement, error));
                }
            };
            let ack_us = ack_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default();
            let total_us = submit_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default();
            if diagnostics {
                hl_log::hl_debug!(
                    hl_log::tag::TRANSPORT,
                    "gpu transport submit={} generation={} commands={} bytes={} replay_bytes={} \
                     encode_us={} write_us={} ack_us={} total_us={} ack={}",
                    submit,
                    self.generation,
                    current.len(),
                    payload.len(),
                    payload.len().saturating_sub(ir.len()),
                    encode_us,
                    write_us,
                    ack_us,
                    total_us,
                    ack
                );
            }
            match ack {
                ACK_OK => {
                    self.residency_reset = false;
                    self.residency.append(current);
                    return Ok(());
                }
                // The executor NACKed this frame (replay failed / surface missing). Surface it as an error
                // rather than letting the guest commit a stale or partly-rendered frame as if it presented.
                nack => {
                    hl_log::hl_count!(hl_log::tag::TRANSPORT, "nacks");
                    // The single most explanatory line available for a guest-visible DEVICE_LOST: the
                    // host ran the frame and refused it. Latched per distinct ack per generation.
                    if self.first_report(REPORT_NACK, nack) {
                        hl_log::hl_error!(
                            hl_log::tag::TRANSPORT,
                            "host executor REJECTED frame ack={} submit={} generation={} surface={} \
                             commands={} bytes={} (first for this ack on this connection)",
                            nack,
                            submit,
                            self.generation,
                            self.surface.id,
                            current.len(),
                            ir.len()
                        );
                    }
                    return Err(GpuError::Transport(TransportError::Rejected {
                        phase: TransportPhase::Acknowledgement,
                        acknowledgement: nack,
                    }));
                }
            }
        }
        let failure = last_error.unwrap_or_else(|| {
            GpuError::Transport(TransportError::Unavailable {
                phase: TransportPhase::Connect,
                detail: "connection attempts exhausted".into(),
            })
        });
        // The frame is gone and the sink is NOT retired, so `lose` will not speak for it. Without this the
        // guest sees a failed present with nothing on either side of the socket explaining it.
        if self.first_report(REPORT_RETRIES, 0) {
            hl_log::hl_error!(
                hl_log::tag::TRANSPORT,
                "gpu transport submit FAILED after retries path={} submit={} generation={} bytes={}: {}",
                self.path,
                submit,
                self.generation,
                ir.len(),
                failure
            );
        }
        Err(failure)
    }
}

impl CommandSink for RemoteCommandSink {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        // Read (if not already) the host's advertised capabilities off the connection, then check the
        // guest's required features against them BEFORE advertising any matching API feature to the app.
        self.ensure()?;
        let caps = self
            .host_caps
            .clone()
            .expect("ensure pinned host capabilities");
        caps.negotiate(request)?;
        Ok(caps)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        let diagnostics = self.diagnostics();
        let encode_started = diagnostics.then(std::time::Instant::now);
        let ir = crate::protocol::codec::Encoder::stream(batch);
        let encode_us = encode_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        self.submit_ir(&ir, batch, encode_us)
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(1);
        loop {
            match self.wait_timeout(fence, value, WAIT_SLICE.as_nanos() as u64)? {
                crate::FenceWait::Complete => return Ok(()),
                crate::FenceWait::Timeout => {}
            }
        }
    }

    fn wait_timeout(
        &mut self,
        fence: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> Result<crate::FenceWait> {
        if timeout_ns == u64::MAX {
            return self.wait(fence, value).map(|()| crate::FenceWait::Complete);
        }
        let request = ReadbackRequest::fence_wait(fence.raw(), value, timeout_ns);
        let response_timeout = self
            .config
            .response_timeout()
            .checked_add(std::time::Duration::from_nanos(timeout_ns))
            .unwrap_or(std::time::Duration::MAX);
        let response = self.request(&request, 1, response_timeout)?;
        match response.as_slice() {
            [0] => Ok(crate::FenceWait::Timeout),
            [1] => Ok(crate::FenceWait::Complete),
            _ => Err(self.protocol(TransportPhase::ResponseRead, "invalid fence wait response")),
        }
    }

    fn poll_fence(&mut self, fence: FenceId, value: u64) -> Result<bool> {
        let request = ReadbackRequest::fence(fence.raw(), value);
        let response = self.request(&request, 1, self.config.response_timeout())?;
        match response.as_slice() {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(self.protocol(TransportPhase::ResponseRead, "invalid fence poll response")),
        }
    }

    /// Read `len` bytes of buffer `id` back from the host executor over the wire (the socketed
    /// `cuMemcpyDtoH` / `glReadPixels` path). Sends a readback-magic REQUEST frame — disjoint from a submit,
    /// so it never collides on the wire — and reads the host's length-prefixed byte response.
    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let diagnostics = self.diagnostics();
        let readback_started = diagnostics.then(std::time::Instant::now);
        let req = ReadbackRequest::buffer(id.raw(), offset, len as u64);
        let write_started = diagnostics.then(std::time::Instant::now);
        let response_started = diagnostics.then(std::time::Instant::now);
        let bytes = self.request(&req, len, self.config.response_timeout())?;
        let write_us = write_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        let response_us = response_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        let total_us = readback_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        if diagnostics {
            hl_log::hl_debug!(
                hl_log::tag::TRANSPORT,
                "gpu transport readback generation={} requested={} received={} write_us={} \
                 response_us={} total_us={}",
                self.generation,
                len,
                bytes.len(),
                write_us,
                response_us,
                total_us
            );
        }
        if self.trace {
            eprintln!(
                "hl-gpu transport: readback complete generation={} requested={} received={} write_us={} response_us={} total_us={}",
                self.generation,
                len,
                bytes.len(),
                write_us,
                response_us,
                total_us
            );
        }
        Ok(bytes)
    }
}
