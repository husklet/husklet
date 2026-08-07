use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{
    DescriptionIdentity, DescriptorCheckpointError, DescriptorError, DescriptorFlags, DescriptorObjectCheckpoint,
    DescriptorTable, ObjectError, OfdMetadata, OfdTimestamp, OpenDescriptionImage, OpenFileDescription, StatusFlags,
};
use hl_task::{NamespaceId, NamespaceKind};

/// Namespace file whose identity remains stable across descriptor duplication.
#[derive(Debug)]
pub struct NamespaceHandle {
    identifier: NamespaceId,
    binding: Mutex<Option<(Weak<NamespaceHandleRegistry>, DescriptionIdentity)>>,
}

impl NamespaceHandle {
    fn new(identifier: NamespaceId) -> Arc<Self> {
        Arc::new(Self {
            identifier,
            binding: Mutex::new(None),
        })
    }

    fn bind(&self, registry: Weak<NamespaceHandleRegistry>, identity: DescriptionIdentity) {
        *self.binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((registry, identity));
    }
}

impl OpenFileDescription for NamespaceHandle {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0x6e73,
            inode: self.identifier.serial,
            kind: 8,
            permissions: 0o444,
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
}

impl Drop for NamespaceHandle {
    fn drop(&mut self) {
        let binding = self
            .binding
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((registry, identity)) = binding
            && let Some(registry) = registry.upgrade()
        {
            registry.retire(identity);
        }
    }
}

/// Resolves namespace descriptors without exposing concrete objects to the FD domain.
#[derive(Debug, Default)]
pub struct NamespaceHandleRegistry {
    objects: Mutex<BTreeMap<DescriptionIdentity, Weak<NamespaceHandle>>>,
}

impl NamespaceHandleRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(
        self: &Arc<Self>,
        descriptors: &DescriptorTable,
        identifier: NamespaceId,
    ) -> Result<i32, DescriptorError> {
        let object = self.object(identifier);
        let opened = descriptors.prepare_open(
            0,
            object.clone(),
            StatusFlags::default(),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )?;
        let identity = opened.description_identity();
        self.bind(identity, &object).map_err(|_| DescriptorError::Corrupt)?;
        Ok(opened.publish())
    }

    #[must_use]
    pub fn object(self: &Arc<Self>, identifier: NamespaceId) -> Arc<NamespaceHandle> {
        NamespaceHandle::new(identifier)
    }

    pub fn bind(
        self: &Arc<Self>,
        identity: DescriptionIdentity,
        object: &Arc<NamespaceHandle>,
    ) -> Result<(), NamespaceHandleError> {
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if objects.contains_key(&identity) {
            return Err(NamespaceHandleError::Duplicate);
        }
        objects.insert(identity, Arc::downgrade(object));
        object.bind(Arc::downgrade(self), identity);
        Ok(())
    }

    pub fn identifier(&self, identity: DescriptionIdentity) -> Result<NamespaceId, NamespaceHandleError> {
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(object) = objects.get(&identity).and_then(Weak::upgrade) {
            Ok(object.identifier)
        } else {
            objects.remove(&identity);
            Err(NamespaceHandleError::NotNamespace)
        }
    }

    fn retire(&self, identity: DescriptionIdentity) {
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&identity);
    }

    fn encode(identifier: NamespaceId) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[0] = match identifier.kind {
            NamespaceKind::Uts => 1,
            NamespaceKind::Ipc => 2,
            NamespaceKind::Network => 3,
            NamespaceKind::Mount => 4,
            NamespaceKind::User => 5,
            NamespaceKind::Pid => 6,
        };
        bytes[8..].copy_from_slice(&identifier.serial.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<NamespaceId, NamespaceHandleError> {
        if bytes.len() != 16 || bytes[1..8] != [0; 7] {
            return Err(NamespaceHandleError::Invalid);
        }
        let kind = match bytes[0] {
            1 => NamespaceKind::Uts,
            2 => NamespaceKind::Ipc,
            3 => NamespaceKind::Network,
            4 => NamespaceKind::Mount,
            5 => NamespaceKind::User,
            6 => NamespaceKind::Pid,
            _ => return Err(NamespaceHandleError::Invalid),
        };
        let serial = u64::from_le_bytes(bytes[8..].try_into().map_err(|_| NamespaceHandleError::Invalid)?);
        if serial == 0 {
            return Err(NamespaceHandleError::Invalid);
        }
        Ok(NamespaceId { kind, serial })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceHandleError {
    Duplicate,
    NotNamespace,
    Invalid,
}

impl DescriptorObjectCheckpoint for NamespaceHandleRegistry {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(16)
    }

    fn snapshot_into(
        &self,
        identity: u64,
        _: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        if output.len() != 16 {
            return Err(DescriptorCheckpointError::Object);
        }
        let objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let identifier = objects
            .iter()
            .find(|(candidate, _)| candidate.identity == identity)
            .and_then(|(_, object)| object.upgrade())
            .map(|object| object.identifier)
            .ok_or(DescriptorCheckpointError::Object)?;
        output.copy_from_slice(&Self::encode(identifier));
        Ok(())
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        let identifier = Self::decode(&description.object).map_err(|_| DescriptorCheckpointError::Object)?;
        let identity = DescriptionIdentity {
            identity: description.identity,
            generation: description.generation,
        };
        let object = NamespaceHandle::new(identifier);
        let mut objects = self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if objects.contains_key(&identity) {
            return Err(DescriptorCheckpointError::Object);
        }
        objects.insert(identity, Arc::downgrade(&object));
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_alias() {
        let table = DescriptorTable::new(8).unwrap();
        let registry = Arc::new(NamespaceHandleRegistry::new());
        let identifier = NamespaceId {
            kind: hl_task::NamespaceKind::Uts,
            serial: 17,
        };
        let descriptor = registry.install(&table, identifier).unwrap();
        let alias = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
        let original = table.pin(descriptor).unwrap();
        let duplicated = table.pin(alias).unwrap();
        assert_eq!(original.description_identity(), duplicated.description_identity());
        assert_eq!(registry.identifier(duplicated.description_identity()), Ok(identifier));
        table.close(descriptor).unwrap();
        assert_eq!(registry.identifier(duplicated.description_identity()), Ok(identifier));
        table.close(alias).unwrap();
        assert_eq!(registry.identifier(duplicated.description_identity()), Ok(identifier));
        let identity = duplicated.description_identity();
        drop(original);
        drop(duplicated);
        assert_eq!(registry.identifier(identity), Err(NamespaceHandleError::NotNamespace),);
    }

    #[test]
    fn checkpoint_rebinds_generation() {
        let table = DescriptorTable::new(8).unwrap();
        let registry = Arc::new(NamespaceHandleRegistry::new());
        let identifier = NamespaceId {
            kind: NamespaceKind::Uts,
            serial: 17,
        };
        let descriptor = registry.install(&table, identifier).unwrap();
        let alias = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(registry.as_ref()).unwrap();
        table.thaw_checkpoint();

        let rebound = Arc::new(NamespaceHandleRegistry::new());
        let restored = DescriptorTable::restore_checkpoint(&image, rebound.as_ref()).unwrap();
        let original = restored.pin(descriptor).unwrap();
        let duplicate = restored.pin(alias).unwrap();
        assert_eq!(original.description_identity(), duplicate.description_identity());
        assert_eq!(rebound.identifier(original.description_identity()), Ok(identifier));

        let mut malformed = image;
        malformed.descriptions[0].object[1] = 1;
        let malformed_registry = Arc::new(NamespaceHandleRegistry::new());
        assert!(matches!(
            DescriptorTable::restore_checkpoint(&malformed, malformed_registry.as_ref()),
            Err(DescriptorCheckpointError::Object),
        ));
    }
}
