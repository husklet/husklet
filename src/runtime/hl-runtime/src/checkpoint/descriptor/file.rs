use std::collections::BTreeMap;
use std::sync::Arc;

use hl_descriptor::{DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectKind, OpenDescriptionImage};

/// Selects and namespaces codecs within the broad [`ObjectKind::File`] family.
///
/// Selection at capture time is based on authoritative codec ownership, while
/// restore dispatch is based only on the durable codec tag written here. This
/// avoids inferring a file subtype from metadata or payload shape.
pub struct FileObjectCatalog {
    codecs: BTreeMap<u32, Arc<dyn FileObjectCheckpoint>>,
}

pub trait FileObjectCheckpoint: DescriptorObjectCheckpoint {
    fn owns(&self, identity: u64) -> Result<bool, DescriptorCheckpointError>;
}

impl FileObjectCatalog {
    #[must_use]
    pub fn rejecting() -> Self {
        Self {
            codecs: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn bind(mut self, tag: u32, codec: Arc<dyn FileObjectCheckpoint>) -> Self {
        assert!(tag != 0, "file checkpoint codec tag zero is reserved");
        assert!(
            self.codecs.insert(tag, codec).is_none(),
            "duplicate file checkpoint codec tag"
        );
        self
    }
}

impl DescriptorObjectCheckpoint for FileObjectCatalog {
    fn snapshot(
        &self,
        identity: u64,
        object: &dyn hl_descriptor::OpenFileDescription,
    ) -> Result<Vec<u8>, DescriptorCheckpointError> {
        if object.kind() != ObjectKind::File {
            return Err(DescriptorCheckpointError::Object);
        }
        let mut selected = None;
        for (tag, codec) in &self.codecs {
            if !codec.owns(identity)? {
                continue;
            }
            if selected.is_some() {
                return Err(DescriptorCheckpointError::Object);
            }
            selected = Some((*tag, codec));
        }
        let (tag, codec) = selected.ok_or(DescriptorCheckpointError::Object)?;
        let payload = codec.snapshot(identity, object)?;
        let mut encoded = Vec::with_capacity(4 + payload.len());
        encoded.extend_from_slice(&tag.to_le_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded)
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn hl_descriptor::OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::File || description.object.len() < 4 {
            return Err(DescriptorCheckpointError::Object);
        }
        let tag = u32::from_le_bytes(description.object[..4].try_into().unwrap());
        let codec = self.codecs.get(&tag).ok_or(DescriptorCheckpointError::Object)?;
        let mut inner = description.clone();
        inner.object = description.object[4..].to_vec();
        codec.rebind(&inner)
    }
}
