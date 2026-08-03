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
