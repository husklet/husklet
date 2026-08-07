use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_runtime::{DescriptorMappingSource, RuntimeMemoryError};

use super::{FileTransferRegistry, NativeFile, NativePath, lease, projected};
use crate::ffi::linux::execution::VirtualMemory;

#[derive(Debug)]
pub(in crate::ffi::linux::execution) struct MappingFiles {
    arena: Arc<VirtualMemory>,
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    projected: projected::Registry,
    transferred: Arc<FileTransferRegistry>,
    next_device_mapping: AtomicU64,
}

#[derive(Clone)]
pub(in crate::ffi::linux::execution) struct MappingPaths {
    paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
    projected: projected::Registry,
}

impl MappingFiles {
    pub(super) fn new(
        arena: Arc<VirtualMemory>,
        paths: Arc<Mutex<BTreeMap<(u64, u64), lease::LeaseEntry>>>,
        projected: projected::Registry,
        transferred: Arc<FileTransferRegistry>,
    ) -> Self {
        Self {
            arena,
            paths,
            projected,
            transferred,
            next_device_mapping: AtomicU64::new(1_u64 << 63),
        }
    }
}

impl NativePath {
    pub(in crate::ffi::linux::execution) fn mapping_source(&self, arena: Arc<VirtualMemory>) -> MappingFiles {
        MappingFiles::new(
            arena,
            Arc::clone(&self.paths),
            self.projected.clone(),
            Arc::clone(&self.transfers),
        )
    }

    pub(in crate::ffi::linux::execution) fn mapping_paths(&self) -> MappingPaths {
        MappingPaths {
            paths: Arc::clone(&self.paths),
            projected: self.projected.clone(),
        }
    }
}

impl MappingPaths {
    pub(in crate::ffi::linux::execution) fn path(&self, identity: (u64, u64)) -> Option<Vec<u8>> {
        if let Some(path) = self
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity)
            .map(|entry| entry.guest.as_str().as_bytes().to_vec())
        {
            return Some(path);
        }
        self.projected
            .get(&identity)
            .and_then(|file| file.guest().ok())
            .map(|path| path.as_str().as_bytes().to_vec())
    }
}

impl DescriptorMappingSource for MappingFiles {
    fn backing(
        &self,
        descriptor: &hl_descriptor::OperationLease,
        _offset: u64,
        _length: u64,
        shared: bool,
        _writable: bool,
    ) -> Result<hl_memory::Backing, RuntimeMemoryError> {
        let description = descriptor.description_identity();
        let metadata = descriptor.metadata().map_err(|_| RuntimeMemoryError::BadDescriptor)?;
        if let Some(backing) = zero_device_backing(&metadata, &self.next_device_mapping, shared) {
            return Ok(backing);
        }
        let identity = (metadata.device, metadata.inode);
        if let Some(file) = self.transferred.mapping(description) {
            self.arena
                .register_file(
                    hl_memory::FileIdentity {
                        device: identity.0,
                        object: identity.1,
                    },
                    &file,
                )
                .map_err(|_| RuntimeMemoryError::NoMemory)?;
            return Ok(hl_memory::Backing::File {
                identity: hl_memory::FileIdentity {
                    device: identity.0,
                    object: identity.1,
                },
                shared,
            });
        }
        let native = self
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity)
            .and_then(|opened| opened.file.upgrade());
        if let Some(file) = native {
            self.register(identity, &file)?;
        } else {
            let projected = self.projected.get(&identity).ok_or(RuntimeMemoryError::BadDescriptor)?;
            let file = projected.mapping().map_err(|_| RuntimeMemoryError::Failed)?;
            self.arena
                .register_file(
                    hl_memory::FileIdentity {
                        device: identity.0,
                        object: identity.1,
                    },
                    &file,
                )
                .map_err(|_| RuntimeMemoryError::NoMemory)?;
        }
        Ok(hl_memory::Backing::File {
            identity: hl_memory::FileIdentity {
                device: identity.0,
                object: identity.1,
            },
            shared,
        })
    }
}

fn zero_device_backing(
    metadata: &hl_descriptor::OfdMetadata,
    next: &AtomicU64,
    shared: bool,
) -> Option<hl_memory::Backing> {
    let device = hl_runtime::DeviceId::from_linux_encoded(metadata.special_device);
    if metadata.kind != 2 || !matches!(device, hl_runtime::DeviceId { major: 1, minor: 5 | 7 }) {
        return None;
    }
    // Linux gives each /dev/zero-style mmap fresh zero storage. MAP_SHARED
    // makes that one mapping coherent across fork; it does not alias a later
    // mmap of the same device. Device offsets never bound content.
    Some(hl_memory::Backing::Anonymous {
        identity: next.fetch_add(1, Ordering::Relaxed),
        shared,
    })
}

impl MappingFiles {
    fn register(&self, identity: (u64, u64), file: &NativeFile) -> Result<(), RuntimeMemoryError> {
        let guard = file.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = guard.as_ref().ok_or(RuntimeMemoryError::BadDescriptor)?;
        self.arena
            .register_file(
                hl_memory::FileIdentity {
                    device: identity.0,
                    object: identity.1,
                },
                file,
            )
            .map_err(|_| RuntimeMemoryError::NoMemory)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::zero_device_backing;

    fn metadata(kind: u8, device: hl_runtime::DeviceId) -> hl_descriptor::OfdMetadata {
        let timestamp = hl_descriptor::OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        hl_descriptor::OfdMetadata {
            device: 0,
            inode: 0,
            kind,
            permissions: 0o666,
            links: 1,
            user: 0,
            group: 0,
            special_device: device.linux_encoded(),
            size: 0,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        }
    }

    #[test]
    fn only_zero_producing_character_devices_map_anonymously() {
        let next = AtomicU64::new(1);
        assert!(zero_device_backing(&metadata(2, hl_runtime::DeviceId::new(1, 5)), &next, false).is_some());
        assert!(zero_device_backing(&metadata(2, hl_runtime::DeviceId::new(1, 7)), &next, false).is_some());
        assert!(zero_device_backing(&metadata(2, hl_runtime::DeviceId::new(1, 3)), &next, false).is_none());
        assert!(zero_device_backing(&metadata(1, hl_runtime::DeviceId::new(1, 5)), &next, false).is_none());
    }

    #[test]
    fn device_mappings_are_fresh_and_preserve_sharing() {
        let next = AtomicU64::new(41);
        let zero = metadata(2, hl_runtime::DeviceId::new(1, 5));
        assert_eq!(
            zero_device_backing(&zero, &next, false),
            Some(hl_memory::Backing::Anonymous {
                identity: 41,
                shared: false,
            })
        );
        assert_eq!(
            zero_device_backing(&zero, &next, true),
            Some(hl_memory::Backing::Anonymous {
                identity: 42,
                shared: true,
            })
        );
    }
}
