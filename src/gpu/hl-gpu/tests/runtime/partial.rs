use super::*;

struct PartialExecutor {
    caps: Capabilities,
    refused: bool,
    used_committed_buffer: bool,
}

impl GpuExecutor for PartialExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn execute(&mut self, resources: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
        for command in batch {
            match command {
                Cmd::CreateBuffer(id, _) => resources.buffers.insert(*id, Box::new(()))?,
                Cmd::Submit(_) if !self.refused => {
                    self.refused = true;
                    return Ok(hl_gpu::Execution::partial(
                        Vec::new(),
                        GpuError::UnknownId {
                            kind: "texture",
                            id: 999,
                        },
                        batch,
                        vec![0],
                        Vec::new(),
                    ));
                }
                Cmd::Submit(_) => {
                    resources.buffers.get(7)?;
                    self.used_committed_buffer = true;
                }
                _ => {}
            }
        }
        Ok(hl_gpu::Execution::accepted(Vec::new()))
    }

    fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> {
        Ok(())
    }
}

#[test]
fn partial_refusal_commits_resource_for_the_next_command_buffer() {
    let caps = Capabilities::permissive_fixture("partial executor");
    let mut executor = PartialExecutor {
        caps: caps.clone(),
        refused: false,
        used_committed_buffer: false,
    };
    let mut session = session(
        Limits::from_capabilities(caps),
        GlobalLedger::unbounded(),
    );
    let result = hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[buffer(7, 64), Cmd::Submit(CommandBuffer::default())],
    );
    assert_eq!(
        result,
        Err(GpuError::Partial(Box::new(GpuError::UnknownId {
            kind: "texture",
            id: 999,
        })))
    );
    assert!(session.resources.buffers.contains(7));
    assert_eq!(session.residency_bytes(), 64);

    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[Cmd::Submit(CommandBuffer::default())],
    )
    .expect("the next command buffer must resolve the committed resource");
    assert!(executor.used_committed_buffer);
}

struct RefuseAll(Capabilities);
impl GpuExecutor for RefuseAll {
    fn capabilities(&self) -> Capabilities { self.0.clone() }
    fn execute(&mut self, _: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
        Ok(hl_gpu::Execution::partial(Vec::new(), GpuError::Invalid("refused"), batch, Vec::new(), Vec::new()))
    }
    fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    fn export_buffer(&self, resources: &SessionResources, id: hl_gpu::BufferId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.buffers.get(id.0)?;
        Ok((std::sync::Arc::new(()), 64))
    }
    fn export_texture(&self, resources: &SessionResources, id: TextureId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.textures.get(id.0)?;
        Ok((std::sync::Arc::new(()), 16))
    }
}

#[test]
fn refused_fence_lifecycle_is_not_committed() {
    let caps = Capabilities::permissive_fixture("fence partial");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = RefuseAll(caps);
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &[Cmd::CreateFence(9)])
        .expect("partial outcome");
    assert!(outcome.committed.commands.is_empty());
    assert!(!session.resources.fences.contains(9));
    assert_eq!(session.object_count(), 0);
}

#[test]
fn successful_presentation_survives_a_later_refusal() {
    struct PresentPartial(Capabilities);
    impl GpuExecutor for PresentPartial {
        fn capabilities(&self) -> Capabilities { self.0.clone() }
        fn execute(&mut self, _: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
            Ok(hl_gpu::Execution::partial(vec![hl_gpu::Presentation {
                surface: SurfaceId(1), texture: TextureId(2),
                token: hl_gpu::SurfaceToken::new(3).unwrap(),
                serial: hl_gpu::FrameSerial::new(4).unwrap(),
            }], GpuError::Invalid("later refusal"), batch, vec![0], Vec::new()))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    }
    let caps = Capabilities::permissive_fixture("present partial");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = PresentPartial(caps);
    let batch = [Cmd::Present { surface: 1, texture: 2, serial: hl_gpu::FrameSerial::new(4).unwrap() }];
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert_eq!(outcome.committed.presentations.len(), 1);
    assert!(outcome.refusal.is_some());
}

#[test]
fn partial_delta_carries_only_the_fence_signal_that_was_scheduled() {
    struct FencePartial(Capabilities);
    impl GpuExecutor for FencePartial {
        fn capabilities(&self) -> Capabilities { self.0.clone() }
        fn execute(&mut self, resources: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
            resources.fences.insert(5, Box::new(()))?;
            Ok(hl_gpu::Execution::partial(Vec::new(), GpuError::Invalid("later refusal"), batch, vec![0, 1], vec![(5, 9)]))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    }
    let caps = Capabilities::permissive_fixture("partial fence signal");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = FencePartial(caps);
    let batch = [
        Cmd::CreateFence(5),
        Cmd::Submit(CommandBuffer { encoder: Vec::new(), signal: Some((5, 9)) }),
        Cmd::DestroyFence(5),
    ];
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert_eq!(outcome.committed.replay_commands().collect::<Vec<_>>(), [&Cmd::CreateFence(5)]);
    assert_eq!(outcome.committed.fence_signals, [(5, 9)]);
    assert!(!outcome.committed.replayable);
    assert_eq!(session.timeline.get(5), Some(9));
}

#[test]
fn committed_submit_does_not_imply_its_fence_signal_was_scheduled() {
    struct UnscheduledSignal(Capabilities);
    impl GpuExecutor for UnscheduledSignal {
        fn capabilities(&self) -> Capabilities { self.0.clone() }
        fn execute(&mut self, _: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
            Ok(hl_gpu::Execution::partial(
                Vec::new(),
                GpuError::UnknownId { kind: "fence", id: 9 },
                batch,
                vec![0],
                Vec::new(),
            ))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    }
    let caps = Capabilities::permissive_fixture("unscheduled partial signal");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = UnscheduledSignal(caps);
    let batch = [Cmd::Submit(CommandBuffer { encoder: Vec::new(), signal: Some((9, 4)) })];
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert!(outcome.committed.fence_signals.is_empty());
    assert_eq!(session.timeline.get(9), None);
}

#[test]
fn fully_accepted_submit_remains_replayable() {
    let caps = Capabilities::permissive_fixture("accepted replayable submit");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = PartialExecutor { caps, refused: true, used_committed_buffer: false };
    let submit = Cmd::Submit(CommandBuffer::default());
    let batch = [buffer(7, 64), submit.clone()];
    let outcome = hl_gpu::runtime::submit_outcome(
        &mut session,
        &mut executor,
        0,
        &batch,
    )
    .unwrap();
    assert!(outcome.committed.replayable);
    assert_eq!(outcome.committed.replay_commands().collect::<Vec<_>>(), [&batch[0], &submit]);
}

#[test]
fn refused_shared_buffer_destroy_keeps_export_and_charge() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("shared partial");
    let exports = Exports::new();
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded()).with_exports(exports.clone());
    let mut executor = RefuseAll(caps);
    session.resources.buffers.insert(1, Box::new(())).unwrap();
    session.charge_frame(&[buffer(1, 64)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_buffer(&mut session, &executor, hl_gpu::BufferId(1)).unwrap();
    hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    assert!(exports.is_live(export));
    assert!(session.resources.buffers.contains(1));
    assert_eq!(session.residency_bytes(), 64);
}

#[test]
fn refused_shared_texture_destroy_keeps_export_and_charge() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("shared texture partial");
    let exports = Exports::new();
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded()).with_exports(exports.clone());
    let mut executor = RefuseAll(caps);
    session.resources.textures.insert(2, Box::new(())).unwrap();
    session.charge_frame(&[texture(2, 2)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_texture(&mut session, &executor, TextureId(2)).unwrap();
    hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &[Cmd::DestroyTexture(2)]).unwrap();
    assert!(exports.is_live(export));
    assert!(session.resources.textures.contains(2));
    assert!(session.residency_bytes() > 0);
}

struct DestroyThenRefuse(Capabilities);

impl GpuExecutor for DestroyThenRefuse {
    fn capabilities(&self) -> Capabilities { self.0.clone() }
    fn execute(&mut self, resources: &mut SessionResources, batch: &[Cmd]) -> Result<hl_gpu::Execution> {
        match &batch[0] {
            Cmd::DestroyBuffer(id) => { resources.buffers.remove(*id)?; }
            Cmd::DestroyTexture(id) => { resources.textures.remove(*id)?; }
            _ => panic!("fixture requires a destroy first"),
        }
        Ok(hl_gpu::Execution::partial(Vec::new(), GpuError::Invalid("replacement refused"), batch, vec![0], Vec::new()))
    }
    fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    fn export_buffer(&self, resources: &SessionResources, id: hl_gpu::BufferId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.buffers.get(id.0)?;
        Ok((std::sync::Arc::new(()), 64))
    }
    fn import_buffer(&self, resource: hl_gpu::runtime::model::sharing::Shared, _: u64) -> Result<hl_gpu::runtime::Native> { Ok(Box::new(resource)) }
    fn export_texture(&self, resources: &SessionResources, id: TextureId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.textures.get(id.0)?;
        Ok((std::sync::Arc::new(()), 16))
    }
    fn import_texture(&self, resource: hl_gpu::runtime::model::sharing::Shared, _: u64) -> Result<hl_gpu::runtime::Native> { Ok(Box::new(resource)) }
}

#[test]
fn committed_owner_buffer_destroy_does_not_preserve_a_refused_replacement() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("owner buffer replacement");
    let exports = Exports::new();
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded()).with_exports(exports.clone());
    let mut executor = DestroyThenRefuse(caps);
    session.resources.buffers.insert(1, Box::new(())).unwrap();
    session.charge_frame(&[buffer(1, 64)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_buffer(&mut session, &executor, hl_gpu::BufferId(1)).unwrap();
    let batch = [Cmd::DestroyBuffer(1), buffer(1, 32)];
    hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert!(!session.resources.buffers.contains(1));
    assert!(!exports.is_live(export));
    assert_eq!(session.residency_bytes(), 0);
}

#[test]
fn committed_owner_texture_destroy_does_not_preserve_a_refused_replacement() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("owner texture replacement");
    let exports = Exports::new();
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded()).with_exports(exports.clone());
    let mut executor = DestroyThenRefuse(caps);
    session.resources.textures.insert(2, Box::new(())).unwrap();
    session.charge_frame(&[texture(2, 2)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_texture(&mut session, &executor, TextureId(2)).unwrap();
    let batch = [Cmd::DestroyTexture(2), texture(2, 1)];
    hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert!(!session.resources.textures.contains(2));
    assert!(!exports.is_live(export));
    assert_eq!(session.residency_bytes(), 0);
}

#[test]
fn committed_importer_buffer_destroy_does_not_preserve_a_refused_replacement() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("importer buffer replacement");
    let exports = Exports::new();
    let global = GlobalLedger::unbounded();
    let mut owner = session(Limits::from_capabilities(caps.clone()), global.clone()).with_exports(exports.clone());
    let mut importer = session(Limits::from_capabilities(caps.clone()), global).with_exports(exports.clone());
    let mut executor = DestroyThenRefuse(caps);
    owner.resources.buffers.insert(1, Box::new(())).unwrap();
    owner.charge_frame(&[buffer(1, 64)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &executor, hl_gpu::BufferId(1)).unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(&mut importer, &executor, hl_gpu::BufferId(1), export).unwrap();
    let batch = [Cmd::DestroyBuffer(1), buffer(1, 32)];
    hl_gpu::runtime::submit_outcome(&mut importer, &mut executor, 0, &batch).unwrap();
    assert!(!importer.resources.buffers.contains(1));
    assert!(exports.is_live(export), "the owner's identity remains live");
    assert_eq!(importer.residency_bytes(), 0);
    assert_eq!(owner.residency_bytes(), 64);
}

#[test]
fn committed_importer_texture_destroy_does_not_preserve_a_refused_replacement() {
    use hl_gpu::runtime::model::sharing::Exports;
    let caps = Capabilities::permissive_fixture("importer texture replacement");
    let exports = Exports::new();
    let global = GlobalLedger::unbounded();
    let mut owner = session(Limits::from_capabilities(caps.clone()), global.clone()).with_exports(exports.clone());
    let mut importer = session(Limits::from_capabilities(caps.clone()), global).with_exports(exports.clone());
    let mut executor = DestroyThenRefuse(caps);
    owner.resources.textures.insert(2, Box::new(())).unwrap();
    owner.charge_frame(&[texture(2, 2)]).unwrap();
    let export = hl_gpu::runtime::service::dispatch::export_texture(&mut owner, &executor, TextureId(2)).unwrap();
    hl_gpu::runtime::service::dispatch::import_texture(&mut importer, &executor, TextureId(2), export).unwrap();
    let batch = [Cmd::DestroyTexture(2), texture(2, 1)];
    hl_gpu::runtime::submit_outcome(&mut importer, &mut executor, 0, &batch).unwrap();
    assert!(!importer.resources.textures.contains(2));
    assert!(exports.is_live(export), "the owner's identity remains live");
    assert_eq!(importer.residency_bytes(), 0);
    assert!(owner.residency_bytes() > 0);
}
