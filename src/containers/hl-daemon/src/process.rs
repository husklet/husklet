/// Host accounting values for one container leader process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessSample {
    pub memory: u64,
    pub cpu_seconds: u64,
}

/// Application-supplied host process accounting capability.
pub trait ProcessSampler: Send + Sync + 'static {
    fn sample(&self, process_id: u64) -> ProcessSample;
}

pub(crate) struct UnavailableProcessSampler;

impl ProcessSampler for UnavailableProcessSampler {
    fn sample(&self, _process_id: u64) -> ProcessSample {
        ProcessSample::default()
    }
}
