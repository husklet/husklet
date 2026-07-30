use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub shaders: Metric,
    pub render_pipelines: Metric,
    pub render_pipeline_compilations: u64,
    pub compute_pipelines: Metric,
    pub bind_groups: Metric,
    pub logical_submissions: Metric,
    pub render_passes: Metric,
    pub compute_passes: Metric,
    pub native_submissions: u64,
    pub waits: Metric,
}

#[derive(Clone, Debug, Default)]
pub struct Metric {
    pub count: u64,
    pub elapsed: Duration,
}

impl Metric {
    pub(crate) fn add(&mut self, elapsed: Duration) {
        self.count = self.count.saturating_add(1);
        self.elapsed = self.elapsed.saturating_add(elapsed);
    }
}
