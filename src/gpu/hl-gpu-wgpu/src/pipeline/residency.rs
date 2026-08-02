//! Device-scoped residency for immutable compiled render pipelines.
//!
//! Guest pipeline ids, live-alias refcounts, accounting, and rollback remain executor-local. This bounded
//! residency contains only exact-keyed immutable artifacts that executors on the same wgpu device can reuse.

use std::sync::Mutex;

use hl_gpu::protocol::model::enums::TextureFormat;

use crate::dedup::{ComputePipeKey, RenderPipeKey};

const CAPACITY: usize = 128;

#[derive(Clone)]
pub(crate) struct Artifact {
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) color_formats: Vec<TextureFormat>,
    pub(crate) used_bindings: Vec<(u32, u32)>,
}

#[derive(Clone)]
pub(crate) struct ComputeArtifact {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) remap_group_zero: bool,
    pub(crate) texel: Option<std::sync::Arc<crate::texel_buffer::ComputeSpecializer>>,
}

struct Entry<K, V> {
    key: K,
    artifact: V,
    used: u64,
}

struct Entries<K, V> {
    values: Vec<Entry<K, V>>,
    capacity: usize,
    clock: u64,
}

impl<K: Clone + PartialEq, V: Clone> Entries<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            capacity,
            clock: 0,
        }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.values
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| entry.artifact.clone())
    }

    fn retain(&mut self, key: K, artifact: V) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.values.iter_mut().find(|entry| entry.key == key) {
            entry.used = self.clock;
            return;
        }
        if self.capacity == 0 {
            return;
        }
        if self.values.len() == self.capacity {
            if let Some((oldest, _)) = self
                .values
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used)
            {
                self.values.swap_remove(oldest);
            }
        }
        self.values.push(Entry {
            key,
            artifact,
            used: self.clock,
        });
    }

    fn touch(&mut self, key: &K) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.values.iter_mut().find(|entry| entry.key == *key) {
            entry.used = self.clock;
        }
    }
}

pub(crate) enum Mutation {
    Hit(RenderPipeKey),
    Install(RenderPipeKey, Artifact),
    ComputeHit(ComputePipeKey),
    ComputeInstall(ComputePipeKey, ComputeArtifact),
}

/// Bounded render pipelines compiled by one host device and reusable by its isolated executors.
pub(crate) struct Residency {
    entries: Mutex<Entries<RenderPipeKey, Artifact>>,
    compute_entries: Mutex<Entries<ComputePipeKey, ComputeArtifact>>,
    #[cfg(test)]
    hits: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    compilations: std::sync::atomic::AtomicU64,
}

impl Residency {
    pub(crate) fn new() -> Self {
        Self::with_capacity(CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::new(capacity)),
            compute_entries: Mutex::new(Entries::new(capacity)),
            #[cfg(test)]
            hits: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            compilations: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Observe an artifact without mutating its eviction order. A successful batch later commits the hit.
    pub(crate) fn get(&self, key: &RenderPipeKey) -> Option<Artifact> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
    }

    pub(crate) fn compute_get(&self, key: &ComputePipeKey) -> Option<ComputeArtifact> {
        self.compute_entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
    }

    pub(crate) fn apply(&self, mutations: Vec<Mutation>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for mutation in mutations {
            match mutation {
                Mutation::Hit(key) => {
                    entries.touch(&key);
                    #[cfg(test)]
                    self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Mutation::Install(key, artifact) => {
                    entries.retain(key, artifact);
                    #[cfg(test)]
                    self.compilations
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Mutation::ComputeHit(key) => {
                    drop(entries);
                    self.compute_entries
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .touch(&key);
                    entries = self
                        .entries
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    #[cfg(test)]
                    self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Mutation::ComputeInstall(key, artifact) => {
                    drop(entries);
                    self.compute_entries
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .retain(key, artifact);
                    entries = self
                        .entries
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    #[cfg(test)]
                    self.compilations
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    #[cfg(test)]
    fn stats(&self) -> (u64, u64, usize) {
        use std::sync::atomic::Ordering;

        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            self.hits.load(Ordering::Relaxed),
            self.compilations.load(Ordering::Relaxed),
            entries.values.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use hl_gpu::protocol::model::command::ShaderPayloadKind;
    use hl_gpu::protocol::model::descriptor::{
        ColorTargetState, ComputePipelineDesc, RenderPipelineDesc, ShaderRef,
    };
    use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::{Cmd, GpuExecutor, SessionResources};

    use super::Entries;
    use crate::{Device, DeviceConfig};

    const VERTEX: &str = r#"#version 450
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
    const RED: &str = r#"#version 450
layout(location = 0) out vec4 color;
void main() { color = vec4(1.0, 0.0, 0.0, 1.0); }
"#;
    const BLUE: &str = r#"#version 450
layout(location = 0) out vec4 color;
void main() { color = vec4(0.0, 0.0, 1.0, 1.0); }
"#;
    const COMPUTE: &str = r#"#version 450
layout(local_size_x = 1) in;
void main() {}
"#;

    fn shader(stage: u32, source: &str) -> Vec<u32> {
        GlslDescriptor {
            stage,
            entry: "main".to_owned(),
            source: source.to_owned(),
        }
        .to_words()
    }

    fn pipeline(write_mask: u32) -> RenderPipelineDesc {
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "main".to_owned(),
            },
            fragment: Some(ShaderRef {
                module: 2,
                entry: "main".to_owned(),
            }),
            vertex_buffers: Vec::new(),
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask,
            }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        }
    }

    fn shaders(fragment: &str) -> Vec<Cmd> {
        vec![
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: shader(glsl_stage::VERTEX, VERTEX),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: shader(glsl_stage::FRAGMENT, fragment),
            },
        ]
    }

    fn create(fragment: &str, write_mask: u32) -> Vec<Cmd> {
        let mut commands = shaders(fragment);
        commands.push(Cmd::CreateRenderPipeline(1, pipeline(write_mask)));
        commands
    }

    fn create_compute() -> Vec<Cmd> {
        vec![
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: shader(glsl_stage::COMPUTE, COMPUTE),
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "main".to_owned(),
                    },
                    label: String::new(),
                },
            ),
        ]
    }

    #[test]
    fn bounded_entries_evict_least_recently_used() {
        let mut entries = Entries::new(2);
        entries.retain("first", 1);
        entries.retain("second", 2);
        entries.touch(&"first");
        entries.retain("third", 3);

        assert_eq!(entries.values.len(), 2);
        assert_eq!(entries.get(&"first"), Some(1));
        assert_eq!(entries.get(&"second"), None);
        assert_eq!(entries.get(&"third"), Some(3));
    }

    #[test]
    fn sequential_executors_reuse_exact_pipeline() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut first = device.executor();
        first
            .execute(&mut SessionResources::default(), &create(RED, 0xf))
            .unwrap();
        drop(first);

        let mut second = device.executor();
        second
            .execute(&mut SessionResources::default(), &create(RED, 0xf))
            .unwrap();

        assert_eq!(device.pipelines.stats(), (1, 1, 1));
        assert_eq!(second.pipeline_backing_count(), 1);
    }

    #[test]
    fn sequential_connections_reuse_exact_compute_pipeline() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut first = device.executor();
        first.enable_profile();
        first
            .execute(&mut SessionResources::default(), &create_compute())
            .unwrap();
        assert_eq!(first.profile().unwrap().compute_pipeline_compilations, 1);

        let mut second = device.executor();
        second.enable_profile();
        second
            .execute(&mut SessionResources::default(), &create_compute())
            .unwrap();
        assert_eq!(
            second.profile().unwrap().compute_pipeline_compilations,
            0,
            "a later connection on the same host device must reuse the committed immutable PSO"
        );
    }

    #[test]
    fn fixed_state_and_source_are_part_of_exact_identity() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        for commands in [create(RED, 0xf), create(RED, 0x7), create(BLUE, 0xf)] {
            device
                .executor()
                .execute(&mut SessionResources::default(), &commands)
                .unwrap();
        }

        assert_eq!(device.pipelines.stats(), (0, 3, 3));
    }

    #[test]
    fn failed_batch_does_not_publish_pipeline() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut commands = create(RED, 0xf);
        commands.push(Cmd::DestroyPipeline(99));

        assert!(device
            .executor()
            .execute(&mut SessionResources::default(), &commands)
            .is_err());
        assert_eq!(device.pipelines.stats(), (0, 0, 0));
    }

    #[test]
    fn duplicate_guest_id_does_not_touch_shared_pipeline() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        device
            .executor()
            .execute(&mut SessionResources::default(), &create(RED, 0xf))
            .unwrap();

        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let mut distinct = shaders(RED);
        distinct.push(Cmd::CreateRenderPipeline(1, pipeline(0x7)));
        executor.execute(&mut resources, &distinct).unwrap();
        let before = device.pipelines.stats();
        assert!(executor
            .execute(
                &mut resources,
                &[Cmd::CreateRenderPipeline(1, pipeline(0xf))]
            )
            .is_err());

        assert_eq!(device.pipelines.stats(), before);
    }

    #[test]
    fn concurrent_executors_converge_on_one_exact_entry() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let device = std::sync::Arc::new(device);
        let ready = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let device = std::sync::Arc::clone(&device);
                let ready = std::sync::Arc::clone(&ready);
                std::thread::spawn(move || {
                    let mut executor = device.executor();
                    let mut resources = SessionResources::default();
                    ready.wait();
                    executor.execute(&mut resources, &create(RED, 0xf)).unwrap();
                    executor
                        .execute(&mut resources, &[Cmd::DestroyPipeline(1)])
                        .unwrap();
                    (
                        executor.pipeline_backing_count(),
                        executor.pipeline_backing_resident_bytes(),
                    )
                })
            })
            .collect();
        ready.wait();
        for worker in workers {
            assert_eq!(
                worker.join().unwrap(),
                (0, 0),
                "concurrent exact misses must not retain losing executor-local pipelines"
            );
        }

        let (hits, compilations, entries) = device.pipelines.stats();
        assert_eq!(entries, 1);
        assert_eq!(
            hits + compilations,
            2,
            "each executor either compiled or reused the exact artifact"
        );
    }
}
