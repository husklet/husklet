use std::ops::Range;

use crate::{ImagePlan, Protection};

const GUEST_PAGE_SIZE: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectionPlanError {
    InvalidHostPageSize,
    AddressOverflow,
    WriteExecutePage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectionRange {
    mapping_offset: u64,
    size: u64,
    protection: Protection,
}

impl ProtectionRange {
    #[must_use]
    pub const fn mapping_offset(&self) -> u64 {
        self.mapping_offset
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    #[must_use]
    pub const fn protection(&self) -> Protection {
        self.protection
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageProtectionPlan {
    ranges: Vec<ProtectionRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestProtectionRange {
    pub guest_address: u64,
    pub size: u64,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestProtectionPlan {
    ranges: Vec<GuestProtectionRange>,
}

impl GuestProtectionPlan {
    pub fn build(image: &ImagePlan, guest_base: u64) -> Result<Self, ProtectionPlanError> {
        const GUEST_PAGE: u64 = 4096;
        let mut ranges = Vec::with_capacity(image.segments().len());
        for segment in image.segments() {
            let segment_offset = segment
                .guest_address()
                .checked_sub(image.link_base())
                .ok_or(ProtectionPlanError::AddressOverflow)?;
            let start = segment_offset & !(GUEST_PAGE - 1);
            let end = segment_offset
                .checked_add(segment.memory_size())
                .and_then(|value| value.checked_add(GUEST_PAGE - 1))
                .map(|value| value & !(GUEST_PAGE - 1))
                .ok_or(ProtectionPlanError::AddressOverflow)?;
            if end <= start {
                continue;
            }
            ranges.push(GuestProtectionRange {
                guest_address: guest_base
                    .checked_add(start)
                    .ok_or(ProtectionPlanError::AddressOverflow)?,
                size: end - start,
                read_only: !segment.flags().is_writable(),
            });
        }
        Ok(Self { ranges })
    }

    #[must_use]
    pub fn ranges(&self) -> &[GuestProtectionRange] {
        &self.ranges
    }
}

impl ImageProtectionPlan {
    pub fn build(image: &ImagePlan, host_page_size: u64) -> Result<Self, ProtectionPlanError> {
        if host_page_size < GUEST_PAGE_SIZE || !host_page_size.is_power_of_two() {
            return Err(ProtectionPlanError::InvalidHostPageSize);
        }
        let page_count = image.image_span() / host_page_size + u64::from(!image.image_span().is_multiple_of(host_page_size));
        let count = usize::try_from(page_count).map_err(|_| ProtectionPlanError::AddressOverflow)?;
        let mut pages = vec![0_u8; count];
        for segment in image.segments() {
            let start = segment
                .guest_address()
                .checked_sub(image.link_base())
                .ok_or(ProtectionPlanError::AddressOverflow)?;
            let end = start
                .checked_add(segment.memory_size())
                .ok_or(ProtectionPlanError::AddressOverflow)?;
            Self::apply(&mut pages, start..end, host_page_size, |bits| {
                bits | segment.flags().bits()
            })?;
        }
        if pages.iter().any(|bits| bits & 3 == 3) {
            return Err(ProtectionPlanError::WriteExecutePage);
        }
        Ok(Self {
            ranges: Self::coalesce(&pages, host_page_size),
        })
    }

    fn apply(
        pages: &mut [u8],
        range: Range<u64>,
        page_size: u64,
        update: impl Fn(u8) -> u8,
    ) -> Result<(), ProtectionPlanError> {
        if range.is_empty() {
            return Ok(());
        }
        let first = range.start / page_size;
        let last = range
            .end
            .checked_add(page_size - 1)
            .ok_or(ProtectionPlanError::AddressOverflow)?
            / page_size;
        for index in first..last {
            let index = usize::try_from(index).map_err(|_| ProtectionPlanError::AddressOverflow)?;
            let bits = pages.get_mut(index).ok_or(ProtectionPlanError::AddressOverflow)?;
            *bits = update(*bits);
        }
        Ok(())
    }

    fn coalesce(pages: &[u8], page_size: u64) -> Vec<ProtectionRange> {
        let mut ranges = Vec::new();
        let mut start = 0;
        while start < pages.len() {
            let bits = pages[start];
            let mut end = start + 1;
            while end < pages.len() && pages[end] == bits {
                end += 1;
            }
            if bits != 0 {
                ranges.push(ProtectionRange {
                    mapping_offset: start as u64 * page_size,
                    size: (end - start) as u64 * page_size,
                    protection: Protection::from_bits(bits),
                });
            }
            start = end;
        }
        ranges
    }

    #[must_use]
    pub fn ranges(&self) -> &[ProtectionRange] {
        &self.ranges
    }
}
