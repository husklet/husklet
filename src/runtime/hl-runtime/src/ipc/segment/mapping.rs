use std::collections::BTreeMap;
use std::sync::Arc;

use hl_ipc::AttachPlan;
use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{Backing, MapRequest, MappingHost, MemoryError, Placement, Protection};

use super::{ForkBinding, Mapping, MappingError, MemoryBinding, MemoryMappings, binding};

impl<H: MappingHost> MemoryMappings<H> {
    pub(crate) fn prepare_owned_bindings(
        &self,
        bindings: &[ForkBinding],
    ) -> Result<binding::OwnedPreparedBindings, MappingError> {
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
        Ok(binding::OwnedPreparedBindings {
            mappings: Arc::clone(&self.mappings),
            expected,
            replacement,
        })
    }

    pub(super) fn request(plan: AttachPlan, requested: GuestAddress) -> Result<MapRequest, MappingError> {
        let requested = if requested == GuestAddress::ZERO {
            None
        } else if plan.round_address {
            Some(GuestAddress::new(requested.get() & !4095))
        } else if requested.get() & 4095 != 0 {
            return Err(MappingError::Invalid);
        } else {
            Some(requested)
        };
        if plan.replace && requested.is_none() {
            return Err(MappingError::Invalid);
        }
        let placement = match (requested, plan.replace) {
            (Some(address), true) => Placement::Fixed(address),
            (Some(address), false) => Placement::FixedNoReplace(address),
            (None, _) => Placement::Anywhere {
                minimum: GuestAddress::new(4096),
                maximum: GuestAddress::new(u64::MAX & !4095),
                hint: None,
            },
        };
        let mut protection = Protection::READ;
        if !plan.read_only {
            protection = protection.union(Protection::WRITE);
        }
        if plan.executable {
            protection = protection.union(Protection::EXECUTE);
        }
        Ok(MapRequest {
            placement,
            length: plan.backing.length,
            alignment: 4096,
            protection,
            backing: Backing::Shared(plan.backing),
            backing_offset: plan.backing.offset,
        })
    }

    pub(super) fn memory_error(error: MemoryError) -> MappingError {
        match error {
            MemoryError::AlreadyMapped
            | MemoryError::Unaligned
            | MemoryError::AddressOverflow
            | MemoryError::EmptyRange => MappingError::Invalid,
            MemoryError::NoAddressSpace | MemoryError::Shared(hl_memory::SharedError::ResourceLimit) => {
                MappingError::NoMemory
            }
            _ => MappingError::Invariant,
        }
    }

    pub(super) fn restored_mapping(
        binding: &MemoryBinding,
        regions: &[hl_memory::Region],
    ) -> Result<Mapping, MappingError> {
        if binding.attachment == 0 {
            return Err(MappingError::Invalid);
        }
        let range = AddressRange::nonempty(binding.address, binding.length).map_err(|_| MappingError::Invalid)?;
        if !range.is_page_aligned(hl_isa::GuestPageSize::LINUX) {
            return Err(MappingError::Invalid);
        }
        let region = regions
            .iter()
            .find(|region| region.range() == range)
            .ok_or(MappingError::Invalid)?;
        if !matches!(region.backing(), Backing::Shared(_)) {
            return Err(MappingError::Invalid);
        }
        Ok(Mapping {
            range,
            attachment: Some(binding.attachment),
        })
    }

    pub(super) fn fork_mapping(planned: &ForkBinding, regions: &[hl_memory::Region]) -> Result<Mapping, MappingError> {
        let mapping = Self::restored_mapping(&planned.binding, regions)?;
        let region = regions
            .iter()
            .find(|region| region.range() == mapping.range)
            .ok_or(MappingError::Invalid)?;
        if region.backing() != Backing::Shared(planned.backing) || region.backing_offset() != planned.backing.offset {
            return Err(MappingError::Invalid);
        }
        Ok(mapping)
    }
}
