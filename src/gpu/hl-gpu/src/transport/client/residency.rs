pub(super) struct ResidencyJournal {
    pub(super) cmds: Vec<Cmd>,
    pub(super) bytes: usize,
    pub(super) replayable: bool,
    /// Maximum encoded residency the channel will replay on reconnect. Past this the journal drops
    /// `replayable` and a reconnect reports a clean API loss instead of silently recovering a truncated
    /// resource set. Configurable so the over-budget transition is testable without a multi-MB fixture.
    pub(super) max_bytes: usize,
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
    pub(super) fn with_budget(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            ..Self::default()
        }
    }

    pub(super) fn append(&mut self, cmds: &[Cmd]) {
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

    pub(super) fn replay_bytes(&self) -> std::result::Result<Vec<u8>, TransportError> {
        if !self.replayable && !self.cmds.is_empty() {
            return Err(TransportError::ApiLost {
                detail: "executor residency exceeded replay budget".into(),
            });
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
                | Cmd::DestroyTextureView(_)
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
            Cmd::CreateTextureView(id, _) => (KIND_TEXTURE, *id),
            Cmd::CreateSampler(id, _) => (KIND_SAMPLER, *id),
            Cmd::CreateShader { id, .. } => (KIND_SHADER, *id),
            Cmd::CreateRenderPipeline(id, _)
            | Cmd::CreateComputePipeline(id, _)
            | Cmd::CreateRenderPipelineLayout(id, _, _, _)
            | Cmd::CreateComputePipelineLayout(id, _, _) => (KIND_PIPELINE, *id),
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
            Cmd::DestroyTextureView(id) => (KIND_TEXTURE, *id),
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
            Cmd::CreateTextureView(id, view) => {
                refs.push((KIND_TEXTURE, *id));
                refs.push((KIND_TEXTURE, view.texture));
            }
            Cmd::DestroyTextureView(id) => refs.push((KIND_TEXTURE, *id)),
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
            Cmd::CreateRenderPipelineLayout(id, d, _, _) => {
                refs.push((KIND_PIPELINE, *id));
                refs.push((KIND_SHADER, d.vertex.module));
                if let Some(fragment) = &d.fragment {
                    refs.push((KIND_SHADER, fragment.module));
                }
            }
            Cmd::CreateComputePipelineLayout(id, d, _) => {
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
                        BindResource::TexelBuffer { id, .. } => refs.push((KIND_BUFFER, id)),
                        BindResource::BufferArray { ref elements } => {
                            refs.extend(elements.iter().map(|element| (KIND_BUFFER, element.id)));
                        }
                        BindResource::TextureArray { ref ids } => {
                            refs.extend(ids.iter().map(|&id| (KIND_TEXTURE, id)));
                        }
                        BindResource::SamplerArray { ref ids } => {
                            refs.extend(ids.iter().map(|&id| (KIND_SAMPLER, id)));
                        }
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
                        Enc::CopyBufferToTexture { src, dst, .. }
                        | Enc::CopyBufferToTextureRegion { src, dst, .. } => {
                            refs.push((KIND_BUFFER, *src));
                            refs.push((KIND_TEXTURE, *dst));
                        }
                        Enc::CopyTextureToBuffer { src, dst, .. }
                        | Enc::CopyTextureToBufferRegion { src, dst, .. } => {
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
use super::*;
