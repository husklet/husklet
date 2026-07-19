//! [`RemoteCommandSink`] — a [`CommandSink`] that encodes protocol batches and writes them, framed, over
//! the Unix adapter. It owns connect/reconnect, the residency journal + replay-on-reconnect, and the
//! capability handshake driving. Ported from `hl-shim`'s `ExecConn` + `negotiate_host_capabilities`.
//!
//! Layering: this is the guest-side realization of the `CommandSink` port. It executes nothing and knows
//! no GPU semantics — it encodes via `protocol::codec`, frames via [`crate::transport::model`], and moves
//! bytes via [`crate::transport::adapter::unix`]. The 16-byte submit header + 1-byte ack framing is
//! transport-private and byte-identical to the shipped guest/host.

use std::collections::HashSet;
use std::io::{self, ErrorKind};
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
use crate::transport::adapter::unix;
use crate::transport::model::abi::{Surface, DEFAULT_EXEC_SOCK};
use crate::transport::model::header::{SubmitHeader, ACK_OK};
use crate::transport::model::readback::ReadbackRequest;

const MAX_REPLAY_BYTES: usize = 64 << 20;

/// Commands acknowledged by the current executor and therefore required to reconstruct the next executor.
/// Keeping the ordered command history is deliberate: uploads and GPU copies/draws can mutate resources,
/// so a create-only cache is not authoritative. Presents and waits are observations, not residency, and
/// are never repeated.
struct ResidencyJournal {
    cmds: Vec<Cmd>,
    bytes: usize,
    replayable: bool,
    /// Maximum encoded residency the channel will replay on reconnect. Past this the journal drops
    /// `replayable` and a reconnect reports a clean API loss instead of silently recovering a truncated
    /// resource set. Configurable so the over-budget transition is testable without a multi-MB fixture.
    max_bytes: usize,
}

impl Default for ResidencyJournal {
    fn default() -> Self {
        Self {
            cmds: Vec::new(),
            bytes: 0,
            replayable: false,
            max_bytes: MAX_REPLAY_BYTES,
        }
    }
}

impl ResidencyJournal {
    #[cfg(test)]
    fn with_budget(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            ..Self::default()
        }
    }

    fn append(&mut self, cmds: &[Cmd]) {
        if !self.replayable && !self.cmds.is_empty() {
            return;
        }
        let mut saw_destroy = false;
        for cmd in cmds {
            if matches!(cmd, Cmd::Present { .. } | Cmd::WaitFence { .. }) {
                continue;
            }
            if cmd.is_destroy() {
                saw_destroy = true;
            }
            let encoded = crate::protocol::codec::Encoder::stream(core::slice::from_ref(cmd));
            self.bytes = self.bytes.saturating_add(encoded.len());
            self.cmds.push(cmd.clone());
        }
        // A frame that FREED residency (a Destroy*) is the moment a create/destroy pair can retire from the
        // journal. Compacting here keeps the journal tracking only LIVE residency — critical for a
        // lost-context client (Chrome tears a context down, its whole abandoned working set is Destroy*d, then
        // it recreates the set with FRESH ids): without compaction a reconnect would replay every DEAD
        // resource's create, re-inflating the host ledger with each retry. With it, only live residency
        // replays, and the journal stays bounded so a healthy churny client never falsely trips the replay
        // budget below.
        if saw_destroy {
            self.compact();
        }
        // Re-evaluate the replay budget against the (compacted) LIVE residency, not the cumulative history.
        self.replayable = self.bytes <= self.max_bytes;
    }

    /// Drop every journal command that references ONLY resources which have been both created AND destroyed
    /// within the journal (a fully-retired working set), leaving the journal replaying exactly the LIVE
    /// residency. Correct by a fixpoint: a command that references any still-live id is retained in full, and
    /// any id a retained command references is promoted back out of the dead set — so after convergence no
    /// retained command references a dropped id, and every dropped command references only dropped ids. Ids
    /// are keyed by `(kind, id)` so a buffer and a texture sharing a numeric id are never confused.
    fn compact(&mut self) {
        let mut created: HashSet<(u8, u32)> = HashSet::new();
        let mut destroyed: HashSet<(u8, u32)> = HashSet::new();
        for cmd in &self.cmds {
            if let Some(key) = cmd.created_key() {
                created.insert(key);
            }
            if let Some(key) = cmd.destroyed_key() {
                destroyed.insert(key);
            }
        }
        // Candidate-dead: created AND destroyed in this journal. Anything created-but-not-destroyed (still
        // live) is never a candidate, so its create/uploads/submits are always kept.
        let mut dead: HashSet<(u8, u32)> = created.intersection(&destroyed).copied().collect();
        if dead.is_empty() {
            return;
        }
        // Fixpoint: any command that references a still-LIVE id is retained; the ids such a command touches
        // are "needed" and must be evicted from `dead` (they cannot be dropped without breaking that retained
        // command's replay). Iterate until `dead` stops shrinking.
        loop {
            let mut evict: Vec<(u8, u32)> = Vec::new();
            for cmd in &self.cmds {
                let refs = cmd.resource_refs();
                if refs.is_empty() {
                    continue;
                }
                let all_dead = refs.iter().all(|k| dead.contains(k));
                if !all_dead {
                    // A retained command — everything it touches must survive.
                    for k in refs {
                        if dead.contains(&k) {
                            evict.push(k);
                        }
                    }
                }
            }
            if evict.is_empty() {
                break;
            }
            for k in evict {
                dead.remove(&k);
            }
        }
        // Keep a command unless every id it references is dead (a command with no ids — none are journaled
        // today, but be safe — is always kept).
        let kept: Vec<Cmd> = self
            .cmds
            .drain(..)
            .filter(|cmd| {
                let refs = cmd.resource_refs();
                refs.is_empty() || !refs.iter().all(|k| dead.contains(k))
            })
            .collect();
        self.cmds = kept;
        self.bytes = crate::protocol::codec::Encoder::stream(&self.cmds).len();
    }

    fn replay_bytes(&self) -> io::Result<Vec<u8>> {
        if !self.replayable && !self.cmds.is_empty() {
            return Err(RemoteCommandSink::api_loss(
                "executor residency exceeded replay budget",
            ));
        }
        Ok(crate::protocol::codec::Encoder::stream(&self.cmds))
    }
}

/// Whether `cmd` frees a resource (a `Destroy*`), the signal to compact the journal.
impl Cmd {
    fn is_destroy(&self) -> bool {
        matches!(
            self,
            Cmd::DestroyBuffer(_)
                | Cmd::DestroyTexture(_)
                | Cmd::DestroySampler(_)
                | Cmd::DestroyShader(_)
                | Cmd::DestroyPipeline(_)
                | Cmd::DestroyBindGroup(_)
                | Cmd::DestroySurface(_)
                | Cmd::DestroyFence(_)
        )
    }

    /// The `(kind, id)` a `Create*` introduces, or `None`.
    fn created_key(&self) -> Option<(u8, u32)> {
        Some(match self {
            Cmd::CreateBuffer(id, _) => (KIND_BUFFER, *id),
            Cmd::CreateTexture(id, _) => (KIND_TEXTURE, *id),
            Cmd::CreateSampler(id, _) => (KIND_SAMPLER, *id),
            Cmd::CreateShader { id, .. } => (KIND_SHADER, *id),
            Cmd::CreateRenderPipeline(id, _) | Cmd::CreateComputePipeline(id, _) => {
                (KIND_PIPELINE, *id)
            }
            Cmd::CreateBindGroup(id, _) => (KIND_BIND_GROUP, *id),
            Cmd::CreateSurface(id, _) => (KIND_SURFACE, *id),
            Cmd::CreateFence(id) => (KIND_FENCE, *id),
            _ => return None,
        })
    }

    /// The `(kind, id)` a `Destroy*` releases, or `None`.
    fn destroyed_key(&self) -> Option<(u8, u32)> {
        Some(match self {
            Cmd::DestroyBuffer(id) => (KIND_BUFFER, *id),
            Cmd::DestroyTexture(id) => (KIND_TEXTURE, *id),
            Cmd::DestroySampler(id) => (KIND_SAMPLER, *id),
            Cmd::DestroyShader(id) => (KIND_SHADER, *id),
            Cmd::DestroyPipeline(id) => (KIND_PIPELINE, *id),
            Cmd::DestroyBindGroup(id) => (KIND_BIND_GROUP, *id),
            Cmd::DestroySurface(id) => (KIND_SURFACE, *id),
            Cmd::DestroyFence(id) => (KIND_FENCE, *id),
            _ => return None,
        })
    }

    /// Every resource `(kind, id)` a journaled command references — the id it creates/destroys plus every id it
    /// DEPENDS on (a pipeline's shader modules, a bind group's buffers/textures/samplers, a submit's bound
    /// pipeline/groups/buffers, its render-pass attachment textures, copy/blit sources+destinations, and a
    /// signalled fence). Used by [`ResidencyJournal::compact`] to decide, safely, when a create/destroy pair is
    /// fully retired and can leave the journal.
    fn resource_refs(&self) -> Vec<(u8, u32)> {
        let mut refs: Vec<(u8, u32)> = Vec::new();
        match self {
            Cmd::CreateBuffer(id, _) | Cmd::DestroyBuffer(id) => refs.push((KIND_BUFFER, *id)),
            Cmd::WriteBuffer { id, .. } => refs.push((KIND_BUFFER, *id)),
            Cmd::CreateTexture(id, _) | Cmd::DestroyTexture(id) => refs.push((KIND_TEXTURE, *id)),
            Cmd::CreateSampler(id, _) | Cmd::DestroySampler(id) => refs.push((KIND_SAMPLER, *id)),
            Cmd::CreateShader { id, .. } | Cmd::DestroyShader(id) => refs.push((KIND_SHADER, *id)),
            Cmd::CreateRenderPipeline(id, d) => {
                refs.push((KIND_PIPELINE, *id));
                refs.push((KIND_SHADER, d.vertex.module));
                if let Some(fs) = &d.fragment {
                    refs.push((KIND_SHADER, fs.module));
                }
            }
            Cmd::CreateComputePipeline(id, d) => {
                refs.push((KIND_PIPELINE, *id));
                refs.push((KIND_SHADER, d.compute.module));
            }
            Cmd::DestroyPipeline(id) => refs.push((KIND_PIPELINE, *id)),
            Cmd::CreateBindGroup(id, d) => {
                refs.push((KIND_BIND_GROUP, *id));
                for e in &d.entries {
                    match e.resource {
                        BindResource::Buffer { id, .. } => refs.push((KIND_BUFFER, id)),
                        BindResource::Texture { id } => refs.push((KIND_TEXTURE, id)),
                        BindResource::Sampler { id } => refs.push((KIND_SAMPLER, id)),
                    }
                }
            }
            Cmd::DestroyBindGroup(id) => refs.push((KIND_BIND_GROUP, *id)),
            Cmd::CreateSurface(id, _) | Cmd::DestroySurface(id) => refs.push((KIND_SURFACE, *id)),
            Cmd::CreateFence(id) | Cmd::DestroyFence(id) => refs.push((KIND_FENCE, *id)),
            Cmd::Submit(cb) => {
                for enc in &cb.encoder {
                    match enc {
                        Enc::SetPipeline(p) => refs.push((KIND_PIPELINE, *p)),
                        Enc::SetBindGroup { group, .. } => refs.push((KIND_BIND_GROUP, *group)),
                        Enc::SetVertexBuffer { buffer, .. }
                        | Enc::SetIndexBuffer { buffer, .. } => refs.push((KIND_BUFFER, *buffer)),
                        Enc::ClearRect { texture, .. } => refs.push((KIND_TEXTURE, *texture)),
                        Enc::BeginRenderPass { color, depth } => {
                            for c in color {
                                refs.push((KIND_TEXTURE, c.texture));
                            }
                            if let Some(d) = depth {
                                refs.push((KIND_TEXTURE, d.texture));
                            }
                        }
                        Enc::CopyBufferToBuffer { src, dst, .. } => {
                            refs.push((KIND_BUFFER, *src));
                            refs.push((KIND_BUFFER, *dst));
                        }
                        Enc::CopyBufferToTexture { src, dst, .. } => {
                            refs.push((KIND_BUFFER, *src));
                            refs.push((KIND_TEXTURE, *dst));
                        }
                        Enc::CopyTextureToBuffer { src, dst, .. } => {
                            refs.push((KIND_TEXTURE, *src));
                            refs.push((KIND_BUFFER, *dst));
                        }
                        Enc::CopyTextureToTexture { src, dst, .. }
                        | Enc::BlitTexture { src, dst, .. }
                        | Enc::ResolveTexture { src, dst, .. } => {
                            refs.push((KIND_TEXTURE, *src));
                            refs.push((KIND_TEXTURE, *dst));
                        }
                        Enc::FillBuffer { buffer, .. } => refs.push((KIND_BUFFER, *buffer)),
                        _ => {}
                    }
                }
                if let Some((fence, _)) = cb.signal {
                    refs.push((KIND_FENCE, fence));
                }
            }
            _ => {}
        }
        refs
    }
}

/// A persistent connection to the host GPU-exec service, implementing the [`CommandSink`] port by encoding
/// each batch and writing it as a framed submit over the Unix adapter.
///
/// One connection lives for the surface's whole lifetime — a frame is just `[hdr][ir]`+ack on the same fd,
/// so the host keeps its per-connection backend (shader/PSO/resource caches) warm across frames. A dropped
/// connection reconnects lazily on the next [`submit`](CommandSink::submit), and any reconnect after the
/// first advances [`generation`](RemoteCommandSink::generation). The connection consumes that reset
/// internally by replaying all acknowledged residency before it sends new work.
pub struct RemoteCommandSink {
    path: String,
    sock: Option<UnixStream>,
    connects: u64,
    residency_reset: bool,
    generation: u64,
    residency: ResidencyJournal,
    /// Signature (handshake bytes) of the last capability descriptor read off the connection. A changed
    /// signature while objects are resident cannot be recovered and is reported as API loss.
    negotiated_capabilities: Option<Vec<u8>>,
    /// The most recent decoded host advertisement, returned by [`CommandSink::negotiate`].
    host_caps: Option<Capabilities>,
    /// The surface the submit header names (which output the host presents to).
    surface: Surface,
}

impl RemoteCommandSink {
    fn api_loss(message: &'static str) -> io::Error {
        io::Error::new(
            ErrorKind::ConnectionAborted,
            format!("API/device/context lost: {message}"),
        )
    }
    /// Connect target from `$HL_GPU_EXEC`, falling back to [`DEFAULT_EXEC_SOCK`].
    pub fn from_env() -> Self {
        let path = std::env::var("HL_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_string());
        Self::new(path)
    }

    pub fn new(path: impl Into<String>) -> Self {
        RemoteCommandSink {
            path: path.into(),
            sock: None,
            connects: 0,
            residency_reset: false,
            generation: 0,
            residency: ResidencyJournal::default(),
            negotiated_capabilities: None,
            host_caps: None,
            surface: Surface::default(),
        }
    }

    /// Connect to `path` targeting `surface` (the output the submit header names).
    pub fn with_surface(path: impl Into<String>, surface: Surface) -> Self {
        let mut s = Self::new(path);
        s.surface = surface;
        s
    }

    /// Set the surface the submit header will name for subsequent submits.
    pub fn set_surface(&mut self, surface: Surface) {
        self.surface = surface;
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
    pub fn set_negotiated_capabilities(&mut self, caps: &Capabilities) -> io::Result<()> {
        let signature = caps.to_handshake();
        if self
            .negotiated_capabilities
            .as_ref()
            .is_some_and(|old| old != &signature)
            && !self.residency.cmds.is_empty()
        {
            return Err(Self::api_loss(
                "executor capabilities changed with live residency",
            ));
        }
        self.negotiated_capabilities = Some(signature);
        self.host_caps = Some(caps.clone());
        Ok(())
    }

    /// Ensure the socket is connected, reading + pinning the host's capability handshake on any fresh
    /// connection. A RE-connect (not the first) means a fresh host backend with an EMPTY resource cache,
    /// so the residency-reset flag is raised for the next submit to replay against.
    fn ensure(&mut self) -> io::Result<()> {
        if self.sock.is_some() {
            return Ok(());
        }
        let s = UnixStream::connect(&self.path)?;
        // The host advertises its capabilities first thing on every connection; read + pin them, which
        // also detects an incompatible profile change across a reconnect that has live residency.
        let caps = unix::Connection::new(&s).read_handshake()?;
        self.set_negotiated_capabilities(&caps)?;
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
    /// payload bytes; the host replies with a single ack byte. On any I/O error the connection is torn down
    /// and retried once (the executor may have restarted).
    fn submit_ir(&mut self, ir: &[u8], current: &[Cmd]) -> io::Result<()> {
        let mut last_err = None;
        for _ in 0..2 {
            // The closure yields the host's ack byte on success. A transport (I/O) error is retried on a
            // fresh connection; a NACK is NOT — the host received the frame and reported failure, so the
            // connection is healthy and re-sending would double-submit.
            let r = (|| -> io::Result<u8> {
                self.ensure()?;
                let mut payload = Vec::new();
                if self.residency_reset {
                    payload = self.residency.replay_bytes()?;
                }
                payload.extend_from_slice(ir);
                let header = SubmitHeader::for_frame(&self.surface, payload.len() as u32);
                let s = self
                    .sock
                    .as_ref()
                    .expect("ensure installed executor socket");
                unix::Connection::new(s).write_frame(&header, &payload)?;
                unix::Connection::new(s).read_ack()
            })();
            match r {
                Ok(ACK_OK) => {
                    self.residency_reset = false;
                    self.residency.append(current);
                    return Ok(());
                }
                // The executor NACKed this frame (replay failed / surface missing). Surface it as an error
                // rather than letting the guest commit a stale or partly-rendered frame as if it presented.
                Ok(nack) => {
                    return Err(io::Error::new(
                        ErrorKind::Other,
                        format!("host executor NACKed frame (ack={nack})"),
                    ));
                }
                Err(e) if e.kind() == ErrorKind::ConnectionAborted => {
                    // A typed API-loss (over-budget replay / incompatible profile change) is terminal — do
                    // not retry it as a transient transport fault.
                    return Err(e);
                }
                Err(e) => {
                    self.sock = None; // reconnect on next attempt
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::from(ErrorKind::BrokenPipe)))
    }
}

impl CommandSink for RemoteCommandSink {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        // Read (if not already) the host's advertised capabilities off the connection, then check the
        // guest's required features against them BEFORE advertising any matching API feature to the app.
        self.ensure().map_err(GpuError::transport)?;
        let caps = self
            .host_caps
            .clone()
            .expect("ensure pinned host capabilities");
        caps.negotiate(request)?;
        Ok(caps)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        let ir = crate::protocol::codec::Encoder::stream(batch);
        self.submit_ir(&ir, batch).map_err(GpuError::transport)
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        // A fence wait crosses the wire as a single-command frame — the host observes it in stream order.
        // It is an observation, not residency, so it is never replayed on reconnect (see `record`).
        let batch = [Cmd::WaitFence {
            id: fence.raw(),
            value,
        }];
        let ir = crate::protocol::codec::Encoder::stream(&batch);
        self.submit_ir(&ir, &batch).map_err(GpuError::transport)
    }

    /// Read `len` bytes of buffer `id` back from the host executor over the wire (the socketed
    /// `cuMemcpyDtoH` / `glReadPixels` path). Sends a readback-magic REQUEST frame — disjoint from a submit,
    /// so it never collides on the wire — and reads the host's length-prefixed byte response.
    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.ensure().map_err(GpuError::transport)?;
        // If a reconnect emptied the host's resource cache, flush acknowledged residency first so the buffer
        // being read is actually resident on the current executor before we query it.
        if self.residency_reset {
            self.submit_ir(&[], &[]).map_err(GpuError::transport)?;
        }
        let req = ReadbackRequest::buffer(id.raw(), offset, len as u64);
        let s = self
            .sock
            .as_ref()
            .expect("ensure installed executor socket");
        unix::Connection::new(s)
            .write_readback_request(&req)
            .map_err(GpuError::transport)?;
        unix::Connection::new(s)
            .read_readback_response()
            .map_err(GpuError::transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::model::descriptor::BufferDesc;
    use crate::protocol::model::enums::buffer_usage;

    #[test]
    fn residency_over_replay_budget_reports_clean_api_loss() {
        // When acknowledged residency exceeds the channel's replay budget, a reconnect must report a
        // clean, typed API loss instead of silently recovering a truncated set.
        let mk = |id| {
            Cmd::CreateBuffer(
                id,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            )
        };
        let mut journal = ResidencyJournal::with_budget(30);
        journal.append(&[mk(1)]);
        assert!(
            journal.replay_bytes().is_ok(),
            "residency within budget replays"
        );
        journal.append(&[mk(2)]); // pushes the encoded journal past the replay budget
        let err = journal
            .replay_bytes()
            .expect_err("over-budget residency must not silently truncate");
        assert_eq!(err.kind(), ErrorKind::ConnectionAborted);
        assert!(err.to_string().contains("API/device/context lost"));
    }

    #[test]
    fn capability_change_with_live_residency_is_typed_api_loss() {
        let mut conn = RemoteCommandSink::new("unused");
        let caps = Capabilities::full("host");
        conn.set_negotiated_capabilities(&caps).unwrap();
        conn.residency.append(&[Cmd::CreateFence(1)]);

        let mut changed = caps;
        changed.wire_version += 1;
        let err = conn
            .set_negotiated_capabilities(&changed)
            .expect_err("live profile change is loss");
        assert_eq!(err.kind(), ErrorKind::ConnectionAborted);
        assert!(err.to_string().contains("API/device/context lost"));
    }

    #[test]
    fn residency_skips_presents_and_waits() {
        let mut journal = ResidencyJournal::default();
        journal.append(&[
            Cmd::CreateFence(1),
            Cmd::Present {
                surface: 1,
                texture: 2,
            },
            Cmd::WaitFence { id: 1, value: 3 },
        ]);
        // Only the create is residency; present/wait are observations.
        assert_eq!(journal.cmds, vec![Cmd::CreateFence(1)]);
    }

    fn buf(id: u32) -> Cmd {
        Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 64,
                usage: buffer_usage::VERTEX,
                label: String::new(),
            },
        )
    }

    fn submit_refs(buffers: &[u32]) -> Cmd {
        use crate::protocol::model::command::{CommandBuffer, Enc};
        use crate::protocol::model::enums::IndexFormat;
        let encoder = buffers
            .iter()
            .map(|&b| Enc::SetVertexBuffer {
                slot: 0,
                buffer: b,
                offset: 0,
            })
            .chain(std::iter::once(Enc::SetIndexBuffer {
                buffer: buffers[0],
                offset: 0,
                format: IndexFormat::U16,
            }))
            .collect();
        Cmd::Submit(CommandBuffer {
            encoder,
            signal: None,
        })
    }

    #[test]
    fn teardown_destroys_compact_the_dead_working_set_out_of_the_journal() {
        // A whole working set (creates + a submit that uses it) then fully destroyed — the lost-context
        // teardown pattern. After the destroys the journal must hold NOTHING: a reconnect would otherwise
        // replay every dead resource's create and re-inflate the host ledger.
        let mut journal = ResidencyJournal::default();
        journal.append(&[buf(1), buf(2), submit_refs(&[1, 2])]);
        assert!(
            !journal.cmds.is_empty() && journal.bytes > 0,
            "working set recorded"
        );

        journal.append(&[Cmd::DestroyBuffer(1), Cmd::DestroyBuffer(2)]);
        assert!(
            journal.cmds.is_empty(),
            "a fully torn-down working set leaves the journal empty"
        );
        assert_eq!(
            journal.bytes, 0,
            "compacted journal reports zero live residency"
        );
        assert!(
            journal.replay_bytes().is_ok(),
            "an empty journal replays cleanly"
        );
    }

    #[test]
    fn compaction_keeps_live_residency_and_drops_only_the_dead() {
        // Two independent resources: buf 1 stays LIVE (used by submit A, never destroyed); buf 2 is created,
        // used by its OWN submit B, then destroyed. Only buf 2's create + submit B may leave the journal;
        // buf 1's create + submit A must survive so a reconnect still rebuilds the live resource.
        let mut journal = ResidencyJournal::default();
        journal.append(&[buf(1), buf(2), submit_refs(&[1]), submit_refs(&[2])]);
        journal.append(&[Cmd::DestroyBuffer(2)]);

        assert!(
            journal.cmds.contains(&buf(1)),
            "the live resource's create survives"
        );
        assert!(
            journal.cmds.contains(&submit_refs(&[1])),
            "the live resource's submit survives"
        );
        assert!(
            !journal.cmds.contains(&buf(2)),
            "the dead resource's create is compacted out"
        );
        assert!(
            !journal.cmds.contains(&submit_refs(&[2])),
            "the dead resource's submit is compacted out"
        );
        assert!(
            !journal
                .cmds
                .iter()
                .any(|c| matches!(c, Cmd::DestroyBuffer(2))),
            "its destroy too"
        );
    }

    #[test]
    fn compaction_reclaims_budget_so_churn_does_not_falsely_trip_replay_loss() {
        // A churny client that repeatedly creates + destroys a working set must NOT trip the replay budget:
        // the LIVE residency stays tiny even though the cumulative history is large. Compaction keeps the
        // journal bounded to the live set, so replay stays available.
        let mut journal = ResidencyJournal::with_budget(4096);
        for gen in 0..500u32 {
            let a = gen * 2 + 1;
            let b = gen * 2 + 2;
            journal.append(&[buf(a), buf(b), submit_refs(&[a, b])]);
            journal.append(&[Cmd::DestroyBuffer(a), Cmd::DestroyBuffer(b)]);
        }
        assert!(
            journal.cmds.is_empty(),
            "no live residency remains after balanced create/destroy churn"
        );
        assert!(
            journal.replay_bytes().is_ok(),
            "compaction kept the journal replayable (no false API loss)"
        );
    }
}
