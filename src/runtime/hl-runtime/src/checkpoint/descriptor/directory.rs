use std::collections::BTreeMap;
use std::sync::Arc;

use hl_descriptor::{
    DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectKind, OpenDescriptionImage, OpenFileDescription,
};

/// Selects and namespaces codecs within the broad [`ObjectKind::Directory`] family.
pub struct DirectoryObjectCatalog {
    codecs: BTreeMap<u32, Arc<dyn DirectoryObjectCheckpoint>>,
}

pub trait DirectoryObjectCheckpoint: DescriptorObjectCheckpoint {
    fn owns(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError>;
}

impl DirectoryObjectCatalog {
    #[must_use]
    pub fn rejecting() -> Self {
        Self {
            codecs: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn bind(mut self, tag: u32, codec: Arc<dyn DirectoryObjectCheckpoint>) -> Self {
        assert!(tag != 0, "directory checkpoint codec tag zero is reserved");
        assert!(
            self.codecs.insert(tag, codec).is_none(),
            "duplicate directory checkpoint codec tag"
        );
        self
    }
}

impl DescriptorObjectCheckpoint for DirectoryObjectCatalog {
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        if object.kind() != ObjectKind::Directory {
            return Err(DescriptorCheckpointError::Object);
        }
        let mut selected = None;
        for (tag, codec) in &self.codecs {
            if !codec.owns(identity, object)? {
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
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::Directory || description.object.len() < 4 {
            return Err(DescriptorCheckpointError::Object);
        }
        let tag = u32::from_le_bytes(description.object[..4].try_into().unwrap());
        let codec = self.codecs.get(&tag).ok_or(DescriptorCheckpointError::Object)?;
        let mut inner = description.clone();
        inner.object = description.object[4..].to_vec();
        codec.rebind(&inner)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Debug)]
    struct Directory;

    impl OpenFileDescription for Directory {
        fn kind(&self) -> ObjectKind {
            ObjectKind::Directory
        }
    }

    struct Codec {
        owner: u64,
    }

    impl DescriptorObjectCheckpoint for Codec {
        fn snapshot(&self, identity: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
            Ok(vec![identity as u8])
        }

        fn rebind(
            &self,
            description: &OpenDescriptionImage,
        ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
            (description.object == [self.owner as u8])
                .then(|| Arc::new(Directory) as Arc<dyn OpenFileDescription>)
                .ok_or(DescriptorCheckpointError::Object)
        }
    }

    impl DirectoryObjectCheckpoint for Codec {
        fn owns(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
            assert_eq!(object.kind(), ObjectKind::Directory);
            Ok(identity == self.owner)
        }
    }

    fn image(object: Vec<u8>) -> OpenDescriptionImage {
        OpenDescriptionImage {
            identity: 7,
            generation: 1,
            offset: 0,
            status: hl_descriptor::StatusFlags::default(),
            kind: ObjectKind::Directory,
            object,
        }
    }

    #[test]
    fn unique_owner_tags_snapshot_and_restore() {
        let catalog = DirectoryObjectCatalog::rejecting().bind(9, Arc::new(Codec { owner: 7 }));
        let encoded = catalog.snapshot(7, &Directory).unwrap();
        assert_eq!(&encoded[..4], &9_u32.to_le_bytes());
        assert_eq!(catalog.rebind(&image(encoded)).unwrap().kind(), ObjectKind::Directory);
    }

    #[test]
    fn missing_ambiguous_and_unknown_owners_are_rejected() {
        assert_eq!(
            DirectoryObjectCatalog::rejecting().snapshot(7, &Directory),
            Err(DescriptorCheckpointError::Object)
        );
        let ambiguous = DirectoryObjectCatalog::rejecting()
            .bind(1, Arc::new(Codec { owner: 7 }))
            .bind(2, Arc::new(Codec { owner: 7 }));
        assert_eq!(
            ambiguous.snapshot(7, &Directory),
            Err(DescriptorCheckpointError::Object)
        );
        assert!(
            DirectoryObjectCatalog::rejecting()
                .bind(1, Arc::new(Codec { owner: 7 }))
                .rebind(&image(99_u32.to_le_bytes().to_vec()))
                .is_err()
        );
    }

    #[test]
    #[should_panic(expected = "duplicate directory checkpoint codec tag")]
    fn duplicate_tag_panics() {
        let _ = DirectoryObjectCatalog::rejecting()
            .bind(1, Arc::new(Codec { owner: 7 }))
            .bind(1, Arc::new(Codec { owner: 8 }));
    }
}
