use hl_gpu::{
    BufferId, Capabilities, Cmd, CommandSink, FeatureRequest, FenceId, FenceWait, GpuError,
};

/// Immutable submit-only work prepared while a share group is exclusively borrowed.
pub(super) struct SubmitPlan<T> {
    batches: Vec<Vec<Cmd>>,
    value: T,
}

impl<T> SubmitPlan<T> {
    pub(super) fn prepare(
        operation: impl FnOnce(&mut dyn CommandSink) -> hl_gpu::Result<T>,
    ) -> hl_gpu::Result<Self> {
        let mut sink = SubmitSink::default();
        let value = operation(&mut sink)?;
        Ok(Self {
            batches: sink.batches,
            value,
        })
    }

    pub(super) fn execute(self, sink: &mut dyn CommandSink) -> hl_gpu::Result<T> {
        for batch in self.batches {
            sink.submit(&batch)?;
        }
        Ok(self.value)
    }
}

#[derive(Default)]
struct SubmitSink {
    batches: Vec<Vec<Cmd>>,
}

impl CommandSink for SubmitSink {
    fn negotiate(&mut self, _request: &FeatureRequest) -> hl_gpu::Result<Capabilities> {
        Err(GpuError::Unsupported("prepared submit: negotiate"))
    }

    fn submit(&mut self, batch: &[Cmd]) -> hl_gpu::Result<()> {
        self.batches.push(batch.to_vec());
        Ok(())
    }

    fn wait(&mut self, _fence: FenceId, _value: u64) -> hl_gpu::Result<()> {
        Err(GpuError::Unsupported("prepared submit: fence wait"))
    }

    fn wait_timeout(
        &mut self,
        _fence: FenceId,
        _value: u64,
        _timeout_ns: u64,
    ) -> hl_gpu::Result<FenceWait> {
        Err(GpuError::Unsupported("prepared submit: timed fence wait"))
    }

    fn poll_fence(&mut self, _fence: FenceId, _value: u64) -> hl_gpu::Result<bool> {
        Err(GpuError::Unsupported("prepared submit: fence poll"))
    }

    fn read_buffer(&mut self, _id: BufferId, _offset: u64, _len: usize) -> hl_gpu::Result<Vec<u8>> {
        Err(GpuError::Unsupported("prepared submit: read buffer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::RecordingSink;

    #[test]
    fn preserves_batch_boundaries_and_result() {
        let plan = SubmitPlan::prepare(|sink| {
            sink.submit(&[Cmd::CreateFence(7)])?;
            sink.submit(&[Cmd::DestroyFence(7)])?;
            Ok(19)
        })
        .expect("prepare");
        let mut sink = RecordingSink::with_full_caps();

        assert_eq!(plan.execute(&mut sink), Ok(19));
        assert_eq!(sink.batches.len(), 2);
        assert!(matches!(sink.batches[0].as_slice(), [Cmd::CreateFence(7)]));
        assert!(matches!(sink.batches[1].as_slice(), [Cmd::DestroyFence(7)]));
    }

    #[test]
    fn rejects_observations_during_prepare() {
        let error = SubmitPlan::prepare(|sink| sink.poll_fence(FenceId(1), 1))
            .err()
            .expect("unsupported");
        assert!(matches!(error, GpuError::Unsupported(_)));
    }
}
