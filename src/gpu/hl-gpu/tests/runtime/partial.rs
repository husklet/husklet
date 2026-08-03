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
                        vec![0],
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
    fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<hl_gpu::Execution> {
        Ok(hl_gpu::Execution::partial(Vec::new(), GpuError::Invalid("refused"), Vec::new()))
    }
    fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    fn export_buffer(&self, resources: &SessionResources, id: hl_gpu::BufferId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.buffers.get(id.0)?;
        Ok((std::sync::Arc::new(()), 64))
    }
    fn export_texture(&self, resources: &SessionResources, id: TextureId) -> Result<(hl_gpu::runtime::model::sharing::Shared, u64)> {
        resources.textures.get(id.0)?;
        Ok((std::sync::Arc::new(()), 4))
    }
}

#[test]
fn refused_fence_lifecycle_is_not_committed() {
    let caps = Capabilities::permissive_fixture("fence partial");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = RefuseAll(caps);
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &[Cmd::CreateFence(9)])
        .expect("partial outcome");
    assert_eq!(outcome.accepted, [false]);
    assert!(!session.resources.fences.contains(9));
    assert_eq!(session.object_count(), 0);
}

#[test]
fn successful_presentation_survives_a_later_refusal() {
    struct PresentPartial(Capabilities);
    impl GpuExecutor for PresentPartial {
        fn capabilities(&self) -> Capabilities { self.0.clone() }
        fn execute(&mut self, _: &mut SessionResources, _: &[Cmd]) -> Result<hl_gpu::Execution> {
            Ok(hl_gpu::Execution::partial(vec![hl_gpu::Presentation {
                surface: SurfaceId(1), texture: TextureId(2),
                token: hl_gpu::SurfaceToken::new(3).unwrap(),
                serial: hl_gpu::FrameSerial::new(4).unwrap(),
            }], GpuError::Invalid("later refusal"), vec![0]))
        }
        fn wait(&mut self, _: &mut SessionResources, _: FenceId, _: u64) -> Result<()> { Ok(()) }
    }
    let caps = Capabilities::permissive_fixture("present partial");
    let mut session = session(Limits::from_capabilities(caps.clone()), GlobalLedger::unbounded());
    let mut executor = PresentPartial(caps);
    let batch = [Cmd::Present { surface: 1, texture: 2, serial: hl_gpu::FrameSerial::new(4).unwrap() }];
    let outcome = hl_gpu::runtime::submit_outcome(&mut session, &mut executor, 0, &batch).unwrap();
    assert_eq!(outcome.presentations.len(), 1);
    assert!(outcome.refusal.is_some());
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
