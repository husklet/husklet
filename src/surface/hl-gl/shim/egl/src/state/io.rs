use hl_gpu::{
    BufferId, Capabilities, Cmd, CommandSink, ExportId, FeatureRequest, FenceId, FenceWait, GpuError, TextureId,
};

pub(crate) enum Observation {
    Wait,
    Timed(FenceWait),
    Poll(bool),
    Read(Vec<u8>),
    Export(ExportId),
    TextureExport(ExportId),
}

pub(crate) struct IoResult<T> {
    pub(crate) value: T,
    pub(crate) observations: Vec<Observation>,
}

pub(super) struct IoPlan<T> {
    operations: Vec<Operation>,
    value: T,
}

enum Operation {
    Submit(Vec<Cmd>),
    Wait(FenceId, u64),
    Timed(FenceId, u64, u64),
    Poll(FenceId, u64),
    Read(BufferId, u64, usize),
    Export(BufferId),
    ExportTexture(TextureId),
}

impl<T> IoPlan<T> {
    pub(super) fn prepare(
        operation: impl FnOnce(&mut dyn CommandSink) -> hl_gpu::Result<T>,
    ) -> hl_gpu::Result<Self> {
        let mut sink = IoSink::default();
        let value = operation(&mut sink)?;
        Ok(Self {
            operations: sink.operations,
            value,
        })
    }

    pub(super) fn execute(self, sink: &mut dyn CommandSink) -> hl_gpu::Result<IoResult<T>> {
        let mut observations = Vec::new();
        for operation in self.operations {
            match operation {
                Operation::Submit(commands) => sink.submit(&commands)?,
                Operation::Wait(fence, value) => {
                    sink.wait(fence, value)?;
                    observations.push(Observation::Wait);
                }
                Operation::Timed(fence, value, timeout) => observations.push(Observation::Timed(
                    sink.wait_timeout(fence, value, timeout)?,
                )),
                Operation::Poll(fence, value) => {
                    observations.push(Observation::Poll(sink.poll_fence(fence, value)?));
                }
                Operation::Read(buffer, offset, len) => {
                    observations.push(Observation::Read(sink.read_buffer(buffer, offset, len)?))
                }
                Operation::Export(buffer) => {
                    observations.push(Observation::Export(sink.export_buffer(buffer)?))
                }
                Operation::ExportTexture(texture) => {
                    observations.push(Observation::TextureExport(sink.export_texture(texture)?))
                }
            }
        }
        Ok(IoResult {
            value: self.value,
            observations,
        })
    }
}

#[derive(Default)]
struct IoSink {
    operations: Vec<Operation>,
}

impl CommandSink for IoSink {
    fn negotiate(&mut self, _request: &FeatureRequest) -> hl_gpu::Result<Capabilities> {
        Err(GpuError::Unsupported("prepared I/O: negotiate"))
    }

    fn submit(&mut self, batch: &[Cmd]) -> hl_gpu::Result<()> {
        self.operations.push(Operation::Submit(batch.to_vec()));
        Ok(())
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> hl_gpu::Result<()> {
        self.operations.push(Operation::Wait(fence, value));
        Ok(())
    }

    fn wait_timeout(
        &mut self,
        fence: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> hl_gpu::Result<FenceWait> {
        self.operations
            .push(Operation::Timed(fence, value, timeout_ns));
        Ok(FenceWait::Timeout)
    }

    fn poll_fence(&mut self, fence: FenceId, value: u64) -> hl_gpu::Result<bool> {
        self.operations.push(Operation::Poll(fence, value));
        Ok(false)
    }

    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> hl_gpu::Result<Vec<u8>> {
        self.operations.push(Operation::Read(id, offset, len));
        Ok(vec![0; len])
    }

    fn export_buffer(&mut self, id: BufferId) -> hl_gpu::Result<ExportId> {
        self.operations.push(Operation::Export(id));
        Ok(ExportId(0))
    }

    fn export_texture(&mut self, id: TextureId) -> hl_gpu::Result<ExportId> {
        self.operations.push(Operation::ExportTexture(id));
        Ok(ExportId(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::RecordingSink;

    struct ObservingSink(RecordingSink);

    impl CommandSink for ObservingSink {
        fn negotiate(&mut self, request: &FeatureRequest) -> hl_gpu::Result<Capabilities> {
            self.0.negotiate(request)
        }

        fn submit(&mut self, batch: &[Cmd]) -> hl_gpu::Result<()> {
            self.0.submit(batch)
        }

        fn wait(&mut self, fence: FenceId, value: u64) -> hl_gpu::Result<()> {
            self.0.wait(fence, value)
        }

        fn wait_timeout(&mut self, _: FenceId, _: u64, _: u64) -> hl_gpu::Result<FenceWait> {
            Ok(FenceWait::Complete)
        }

        fn poll_fence(&mut self, _: FenceId, _: u64) -> hl_gpu::Result<bool> {
            Ok(true)
        }

        fn read_buffer(&mut self, _: BufferId, _: u64, len: usize) -> hl_gpu::Result<Vec<u8>> {
            Ok((1..=len as u8).collect())
        }

        fn export_buffer(&mut self, id: BufferId) -> hl_gpu::Result<ExportId> {
            Ok(ExportId(id.0 as u64 + 100))
        }
    }

    #[test]
    fn execution_returns_actor_observations_in_operation_order() {
        let plan = IoPlan::prepare(|sink| {
            assert_eq!(sink.wait_timeout(FenceId(1), 2, 3)?, FenceWait::Timeout);
            assert!(!sink.poll_fence(FenceId(4), 5)?);
            assert_eq!(sink.read_buffer(BufferId(6), 7, 3)?, [0, 0, 0]);
            assert_eq!(sink.export_buffer(BufferId(8))?, ExportId(0));
            Ok(11)
        })
        .expect("prepare");
        let mut sink = ObservingSink(RecordingSink::with_full_caps());

        let result = plan.execute(&mut sink).expect("execute");
        assert_eq!(result.value, 11);
        assert!(matches!(
            result.observations.as_slice(),
            [
                Observation::Timed(FenceWait::Complete),
                Observation::Poll(true),
                Observation::Read(bytes),
                Observation::Export(ExportId(108))
            ] if bytes == &[1, 2, 3]
        ));
    }
}
