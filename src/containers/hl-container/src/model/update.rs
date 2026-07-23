use super::{Resources, RestartPolicy};

/// Partial mutable container configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Update {
    pub memory_bytes: Option<u64>,
    pub process_count: Option<u32>,
    pub cpu_count: Option<u32>,
    pub restart: Option<RestartPolicy>,
}

impl Update {
    pub(crate) fn apply(self, resources: &mut Resources, restart: &mut RestartPolicy) {
        if let Some(value) = self.memory_bytes {
            resources.memory_bytes = value;
        }
        if let Some(value) = self.process_count {
            resources.process_count = value;
        }
        if let Some(value) = self.cpu_count {
            resources.cpu_count = value;
        }
        if let Some(value) = self.restart {
            *restart = value;
        }
    }
}
