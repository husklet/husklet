use crate::{ImageKind, ImagePlan, LoadError, ReservedMapping};

/// Canonical guest-address interval occupied by one ELF image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestAddressRange {
    pub start: u64,
    pub end: u64,
}

/// Projection between canonical ELF link addresses and private host storage.
///
/// Linux-visible addresses remain in `guest`; the bias is applied only when
/// an engine component dereferences storage backing a displaced `ET_EXEC`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageProjection {
    pub guest: GuestAddressRange,
    pub storage_bias: u64,
}

impl ImageProjection {
    pub(crate) fn build<R>(plan: &ImagePlan, mapping: &ReservedMapping<R>) -> Result<Option<Self>, LoadError> {
        let storage_bias = mapping
            .address()
            .checked_sub(plan.link_base())
            .ok_or(LoadError::InvalidReservation)?;
        if plan.kind() != ImageKind::Executable || storage_bias == 0 {
            return Ok(None);
        }
        let end = plan
            .link_base()
            .checked_add(plan.image_span())
            .ok_or(LoadError::InvalidReservation)?;
        Ok(Some(Self {
            guest: GuestAddressRange {
                start: plan.link_base(),
                end,
            },
            storage_bias,
        }))
    }

    #[must_use]
    pub fn storage_address(self, guest: u64) -> Option<u64> {
        if guest >= self.guest.start && guest < self.guest.end {
            guest.checked_add(self.storage_bias)
        } else {
            Some(guest)
        }
    }

    #[must_use]
    pub fn guest_address(self, storage: u64) -> Option<u64> {
        let start = self.guest.start.checked_add(self.storage_bias)?;
        let end = self.guest.end.checked_add(self.storage_bias)?;
        if storage >= start && storage < end {
            storage.checked_sub(self.storage_bias)
        } else {
            Some(storage)
        }
    }
}
