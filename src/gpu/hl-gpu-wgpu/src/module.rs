//! Device-scoped residency for immutable compiled shader modules.
//!
//! Protocol executors keep guest ids and live-alias accounting locally. This cache sits one level lower:
//! executors created from the same [`crate::Device`] may reuse an exact compiled module after an earlier
//! connection has gone away. Full [`ShaderKey`] equality decides identity; the hash is only an index.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

use crate::dedup::ShaderKey;
use crate::reflect::ModuleUsage;

const CAPACITY: usize = 256;

#[derive(Clone)]
struct Compiled {
    module: wgpu::ShaderModule,
    reflected: ModuleUsage,
}

struct Entry<V> {
    value: V,
    used: u64,
}

struct Residency<K, V> {
    entries: HashMap<K, Entry<V>>,
    capacity: usize,
    clock: u64,
}

impl<K: Clone + Eq + Hash, V: Clone> Residency<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            capacity,
            clock: 0,
        }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.entries.get(key).map(|entry| entry.value.clone())
    }

    fn retain(&mut self, key: K, value: V) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = self.clock;
            return;
        }
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() == self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            Entry {
                value,
                used: self.clock,
            },
        );
    }

    fn touch(&mut self, key: &K) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.used = self.clock;
        }
    }
}

/// Bounded modules compiled by one host device and reusable by its isolated executors.
pub(crate) struct Modules {
    residency: Mutex<Residency<ShaderKey, Compiled>>,
    #[cfg(test)]
    hits: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    misses: std::sync::atomic::AtomicU64,
}

pub(crate) enum Mutation {
    Hit(ShaderKey),
    Install(ShaderKey, wgpu::ShaderModule, ModuleUsage),
}

impl Modules {
    pub(crate) fn new() -> Self {
        Self {
            residency: Mutex::new(Residency::new(CAPACITY)),
            #[cfg(test)]
            hits: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Observe an immutable compiled artifact without changing eviction order. The caller records the hit
    /// only after its guest id insertion succeeds, so a rejected duplicate id cannot perturb residency.
    pub(crate) fn get(&self, key: &ShaderKey) -> Option<(wgpu::ShaderModule, ModuleUsage)> {
        let residency = self
            .residency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        residency
            .get(key)
            .map(|compiled| (compiled.module, compiled.reflected))
    }

    pub(crate) fn hit(&self, key: &ShaderKey) {
        let mut residency = self
            .residency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        residency.touch(key);
        #[cfg(test)]
        self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Retain a successfully accepted compilation. Entries are immutable and exact-keyed, so retaining
    /// one after a later command in the protocol batch fails cannot expose guest state or poison a retry.
    pub(crate) fn install(
        &self,
        key: ShaderKey,
        module: wgpu::ShaderModule,
        reflected: ModuleUsage,
    ) {
        let mut residency = self
            .residency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        residency.retain(key, Compiled { module, reflected });
        #[cfg(test)]
        self.misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn apply(&self, mutations: Vec<Mutation>) {
        for mutation in mutations {
            match mutation {
                Mutation::Hit(key) => self.hit(&key),
                Mutation::Install(key, module, reflected) => {
                    self.install(key, module, reflected);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> (u64, u64, usize) {
        use std::sync::atomic::Ordering;

        let residency = self
            .residency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            residency.entries.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use hl_gpu::protocol::model::command::ShaderPayloadKind;
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::{Cmd, GpuExecutor, SessionResources};

    use super::Residency;
    use crate::{Device, DeviceConfig};

    #[test]
    fn eviction_is_bounded_and_least_recently_used() {
        let mut residency = Residency::new(2);
        residency.retain("first", 1);
        residency.retain("second", 2);
        residency.touch(&"first");
        residency.retain("third", 3);

        assert_eq!(residency.entries.len(), 2);
        assert_eq!(residency.get(&"first"), Some(1));
        assert_eq!(residency.get(&"second"), None);
        assert_eq!(residency.get(&"third"), Some(3));
    }

    #[test]
    fn exact_keys_do_not_alias() {
        let mut residency = Residency::new(2);
        residency.retain(vec![1, 2, 3], "a");
        residency.retain(vec![1, 2, 4], "b");

        assert_eq!(residency.get(&vec![1, 2, 3]), Some("a"));
        assert_eq!(residency.get(&vec![1, 2, 4]), Some("b"));
    }

    fn glsl(source: &str) -> Vec<u32> {
        GlslDescriptor {
            stage: glsl_stage::FRAGMENT,
            entry: "main".to_owned(),
            source: source.to_owned(),
        }
        .to_words()
    }

    #[test]
    fn sequential_executors_reuse_one_device_compilation() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let words =
            glsl("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){color=vec4(1.0);}");

        let mut first = device.executor();
        let mut first_resources = SessionResources::default();
        first
            .execute(
                &mut first_resources,
                &[Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: words.clone(),
                }],
            )
            .unwrap();
        first
            .execute(&mut first_resources, &[Cmd::DestroyShader(1)])
            .unwrap();
        drop(first);

        let mut second = device.executor();
        let mut second_resources = SessionResources::default();
        second
            .execute(
                &mut second_resources,
                &[Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: words,
                }],
            )
            .unwrap();

        assert_eq!(
            device.modules.stats(),
            (1, 1, 1),
            "the first connection compiles cold; the next exact source is a device-cache hit"
        );
        assert_eq!(second.shader_backing_count(), 1);
    }

    #[test]
    fn rejected_duplicate_id_does_not_touch_device_residency() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let cached =
            glsl("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){color=vec4(1.0);}");
        let other =
            glsl("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){color=vec4(0.0);}");

        let mut seed = device.executor();
        seed.execute(
            &mut SessionResources::default(),
            &[Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: cached.clone(),
            }],
        )
        .unwrap();

        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: other,
                }],
            )
            .unwrap();
        let before = device.modules.stats();
        assert!(executor
            .execute(
                &mut resources,
                &[Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: cached,
                }],
            )
            .is_err());

        assert_eq!(
            device.modules.stats(),
            before,
            "a rejected guest-id insertion cannot count or promote a cache hit"
        );
    }

    #[test]
    fn failed_batch_does_not_publish_compilations() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let words =
            glsl("#version 460\nlayout(location=0) out vec4 color;\nvoid main(){color=vec4(0.5);}");
        let before = device.modules.stats();
        let mut executor = device.executor();

        assert!(executor
            .execute(
                &mut SessionResources::default(),
                &[
                    Cmd::CreateShader {
                        id: 1,
                        kind: ShaderPayloadKind::Glsl,
                        spirv: words,
                    },
                    Cmd::DestroyShader(99),
                ],
            )
            .is_err());
        assert_eq!(
            device.modules.stats(),
            before,
            "a compilation from a rolled-back batch must not enter device residency"
        );
    }
}
