use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub shaders: Metric,
    pub render_pipelines: Metric,
    pub render_pipeline_compilations: u64,
    pub compute_pipelines: Metric,
    pub compute_pipeline_compilations: u64,
    pub bind_groups: Metric,
    /// `CreateBuffer` → `wgpu::Buffer` allocation. A driver that mints a fresh uniform buffer per draw
    /// pays this once per draw.
    pub buffers: Metric,
    /// `WriteBuffer` → `queue.write_buffer` (plus the read-modify-write path for an unaligned window).
    pub buffer_writes: Metric,
    /// `DestroyBuffer` + `DestroyBindGroup`: dropping the native handle behind a protocol id. These are the
    /// two a per-draw-resource driver issues once per draw.
    pub destroys: Metric,
    /// Building the concrete `wgpu::BindGroup` a draw binds, at draw-record time. Counted per (draw, set),
    /// not per `CreateBindGroup` — this is the per-draw cost `bind_groups` does not see.
    pub draw_bind_groups: Metric,
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
