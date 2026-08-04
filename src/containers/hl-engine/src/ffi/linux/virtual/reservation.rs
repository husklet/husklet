use std::sync::Arc;

use super::virtual_host::GuestVm;

/// Owns one complete inaccessible host mapping reservation.
pub(super) struct Reservation {
    address: usize,
    length: usize,
    host: Arc<dyn GuestVm>,
}

impl Reservation {
    #[must_use]
    pub(super) fn new(address: usize, length: usize, host: Arc<dyn GuestVm>) -> Self {
        Self { address, length, host }
    }
}

// SAFETY: the reservation has no Rust references and `munmap` may release it
// from any host thread; ownership remains unique inside HostResourceLease.
unsafe impl Send for Reservation {}
// SAFETY: shared observation exposes only scalar ownership metadata; teardown
// remains exclusive through HostResourceLease::drop.
unsafe impl Sync for Reservation {}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.host.release(self.address, self.length);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::ffi::linux::virtual_host::MapSource;

    #[derive(Debug, Default)]
    struct Host {
        releases: AtomicUsize,
    }

    impl GuestVm for Host {
        fn reserve(&self, _length: usize) -> Result<usize, ()> {
            unreachable!("reservation is constructed from an already-owned range")
        }

        fn map(
            &self,
            _address: usize,
            _length: usize,
            _protection: i32,
            _shared: bool,
            _source: MapSource,
        ) -> Result<(), ()> {
            unreachable!("reservation teardown does not map")
        }

        fn protect(&self, _address: usize, _length: usize, _protection: i32) -> Result<(), ()> {
            unreachable!("reservation teardown does not protect")
        }

        fn remap(
            &self,
            _source: usize,
            _old_length: usize,
            _destination: usize,
            _new_length: usize,
            _keep: bool,
        ) -> Result<(), ()> {
            unreachable!("reservation teardown does not remap")
        }

        fn release(&self, address: usize, length: usize) {
            assert_eq!((address, length), (0x1000, 0x2000));
            self.releases.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn reservation_releases_exactly_once() {
        let host = Arc::new(Host::default());
        let port: Arc<dyn GuestVm> = host.clone();
        drop(Reservation::new(0x1000, 0x2000, port));
        assert_eq!(host.releases.load(Ordering::Acquire), 1);
    }
}
