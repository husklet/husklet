use super::super::MemoryError;

pub(super) struct Capacity;

const _: () = assert!(usize::BITS >= 64);

impl Capacity {
    pub(super) const MAXIMUM: usize = hl_memory::MEMORY_ADDRESS_MAXIMUM as usize;
    pub(super) const MINIMUM: usize = 16 * 1024 * 1024 * 1024;
    // The retained C engine gives an unconstrained guest the host's ordinary
    // virtual address space. Rust uses one sparse PROT_NONE reservation instead,
    // so its default must not turn a few ordinary pthread stacks into an
    // artificial mmap failure. Reserving the implementation's full current
    // ceiling consumes virtual addresses, not resident memory. HL_MEM_MAX is a
    // separate anonymous-memory charge and never narrows this address aperture.
    pub(super) const DEFAULT: usize = Self::MAXIMUM;

    pub(super) fn reserve(
        context: std::sync::Arc<crate::native_host::HostResourceContext>,
    ) -> Result<super::VirtualMemory, MemoryError> {
        Self::reserve_with(|length| super::VirtualMemory::reserve_in(std::sync::Arc::clone(&context), length))
    }

    fn reserve_with<T>(mut attempt: impl FnMut(usize) -> Result<T, MemoryError>) -> Result<T, MemoryError> {
        let mut length = Self::DEFAULT;
        loop {
            match attempt(length) {
                Ok(value) => return Ok(value),
                Err(MemoryError::OutOfMemory) if length > Self::MINIMUM => {
                    length = (length / 2).max(Self::MINIMUM);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hl_isa::GuestAddress;
    use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement, Protection};

    use super::super::{MappingHostAdapter, VirtualMemory};
    use super::{Capacity, MemoryError};

    #[test]
    fn reservation_selects_largest_supported_capacity() {
        let mut attempted = Vec::new();
        let selected = Capacity::reserve_with(|length| {
            attempted.push(length);
            (length <= Capacity::DEFAULT / 4)
                .then_some(length)
                .ok_or(MemoryError::OutOfMemory)
        })
        .unwrap();
        assert_eq!(selected, Capacity::DEFAULT / 4);
        assert_eq!(
            attempted,
            vec![Capacity::DEFAULT, Capacity::DEFAULT / 2, Capacity::DEFAULT / 4]
        );
    }

    #[test]
    fn reservation_preserves_non_capacity_error_ordering() {
        let mut calls = 0;
        let error = Capacity::reserve_with::<()>(|_| {
            calls += 1;
            Err(MemoryError::Host)
        })
        .unwrap_err();
        assert_eq!(error, MemoryError::Host);
        assert_eq!(calls, 1);
    }

    #[test]
    fn reservation_returns_out_of_memory_when_minimum_cannot_fit() {
        let mut last = 0;
        let error = Capacity::reserve_with::<()>(|length| {
            last = length;
            Err(MemoryError::OutOfMemory)
        })
        .unwrap_err();
        assert_eq!(error, MemoryError::OutOfMemory);
        assert_eq!(last, Capacity::MINIMUM);
    }

    fn request(placement: Placement, length: u64, identity: u64) -> MapRequest {
        MapRequest {
            placement,
            length,
            alignment: 4096,
            protection: Protection::NONE,
            backing: Backing::Anonymous {
                identity,
                shared: false,
            },
            backing_offset: 0,
        }
    }

    #[test]
    fn ordinary_threads_fit() {
        const MAIN_STACK: u64 = 0x30_00000;
        const STACK_LENGTH: u64 = 8 * 1024 * 1024;
        const THREAD_STACK: u64 = STACK_LENGTH + 4096;

        let arena = Arc::new(VirtualMemory::reserve(Capacity::DEFAULT).unwrap());
        let mappings = MappingCoordinator::new(MappingHostAdapter::new(arena));
        mappings
            .map(request(
                Placement::Fixed(GuestAddress::new(MAIN_STACK)),
                STACK_LENGTH,
                1,
            ))
            .unwrap();
        for identity in 2..=5 {
            mappings
                .map(request(
                    Placement::Anywhere {
                        minimum: GuestAddress::new(4096),
                        maximum: GuestAddress::new(Capacity::DEFAULT as u64),
                        hint: None,
                    },
                    THREAD_STACK,
                    identity,
                ))
                .unwrap();
        }
    }

    #[test]
    fn aperture_is_independent() {
        assert_eq!(Capacity::DEFAULT as u64, hl_memory::MEMORY_ADDRESS_MAXIMUM);
        assert!(Capacity::DEFAULT >= 16 * 1024 * 1024 * 1024);
    }

    #[test]
    fn sparse_offset_fit() {
        const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;
        const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;

        let arena = Arc::new(VirtualMemory::reserve(Capacity::DEFAULT).unwrap());
        let mappings = MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena)));
        let address = mappings
            .map(MapRequest {
                placement: Placement::Anywhere {
                    minimum: GuestAddress::new(4096),
                    maximum: GuestAddress::new(Capacity::DEFAULT as u64),
                    hint: None,
                },
                length: FIVE_GIB,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();

        // The host reservation and mapping are sparse. Only these bounded
        // bytes are materialized while the guest offset crosses 32 bits.
        let offset = address.get() + FOUR_GIB;
        arena.write(offset - 8, b"before32").unwrap();
        arena.write(offset, b"after-32").unwrap();
        let mut bytes = [0; 8];
        arena.read(offset - 8, &mut bytes).unwrap();
        assert_eq!(&bytes, b"before32");
        arena.read(offset, &mut bytes).unwrap();
        assert_eq!(&bytes, b"after-32");
    }
}
