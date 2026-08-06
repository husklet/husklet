use std::collections::BTreeMap;
use std::sync::Arc;

use hl_descriptor::{
    DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM, DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectKind,
    OpenDescriptionImage, OpenFileDescription,
};

/// Selects and namespaces codecs within the broad [`ObjectKind::Directory`] family.
pub struct DirectoryObjectCatalog {
    codecs: BTreeMap<u32, Arc<dyn DirectoryObjectCheckpoint>>,
}

pub trait DirectoryObjectCheckpoint: DescriptorObjectCheckpoint {
    fn snapshot_size(
        &self,
        identity: u64,
        object: &dyn OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError>;

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
        let payload_size = codec.snapshot_size(identity, object)?;
        let encoded_size = 4_usize
            .checked_add(payload_size)
            .ok_or(DescriptorCheckpointError::Limit)?;
        if encoded_size > DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
            return Err(DescriptorCheckpointError::Limit);
        }
        let payload = codec.snapshot(identity, object)?;
        if payload.len() != payload_size {
            return Err(DescriptorCheckpointError::Object);
        }
        let mut encoded = Vec::with_capacity(encoded_size);
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
        let object = codec.rebind(&inner)?;
        if object.kind() != ObjectKind::Directory {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(object)
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
        fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
            Ok(1)
        }

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
    fn selectors_roundtrip() {
        let catalog = DirectoryObjectCatalog::rejecting().bind(9, Arc::new(Codec { owner: 7 }));
        let encoded = catalog.snapshot(7, &Directory).unwrap();
        assert_eq!(&encoded[..4], &9_u32.to_le_bytes());
        assert_eq!(catalog.rebind(&image(encoded)).unwrap().kind(), ObjectKind::Directory);
    }

    #[test]
    fn selector_rejections() {
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

    struct BoundCodec(usize);

    impl DescriptorObjectCheckpoint for BoundCodec {
        fn snapshot(&self, _: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
            Ok(vec![0; self.0])
        }

        fn rebind(&self, _: &OpenDescriptionImage) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
            Ok(Arc::new(Directory))
        }
    }

    impl DirectoryObjectCheckpoint for BoundCodec {
        fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
            Ok(self.0)
        }

        fn owns(&self, _: u64, _: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
            Ok(true)
        }
    }

    #[test]
    fn bounded_envelope() {
        let exact =
            DirectoryObjectCatalog::rejecting().bind(1, Arc::new(BoundCodec(DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM - 4)));
        assert_eq!(
            exact.snapshot(7, &Directory).unwrap().len(),
            DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM
        );
        for size in [DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM - 3, usize::MAX] {
            let over = DirectoryObjectCatalog::rejecting().bind(1, Arc::new(BoundCodec(size)));
            assert_eq!(over.snapshot(7, &Directory), Err(DescriptorCheckpointError::Limit));
        }
    }

    #[derive(Debug)]
    struct File;

    impl OpenFileDescription for File {
        fn kind(&self) -> ObjectKind {
            ObjectKind::File
        }
    }

    struct WrongKind;

    impl DescriptorObjectCheckpoint for WrongKind {
        fn snapshot(&self, _: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
            Ok(Vec::new())
        }

        fn rebind(&self, _: &OpenDescriptionImage) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
            Ok(Arc::new(File))
        }
    }

    impl DirectoryObjectCheckpoint for WrongKind {
        fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
            Ok(0)
        }

        fn owns(&self, _: u64, _: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
            Ok(true)
        }
    }

    #[test]
    fn restored_kind() {
        let catalog = DirectoryObjectCatalog::rejecting().bind(1, Arc::new(WrongKind));
        assert!(matches!(
            catalog.rebind(&image(1_u32.to_le_bytes().to_vec())),
            Err(DescriptorCheckpointError::Object)
        ));
    }
}
