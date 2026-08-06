use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{
    DescriptionIdentity, DescriptionRef, DescriptorCheckpointError, DescriptorObjectCheckpoint, DescriptorTable,
    ObjectError, OfdMetadata, OfdTimestamp, OpenDescriptionImage, OpenFileDescription, Readiness, ReadinessObserver,
    ReadinessRegistry, ReadinessSubscription,
};
use hl_task::{ForkEntityId, ProcessId};

#[derive(Debug, Default)]
pub struct ProcessHandleRegistry {
    objects: Mutex<BTreeMap<DescriptionIdentity, Weak<ProcessHandle>>>,
    files: Mutex<BTreeMap<ProcessId, Weak<DescriptorTable>>>,
}

impl ProcessHandleRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn create(target: ProcessId) -> Arc<ProcessHandle> {
        Arc::new(ProcessHandle {
            target,
            binding: Mutex::new(None),
            exited: AtomicBool::new(false),
            readiness: ReadinessRegistry::new(),
        })
    }

    pub fn notify_exit(&self, target: ProcessId) {
        let objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let handles = objects
            .values()
            .filter_map(Weak::upgrade)
            .filter(|handle| handle.target == target)
            .collect::<Vec<_>>();
        drop(objects);
        for handle in handles {
            handle.mark_exited();
        }
    }

    /// Publishes the descriptor table currently owned by a process.
    ///
    /// The weak binding does not extend process or table lifetime. Rebinding is
    /// intentional across exec and descriptor-table replacement.
    pub fn register_files(&self, process: ProcessId, table: &Arc<DescriptorTable>) {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(process, Arc::downgrade(table));
    }

    pub fn export(&self, process: ProcessId, descriptor: i32) -> Result<DescriptionRef, ProcessHandleError> {
        let mut files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let table = match files.get(&process).and_then(Weak::upgrade) {
            Some(table) => table,
            None => {
                files.remove(&process);
                return Err(ProcessHandleError::MissingFiles);
            }
        };
        drop(files);
        table
            .export_description(descriptor)
            .map_err(|_| ProcessHandleError::BadDescriptor)
    }

    pub fn register(
        self: &Arc<Self>,
        identity: DescriptionIdentity,
        object: Arc<ProcessHandle>,
    ) -> Result<(), ProcessHandleError> {
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if objects.insert(identity, Arc::downgrade(&object)).is_some() {
            return Err(ProcessHandleError::Duplicate);
        }
        object.bind(Arc::downgrade(self), identity);
        Ok(())
    }

    pub fn target(&self, identity: DescriptionIdentity) -> Result<ProcessId, ProcessHandleError> {
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match objects.get(&identity).and_then(Weak::upgrade) {
            Some(object) => Ok(object.target),
            None => {
                objects.remove(&identity);
                Err(ProcessHandleError::Missing)
            }
        }
    }

    pub fn encode(&self, identity: DescriptionIdentity) -> Result<[u8; 8], ProcessHandleError> {
        Ok(Self::encode_target(self.target(identity)?))
    }

    fn encode_target(target: ProcessId) -> [u8; 8] {
        let target = target.fork_identity();
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&target.slot.to_le_bytes());
        bytes[4..].copy_from_slice(&target.generation.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Arc<ProcessHandle>, ProcessHandleError> {
        if bytes.len() != 8 {
            return Err(ProcessHandleError::Invalid);
        }
        let identity = ForkEntityId {
            slot: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            generation: u32::from_le_bytes(bytes[4..].try_into().unwrap()),
        };
        ProcessId::from_fork_identity(identity)
            .map(Self::create)
            .ok_or(ProcessHandleError::Invalid)
    }

    fn retire(&self, identity: DescriptionIdentity) {
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&identity);
    }
}

#[derive(Debug)]
pub struct ProcessHandle {
    target: ProcessId,
    binding: Mutex<Option<(Weak<ProcessHandleRegistry>, DescriptionIdentity)>>,
    exited: AtomicBool,
    readiness: ReadinessRegistry,
}

impl ProcessHandle {
    fn bind(&self, registry: Weak<ProcessHandleRegistry>, identity: DescriptionIdentity) {
        *self.binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((registry, identity));
    }

    fn mark_exited(&self) {
        if !self.exited.swap(true, Ordering::AcqRel) {
            self.readiness.notify();
        }
    }
}

impl OpenFileDescription for ProcessHandle {
    fn readiness(&self, interests: Readiness) -> Readiness {
        if self.exited.load(Ordering::Acquire) && interests.contains(Readiness::READ) {
            Readiness::from_bits(Readiness::READ)
        } else {
            Readiness::default()
        }
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.readiness.subscribe(observer)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0,
            inode: u64::from(self.target.number()),
            kind: 8,
            permissions: 0o600,
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn retire(&self) {
        let binding = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((registry, identity)) = binding {
            if let Some(registry) = registry.upgrade() {
                registry.retire(identity);
            }
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let binding = self
            .binding
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((registry, identity)) = binding {
            if let Some(registry) = registry.upgrade() {
                registry.retire(identity);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessHandleError {
    Duplicate,
    Missing,
    MissingFiles,
    BadDescriptor,
    Invalid,
}

impl DescriptorObjectCheckpoint for ProcessHandleRegistry {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(8)
    }

    fn snapshot_into(
        &self,
        identity: u64,
        _: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        if output.len() != 8 {
            return Err(DescriptorCheckpointError::Object);
        }
        let objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let found = objects
            .iter()
            .find(|(key, _)| key.identity == identity)
            .and_then(|(_, object)| object.upgrade())
            .ok_or(DescriptorCheckpointError::Object)?;
        output.copy_from_slice(&Self::encode_target(found.target));
        Ok(())
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        let object = Self::decode(&description.object).map_err(|_| DescriptorCheckpointError::Object)?;
        let identity = DescriptionIdentity {
            identity: description.identity,
            generation: description.generation,
        };
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        objects.insert(identity, Arc::downgrade(&object));
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_descriptor::{DescriptorFlags, DescriptorTable, StatusFlags};
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

    #[test]
    fn identity_lifecycle() {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (target, _) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 32).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        let table = DescriptorTable::new(4).unwrap();
        let registry = Arc::new(ProcessHandleRegistry::new());
        let object = ProcessHandleRegistry::create(target);
        let install = table
            .prepare_open(
                0,
                object.clone(),
                StatusFlags::default(),
                DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
            )
            .unwrap();
        let identity = install.description_identity();
        registry.register(identity, object).unwrap();
        let descriptor = install.publish();
        let duplicate = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
        assert_eq!(
            registry.target(table.pin(duplicate).unwrap().description_identity()),
            Ok(target)
        );
        assert_eq!(
            table.snapshot(descriptor).unwrap().flags.bits(),
            DescriptorFlags::CLOSE_ON_EXEC
        );
        table.close(descriptor).unwrap();
        assert_eq!(registry.target(identity), Ok(target));
        table.close(duplicate).unwrap();
        assert_eq!(registry.target(identity), Err(ProcessHandleError::Missing));
    }

    #[test]
    fn child_exit_becomes_readable() {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (target, _) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 32).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        let registry = Arc::new(ProcessHandleRegistry::new());
        let object = ProcessHandleRegistry::create(target);
        let table = DescriptorTable::new(1).unwrap();
        let install = table
            .prepare_open(0, object.clone(), StatusFlags::default(), DescriptorFlags::default())
            .unwrap();
        registry
            .register(install.description_identity(), object.clone())
            .unwrap();
        assert_eq!(
            object.readiness(Readiness::from_bits(Readiness::READ)),
            Readiness::default()
        );
        registry.notify_exit(target);
        assert_eq!(
            object.readiness(Readiness::from_bits(Readiness::READ)),
            Readiness::from_bits(Readiness::READ),
        );
    }

    #[test]
    fn checkpoint_codec() {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (target, _) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 32).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        let registry = Arc::new(ProcessHandleRegistry::new());
        let identity = DescriptionIdentity {
            identity: 7,
            generation: 3,
        };
        let object = ProcessHandleRegistry::create(target);
        registry.register(identity, object.clone()).unwrap();
        let bytes = registry.encode(identity).unwrap();
        let restored = ProcessHandleRegistry::decode(&bytes).unwrap();
        assert_eq!(restored.target, target);
        assert!(matches!(
            ProcessHandleRegistry::decode(&bytes[..7]),
            Err(ProcessHandleError::Invalid)
        ));

        let table = DescriptorTable::new(4).unwrap();
        let object = ProcessHandleRegistry::create(target);
        let install = table
            .prepare_open(0, object.clone(), StatusFlags::default(), DescriptorFlags::default())
            .unwrap();
        let identity = install.description_identity();
        registry.register(identity, object).unwrap();
        let descriptor = install.publish();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(registry.as_ref()).unwrap();
        table.thaw_checkpoint();
        let rebound = Arc::new(ProcessHandleRegistry::new());
        let restored = DescriptorTable::restore_checkpoint(&image, rebound.as_ref()).unwrap();
        let identity = restored.pin(descriptor).unwrap().description_identity();
        assert_eq!(rebound.target(identity), Ok(target));
    }
}
