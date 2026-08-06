use std::collections::BTreeMap;
use std::sync::Arc;

use hl_descriptor::{
    DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM, DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectKind,
    OpenDescriptionImage,
};

/// Selects and namespaces codecs within the broad [`ObjectKind::File`] family.
///
/// Selection at capture time is based on authoritative codec ownership, while
/// restore dispatch is based only on the durable codec tag written here. This
/// avoids inferring a file subtype from metadata or payload shape.
pub struct FileObjectCatalog {
    codecs: BTreeMap<u32, Arc<dyn FileObjectCheckpoint>>,
}

pub trait FileObjectCheckpoint: DescriptorObjectCheckpoint {
    fn snapshot_size(
        &self,
        identity: u64,
        object: &dyn hl_descriptor::OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError>;
    fn owns(
        &self,
        identity: u64,
        object: &dyn hl_descriptor::OpenFileDescription,
    ) -> Result<bool, DescriptorCheckpointError>;
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
    ) -> Result<Arc<dyn hl_descriptor::OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::File || description.object.len() < 4 {
            return Err(DescriptorCheckpointError::Object);
        }
        let tag = u32::from_le_bytes(description.object[..4].try_into().unwrap());
        let codec = self.codecs.get(&tag).ok_or(DescriptorCheckpointError::Object)?;
        let mut inner = description.clone();
        inner.object = description.object[4..].to_vec();
        let object = codec.rebind(&inner)?;
        if object.kind() != ObjectKind::File {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(object)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use hl_descriptor::{OpenFileDescription, StatusFlags};

    #[derive(Debug)]
    struct File(u8);
    impl OpenFileDescription for File {
        fn kind(&self) -> ObjectKind {
            ObjectKind::File
        }
        fn domain_extension(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }
    }

    #[derive(Debug)]
    struct Directory;
    impl OpenFileDescription for Directory {
        fn kind(&self) -> ObjectKind {
            ObjectKind::Directory
        }
    }

    struct Codec {
        owner: u8,
        size: usize,
        wrong_kind: bool,
    }
    impl DescriptorObjectCheckpoint for Codec {
        fn snapshot(&self, _: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
            Ok(vec![self.owner; self.size])
        }
        fn rebind(&self, _: &OpenDescriptionImage) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
            if self.wrong_kind {
                Ok(Arc::new(Directory))
            } else {
                Ok(Arc::new(File(self.owner)))
            }
        }
    }
    impl FileObjectCheckpoint for Codec {
        fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
            Ok(self.size)
        }
        fn owns(&self, _: u64, object: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
            Ok(object
                .domain_extension()
                .and_then(|value| value.downcast_ref::<File>())
                .is_some_and(|file| file.0 == self.owner))
        }
    }
    fn image(object: Vec<u8>) -> OpenDescriptionImage {
        OpenDescriptionImage {
            identity: 1,
            generation: 1,
            offset: 0,
            status: StatusFlags::default(),
            kind: ObjectKind::File,
            object,
        }
    }

    #[test]
    fn selector_evidence() {
        let selected = FileObjectCatalog::rejecting()
            .bind(
                1,
                Arc::new(Codec {
                    owner: 7,
                    size: 1,
                    wrong_kind: false,
                }),
            )
            .bind(
                2,
                Arc::new(Codec {
                    owner: 8,
                    size: 1,
                    wrong_kind: false,
                }),
            );
        assert_eq!(&selected.snapshot(99, &File(7)).unwrap()[..4], &1_u32.to_le_bytes());
        assert_eq!(selected.snapshot(99, &File(9)), Err(DescriptorCheckpointError::Object));
        let ambiguous = FileObjectCatalog::rejecting()
            .bind(
                1,
                Arc::new(Codec {
                    owner: 7,
                    size: 1,
                    wrong_kind: false,
                }),
            )
            .bind(
                2,
                Arc::new(Codec {
                    owner: 7,
                    size: 1,
                    wrong_kind: false,
                }),
            );
        assert_eq!(ambiguous.snapshot(99, &File(7)), Err(DescriptorCheckpointError::Object));
    }

    #[test]
    fn bounds_and_kind() {
        let exact = FileObjectCatalog::rejecting().bind(
            1,
            Arc::new(Codec {
                owner: 7,
                size: DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM - 4,
                wrong_kind: false,
            }),
        );
        assert_eq!(
            exact.snapshot(1, &File(7)).unwrap().len(),
            DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM
        );
        for size in [DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM - 3, usize::MAX] {
            let over = FileObjectCatalog::rejecting().bind(
                1,
                Arc::new(Codec {
                    owner: 7,
                    size,
                    wrong_kind: false,
                }),
            );
            assert_eq!(over.snapshot(1, &File(7)), Err(DescriptorCheckpointError::Limit));
        }
        let wrong = FileObjectCatalog::rejecting().bind(
            1,
            Arc::new(Codec {
                owner: 7,
                size: 0,
                wrong_kind: true,
            }),
        );
        assert!(matches!(
            wrong.rebind(&image(1_u32.to_le_bytes().to_vec())),
            Err(DescriptorCheckpointError::Object)
        ));
    }
}
