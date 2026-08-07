use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_ipc::AttachPlan;
use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{MappingBatch, MappingCoordinator, MappingHost, MappingOperation};

use super::{PreparedBindingSet, binding};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingError {
    Invalid,
    NoMemory,
    Invariant,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryBinding {
    pub address: GuestAddress,
    pub length: u64,
    pub attachment: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForkBinding {
    pub binding: MemoryBinding,
    pub backing: hl_memory::SharedBackingRef,
}

/// Process-local mapping capability consumed by the `SysV` syscall composition.
///
/// A successful `map` owns a pending mapping until `bind` associates the
/// domain attachment token. `unmap` publishes no tracking change when the
/// memory transaction fails.
pub trait MemoryPort: Send + Sync {
    fn map(&self, plan: AttachPlan, requested: GuestAddress) -> Result<GuestAddress, MappingError>;

    fn bind(&self, address: GuestAddress, attachment: u64) -> Result<(), MappingError>;

    fn rollback(&self, address: GuestAddress) -> Result<(), MappingError>;

    fn unmap(&self, address: GuestAddress) -> Result<u64, MappingError>;

    fn bindings(&self) -> Result<Vec<MemoryBinding>, MappingError> {
        Err(MappingError::Unsupported)
    }

    fn restore_bindings(&self, _: &[MemoryBinding]) -> Result<(), MappingError> {
        Err(MappingError::Unsupported)
    }

    fn prepare_restore_bindings(
        &self,
        _: &[MemoryBinding],
    ) -> Result<Box<dyn PreparedBindingSet<'_> + '_>, MappingError> {
        Err(MappingError::Unsupported)
    }

    fn prepare_fork_bindings(&self, _: &[ForkBinding]) -> Result<Box<dyn PreparedBindingSet<'_> + '_>, MappingError> {
        Err(MappingError::Unsupported)
    }

    fn rebind_fork(&self, _: &[(u64, u64)]) -> Result<(), MappingError> {
        Err(MappingError::Unsupported)
    }

    fn unmap_all(&self) -> Result<Vec<u64>, MappingError> {
        Err(MappingError::Unsupported)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Mapping {
    pub(crate) range: AddressRange,
    pub(crate) attachment: Option<u64>,
}

pub struct MemoryMappings<H: MappingHost> {
    pub(crate) coordinator: Arc<MappingCoordinator<H>>,
    pub(crate) mappings: Arc<Mutex<BTreeMap<GuestAddress, Mapping>>>,
}

impl<H: MappingHost> MemoryMappings<H> {
    #[must_use]
    pub fn new(coordinator: Arc<MappingCoordinator<H>>) -> Self {
        Self {
            coordinator,
            mappings: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl<H: MappingHost> MemoryPort for MemoryMappings<H> {
    fn map(&self, plan: AttachPlan, requested: GuestAddress) -> Result<GuestAddress, MappingError> {
        let request = Self::request(plan, requested)?;
        let address = self.coordinator.map(request).map_err(Self::memory_error)?;
        let range = AddressRange::nonempty(address, request.length).map_err(|_| MappingError::Invariant)?;
        let previous = self
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                address,
                Mapping {
                    range,
                    attachment: None,
                },
            );
        if previous.is_some() {
            let _ = self.coordinator.unmap(range);
            return Err(MappingError::Invariant);
        }
        Ok(address)
    }

    fn bind(&self, address: GuestAddress, attachment: u64) -> Result<(), MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mapping = mappings.get_mut(&address).ok_or(MappingError::Invalid)?;
        if mapping.attachment.replace(attachment).is_some() {
            return Err(MappingError::Invariant);
        }
        Ok(())
    }

    fn rollback(&self, address: GuestAddress) -> Result<(), MappingError> {
        let range = self
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address)
            .ok_or(MappingError::Invalid)?
            .range;
        self.coordinator.unmap(range).map_err(Self::memory_error)?;
        self.mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&address);
        Ok(())
    }

    fn unmap(&self, address: GuestAddress) -> Result<u64, MappingError> {
        let mapping = *self
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address)
            .ok_or(MappingError::Invalid)?;
        let attachment = mapping.attachment.ok_or(MappingError::Invalid)?;
        self.coordinator.unmap(mapping.range).map_err(Self::memory_error)?;
        self.mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&address);
        Ok(attachment)
    }

    fn bindings(&self) -> Result<Vec<MemoryBinding>, MappingError> {
        let mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        mappings
            .iter()
            .map(|(address, mapping)| {
                Ok(MemoryBinding {
                    address: *address,
                    length: mapping.range.length(),
                    attachment: mapping.attachment.ok_or(MappingError::Invariant)?,
                })
            })
            .collect()
    }

    fn restore_bindings(&self, bindings: &[MemoryBinding]) -> Result<(), MappingError> {
        let mut restored = BTreeMap::new();
        for binding in bindings {
            if binding.attachment == 0 {
                return Err(MappingError::Invalid);
            }
            let range = AddressRange::nonempty(binding.address, binding.length).map_err(|_| MappingError::Invalid)?;
            if !range.is_page_aligned(hl_isa::GuestPageSize::LINUX)
                || restored
                    .insert(
                        binding.address,
                        Mapping {
                            range,
                            attachment: Some(binding.attachment),
                        },
                    )
                    .is_some()
            {
                return Err(MappingError::Invalid);
            }
        }
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !mappings.is_empty() {
            return Err(MappingError::Invariant);
        }
        *mappings = restored;
        Ok(())
    }

    fn prepare_restore_bindings(
        &self,
        bindings: &[MemoryBinding],
    ) -> Result<Box<dyn PreparedBindingSet<'_> + '_>, MappingError> {
        let mut replacement = BTreeMap::new();
        let regions = self.coordinator.ledger().regions();
        for binding in bindings {
            let mapping = Self::restored_mapping(binding, &regions)?;
            if replacement.insert(binding.address, mapping).is_some() {
                return Err(MappingError::Invalid);
            }
        }
        let expected = self
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(Box::new(binding::PreparedBindings {
            mappings: &self.mappings,
            expected,
            replacement,
        }))
    }

    fn prepare_fork_bindings(
        &self,
        bindings: &[ForkBinding],
    ) -> Result<Box<dyn PreparedBindingSet<'_> + '_>, MappingError> {
        let regions = self.coordinator.ledger().regions();
        let mut replacement = BTreeMap::new();
        for planned in bindings {
            let mapping = Self::fork_mapping(planned, &regions)?;
            if replacement.insert(planned.binding.address, mapping).is_some() {
                return Err(MappingError::Invalid);
            }
        }
        let expected = self
            .mappings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(Box::new(binding::PreparedBindings {
            mappings: &self.mappings,
            expected,
            replacement,
        }))
    }

    fn rebind_fork(&self, inherited: &[(u64, u64)]) -> Result<(), MappingError> {
        let replacements = inherited.iter().copied().collect::<BTreeMap<_, _>>();
        if replacements.len() != inherited.len() {
            return Err(MappingError::Invalid);
        }
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for mapping in mappings.values_mut() {
            let old = mapping.attachment.ok_or(MappingError::Invariant)?;
            mapping.attachment = Some(*replacements.get(&old).ok_or(MappingError::Invalid)?);
        }
        Ok(())
    }

    fn unmap_all(&self) -> Result<Vec<u64>, MappingError> {
        let mut mappings = self.mappings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut batch = MappingBatch::new();
        let mut attachments = Vec::with_capacity(mappings.len());
        for mapping in mappings.values() {
            batch.push(MappingOperation::Unmap(mapping.range));
            attachments.push(mapping.attachment.ok_or(MappingError::Invariant)?);
        }
        self.coordinator.apply(&batch).map_err(Self::memory_error)?;
        mappings.clear();
        Ok(attachments)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use hl_ipc::{SHM_EXEC, SharedMemoryId};
    use hl_memory::{
        MapRequest, MemoryError, Placement, Protection, SharedBackingRef, SharedLimits, SharedObjectStore,
    };

    use super::*;

    #[derive(Clone, Debug)]
    struct Host {
        request: Arc<Mutex<Option<MapRequest>>>,
        fail_unmap: Arc<AtomicBool>,
    }

    impl Host {
        fn new() -> Self {
            Self {
                request: Arc::new(Mutex::new(None)),
                fail_unmap: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl MappingHost for Host {
        fn stage_map(&self, _: GuestAddress, request: MapRequest) -> Result<u64, MemoryError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(1)
        }

        fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
            if self.fail_unmap.load(Ordering::Acquire) {
                Err(MemoryError::InvariantViolation)
            } else {
                Ok(2)
            }
        }

        fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
            Ok(3)
        }

        fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
            Ok(())
        }

        fn rollback(&self, _: u64) {}
    }

    fn fixture() -> (Host, MemoryMappings<Host>, hl_memory::SharedObjectId) {
        let host = Host::new();
        let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let object = store.create(1, 4096).unwrap();
        let coordinator = Arc::new(MappingCoordinator::with_shared(host.clone(), store));
        (host, MemoryMappings::new(coordinator), object)
    }

    fn plan(
        object: hl_memory::SharedObjectId,
        read_only: bool,
        executable: bool,
        round_address: bool,
        replace: bool,
    ) -> AttachPlan {
        AttachPlan {
            segment: SharedMemoryId { slot: 0, generation: 1 },
            backing: SharedBackingRef {
                object,
                offset: 0,
                length: 4096,
                write_shared: true,
            },
            read_only,
            executable,
            round_address,
            replace,
        }
    }

    #[test]
    fn mapping_flags() {
        let (host, mappings, object) = fixture();
        let address = mappings
            .map(plan(object, true, true, true, true), GuestAddress::new(0x2345))
            .unwrap();
        assert_eq!(address, GuestAddress::new(0x2000));
        let request = host.request.lock().unwrap().unwrap();
        assert_eq!(request.placement, Placement::Fixed(GuestAddress::new(0x2000)));
        assert!(request.protection.contains(Protection::READ));
        assert!(request.protection.contains(Protection::EXECUTE));
        assert!(!request.protection.contains(Protection::WRITE));
        assert_eq!(SHM_EXEC, 0x8000);
    }

    #[test]
    fn address_precedence() {
        let (_, mappings, object) = fixture();
        assert_eq!(
            mappings.map(plan(object, false, false, false, false), GuestAddress::new(0x2345),),
            Err(MappingError::Invalid),
        );
        assert_eq!(
            mappings.map(plan(object, false, false, false, true), GuestAddress::ZERO,),
            Err(MappingError::Invalid),
        );
    }

    #[test]
    fn unmap_rollback() {
        let (host, mappings, object) = fixture();
        let address = mappings
            .map(plan(object, false, false, false, false), GuestAddress::new(0x4000))
            .unwrap();
        mappings.bind(address, 91).unwrap();
        host.fail_unmap.store(true, Ordering::Release);
        assert_eq!(mappings.unmap(address), Err(MappingError::Invariant),);
        host.fail_unmap.store(false, Ordering::Release);
        assert_eq!(mappings.unmap(address), Ok(91));
    }
}
