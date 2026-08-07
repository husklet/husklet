use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Backing, MemoryError};

const STRIPE_COUNT: usize = 4096;
const GRANULE_BYTES: u64 = 64;

/// Stable exclusive-monitor coordinate for one backing granule.
///
/// Shared coordinates are independent of the guest address at which a backing
/// is projected. Private coordinates deliberately use the guest address and an
/// address-space-owned epoch table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReservationCoordinate {
    key: u64,
    shared: bool,
}

impl ReservationCoordinate {
    pub fn from_mapping(backing: Backing, backing_offset: u64, guest_address: u64) -> Result<Self, MemoryError> {
        let absolute_offset = match backing {
            Backing::Shared(reference) => reference
                .offset
                .checked_add(backing_offset)
                .ok_or(MemoryError::BackingOverflow)?,
            Backing::Anonymous { .. } | Backing::File { .. } => backing_offset,
        };
        let granule = absolute_offset / GRANULE_BYTES;
        let (shared, identity) = match backing {
            Backing::Shared(reference) => (
                true,
                Self::mix(u64::from(reference.object.slot), u64::from(reference.object.generation)),
            ),
            Backing::Anonymous { identity, shared } => (shared, Self::mix(identity, 1)),
            Backing::File { identity, shared } => (shared, Self::mix(identity.device, identity.object)),
        };
        Ok(Self {
            key: if shared {
                Self::mix(identity, granule)
            } else {
                ReservationEpochs::private_key(guest_address)
            },
            shared,
        })
    }

    #[must_use]
    pub const fn shared(self) -> bool {
        self.shared
    }

    const fn mix(left: u64, right: u64) -> u64 {
        left ^ right.wrapping_add(0x9e37_79b9_7f4a_7c15).rotate_left(27)
    }
}

/// Fixed, allocation-free generations for exclusive-reservation granules.
/// Keys include backing identity and the retained CPU model's 64-byte ERG.
#[derive(Debug)]
pub struct ReservationEpochs {
    stripes: Box<[AtomicU64; STRIPE_COUNT]>,
}

impl Default for ReservationEpochs {
    fn default() -> Self {
        Self {
            stripes: Box::new(std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }
}

impl ReservationEpochs {
    #[must_use]
    pub const fn private_key(address: u64) -> u64 {
        let granule = address / 64;
        granule ^ 0x9e37_79b9_7f4a_7c15_u64.rotate_left(27)
    }

    #[must_use]
    pub fn capture(&self, key: u64) -> u64 {
        self.stripes[Self::stripe(key)].load(Ordering::Acquire)
    }

    #[must_use]
    pub fn capture_at(&self, coordinate: ReservationCoordinate) -> u64 {
        self.capture(coordinate.key)
    }

    pub fn invalidate(&self, key: u64) {
        self.stripes[Self::stripe(key)].fetch_add(1, Ordering::AcqRel);
    }

    pub fn invalidate_at(&self, coordinate: ReservationCoordinate) {
        self.invalidate(coordinate.key);
    }

    const fn stripe(key: u64) -> usize {
        let mut mixed = key.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        (mixed as usize) & (STRIPE_COUNT - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SharedBackingRef, SharedObjectId};

    #[test]
    fn shared_offsets_alias() {
        let object = SharedObjectId { slot: 3, generation: 5 };
        let first = ReservationCoordinate::from_mapping(
            Backing::Shared(SharedBackingRef {
                object,
                offset: 64,
                length: 256,
                write_shared: true,
            }),
            64,
            0x1000,
        )
        .unwrap();
        let second = ReservationCoordinate::from_mapping(
            Backing::Shared(SharedBackingRef {
                object,
                offset: 0,
                length: 256,
                write_shared: true,
            }),
            128,
            0x9000,
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn private_addresses_differ() {
        let backing = Backing::Anonymous {
            identity: 7,
            shared: false,
        };
        let first = ReservationCoordinate::from_mapping(backing, 0, 0x1000).unwrap();
        let second = ReservationCoordinate::from_mapping(backing, 0, 0x2000).unwrap();
        assert_ne!(first, second);
    }
}
