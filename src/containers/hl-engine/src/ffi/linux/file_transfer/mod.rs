use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::DescriptionIdentity;
use hl_runtime::{ImportedDescription, ImportedTransfer, RuntimeNetworkError, TransferPublication};

mod imported;
mod cursor;

use imported::ImportedFile;

trait FileCapability: Send + Sync {
    fn duplicate(&self) -> Result<OwnedFd, RuntimeNetworkError>;
    fn mapping(&self) -> Result<File, RuntimeNetworkError>;
}

#[derive(Default)]
pub(in crate::ffi::linux) struct FileTransferRegistry {
    files: Mutex<BTreeMap<DescriptionIdentity, Weak<dyn FileCapability>>>,
}

impl fmt::Debug for FileTransferRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let files = self.files.lock().map_err(|_| fmt::Error)?;
        formatter
            .debug_struct("FileTransferRegistry")
            .field("files", &files.len())
            .finish()
    }
}

impl FileTransferRegistry {
    pub(in crate::ffi::linux) fn duplicate(
        &self,
        identity: DescriptionIdentity,
    ) -> Result<OwnedFd, RuntimeNetworkError> {
        self.capability(identity)?.duplicate()
    }

    pub(super) fn mapping(&self, identity: DescriptionIdentity) -> Option<File> {
        self.capability(identity).ok()?.mapping().ok()
    }

    pub(in crate::ffi::linux) fn import(
        self: &Arc<Self>,
        descriptor: OwnedFd,
    ) -> Result<ImportedTransfer, RuntimeNetworkError> {
        let file = Arc::new(ImportedFile::new(descriptor)?);
        let description = ImportedDescription {
            object: file.clone(),
            status: file.status,
        };
        Ok(ImportedTransfer::new(
            vec![description],
            Box::new(FilePublication {
                registry: Arc::clone(self),
                file,
                identity: None,
            }),
        ))
    }

    fn capability(&self, identity: DescriptionIdentity) -> Result<Arc<dyn FileCapability>, RuntimeNetworkError> {
        self.files
            .lock()
            .map_err(|_| RuntimeNetworkError::Failed)?
            .get(&identity)
            .and_then(Weak::upgrade)
            .ok_or(RuntimeNetworkError::Unsupported)
    }

    fn bind_import(&self, identity: DescriptionIdentity, file: Arc<ImportedFile>) -> Result<(), RuntimeNetworkError> {
        let capability: Arc<dyn FileCapability> = file;
        let mut files = self.files.lock().map_err(|_| RuntimeNetworkError::Failed)?;
        files.retain(|_, file| file.strong_count() != 0);
        if files.len() >= 4096 {
            return Err(RuntimeNetworkError::NoMemory);
        }
        match files.entry(identity) {
            Entry::Vacant(slot) => {
                slot.insert(Arc::downgrade(&capability));
            }
            Entry::Occupied(_) => return Err(RuntimeNetworkError::Invalid),
        }
        Ok(())
    }

    fn remove(&self, identity: DescriptionIdentity) {
        if let Ok(mut files) = self.files.lock() {
            files.remove(&identity);
        }
    }
}

struct FilePublication {
    registry: Arc<FileTransferRegistry>,
    file: Arc<ImportedFile>,
    identity: Option<DescriptionIdentity>,
}

impl TransferPublication for FilePublication {
    fn bind(&mut self, identities: &[DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        let [identity] = identities else {
            return Err(RuntimeNetworkError::Invalid);
        };
        self.registry.bind_import(*identity, Arc::clone(&self.file))?;
        self.identity = Some(*identity);
        Ok(())
    }

    fn commit(mut self: Box<Self>) {
        self.identity = None;
    }

    fn rollback(mut self: Box<Self>) {
        self.unbind();
    }
}

impl FilePublication {
    fn unbind(&mut self) {
        if let Some(identity) = self.identity.take() {
            self.registry.remove(identity);
        }
    }
}

impl Drop for FilePublication {
    fn drop(&mut self) {
        self.unbind();
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Read, Seek, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_descriptor::DescriptorTable;
    use hl_runtime::TransferCommitError;

    use super::FileTransferRegistry;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryFile(std::path::PathBuf);

    impl TemporaryFile {
        fn create() -> (Self, std::fs::File) {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("hl-file-transfer-{}-{sequence}", std::process::id()));
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(b"abcdef").unwrap();
            file.rewind().unwrap();
            (Self(path), file)
        }
    }

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn publish(registry: &Arc<FileTransferRegistry>, receiver: std::fs::File) -> (DescriptorTable, i32) {
        let table = DescriptorTable::new(4).unwrap();
        let transfer = registry.import(OwnedFd::from(receiver)).unwrap();
        let descriptors = transfer
            .prepare(&table, true)
            .unwrap()
            .publish_after(|_| Ok::<_, ()>(()))
            .unwrap();
        (table, descriptors[0])
    }

    #[test]
    fn sender_offset() {
        let (_temporary, sender) = TemporaryFile::create();
        let receiver = sender.try_clone().unwrap();
        drop(sender);
        let registry = Arc::new(FileTransferRegistry::default());
        let (table, number) = publish(&registry, receiver);
        let lease = table.pin(number).unwrap();
        let identity = lease.description_identity();
        let mut first = [0_u8; 2];
        assert_eq!(lease.read(&mut first), Ok(2));
        assert_eq!(&first, b"ab");

        let exported = registry.duplicate(identity).unwrap();
        // SAFETY: F_GETFD only observes the live owned descriptor.
        let flags = unsafe { libc::fcntl(exported.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let mut duplicate = std::fs::File::from(exported);
        let mut second = [0_u8; 2];
        duplicate.read_exact(&mut second).unwrap();
        assert_eq!(&second, b"cd");
    }

    #[test]
    fn mapping_identity() {
        let (_temporary, sender) = TemporaryFile::create();
        let registry = Arc::new(FileTransferRegistry::default());
        let (table, number) = publish(&registry, sender.try_clone().unwrap());
        drop(sender);
        let lease = table.pin(number).unwrap();
        let expected = lease.metadata().unwrap();
        let mapping = registry.mapping(lease.description_identity()).unwrap();
        let actual = mapping.metadata().unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!((actual.dev(), actual.ino()), (expected.device, expected.inode));
    }

    #[test]
    fn copyout_rollback() {
        let (_temporary, sender) = TemporaryFile::create();
        let registry = Arc::new(FileTransferRegistry::default());
        let table = DescriptorTable::new(4).unwrap();
        let transfer = registry.import(OwnedFd::from(sender.try_clone().unwrap())).unwrap();
        let result = transfer.prepare(&table, false).unwrap().publish_after(|_| Err("fault"));
        assert_eq!(result, Err(TransferCommitError::Copyout("fault")));
        assert!(registry.files.lock().unwrap().is_empty());
        assert!(table.pin(0).is_err());
    }
}
