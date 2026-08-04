use crate::model::{MapRequest, MemoryError, Placement, Region};
use hl_isa::{AddressRange, GuestAddress, GuestPageSize};

impl MapRequest {
    pub(crate) fn validate(self) -> Result<(), MemoryError> {
        self.validate_shape()?;
        self.validate_offset()?;
        self.validate_placement()
    }

    fn validate_shape(self) -> Result<(), MemoryError> {
        let page = GuestPageSize::LINUX.bytes();
        if self.length == 0 {
            return Err(MemoryError::EmptyRange);
        }
        if self.length % page != 0 || self.alignment < page || !self.alignment.is_power_of_two() {
            return Err(MemoryError::Unaligned);
        }
        Ok(())
    }

    fn validate_offset(self) -> Result<(), MemoryError> {
        if self.backing_offset % GuestPageSize::LINUX.bytes() != 0 {
            return Err(MemoryError::Unaligned);
        }
        self.backing_offset
            .checked_add(self.length)
            .ok_or(MemoryError::BackingOverflow)?;
        Ok(())
    }

    fn validate_placement(self) -> Result<(), MemoryError> {
        match self.placement {
            Placement::Fixed(start) | Placement::FixedNoReplace(start) => self.validate_fixed(start),
            Placement::Anywhere { minimum, maximum, hint } => self.validate_anywhere(minimum, maximum, hint),
        }
    }

    fn validate_fixed(self, start: GuestAddress) -> Result<(), MemoryError> {
        if !start.is_page_aligned(GuestPageSize::LINUX) || start.get() % self.alignment != 0 {
            return Err(MemoryError::Unaligned);
        }
        start.checked_add(self.length)?;
        Ok(())
    }

    fn validate_anywhere(
        self,
        minimum: GuestAddress,
        maximum: GuestAddress,
        hint: Option<GuestAddress>,
    ) -> Result<(), MemoryError> {
        if minimum >= maximum {
            return Err(MemoryError::NoAddressSpace);
        }
        if hint.is_some_and(|address| !address.is_page_aligned(GuestPageSize::LINUX)) {
            return Err(MemoryError::Unaligned);
        }
        Ok(())
    }

    pub(crate) fn choose(self, regions: &[Region]) -> Result<GuestAddress, MemoryError> {
        match self.placement {
            Placement::Fixed(address) | Placement::FixedNoReplace(address) => Ok(address),
            Placement::Anywhere { minimum, maximum, hint } => self.choose_anywhere(regions, minimum, maximum, hint),
        }
    }

    fn choose_anywhere(
        self,
        regions: &[Region],
        minimum: GuestAddress,
        maximum: GuestAddress,
        hint: Option<GuestAddress>,
    ) -> Result<GuestAddress, MemoryError> {
        if let Some(address) = self.usable_hint(regions, minimum, maximum, hint) {
            return Ok(address);
        }
        self.first_fit(regions, minimum, maximum)
    }

    fn usable_hint(
        self,
        regions: &[Region],
        minimum: GuestAddress,
        maximum: GuestAddress,
        hint: Option<GuestAddress>,
    ) -> Option<GuestAddress> {
        let start = self.align(hint?).ok()?;
        self.fits(regions, start, minimum, maximum).then_some(start)
    }

    fn first_fit(
        self,
        regions: &[Region],
        minimum: GuestAddress,
        maximum: GuestAddress,
    ) -> Result<GuestAddress, MemoryError> {
        let mut candidate = self.align(minimum)?;
        for region in regions {
            if region.range().end() <= candidate {
                continue;
            }
            if region.range().start() >= maximum {
                break;
            }
            if self.ends_before(candidate, region.range().start(), maximum) {
                return Ok(candidate);
            }
            candidate = self.align(candidate.max(region.range().end()))?;
        }
        if self.ends_before(candidate, maximum, maximum) {
            Ok(candidate)
        } else {
            Err(MemoryError::NoAddressSpace)
        }
    }

    fn fits(self, regions: &[Region], start: GuestAddress, minimum: GuestAddress, maximum: GuestAddress) -> bool {
        if start < minimum {
            return false;
        }
        let Ok(range) = AddressRange::nonempty(start, self.length) else {
            return false;
        };
        range.end() <= maximum && !regions.iter().any(|region| region.overlaps(range))
    }

    fn ends_before(self, start: GuestAddress, boundary: GuestAddress, maximum: GuestAddress) -> bool {
        start
            .checked_add(self.length)
            .is_ok_and(|end| end <= boundary && end <= maximum)
    }

    fn align(self, address: GuestAddress) -> Result<GuestAddress, MemoryError> {
        let mask = self.alignment - 1;
        let value = address.get().checked_add(mask).ok_or(MemoryError::AddressOverflow)? & !mask;
        Ok(GuestAddress::new(value))
    }
}
