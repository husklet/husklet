use std::sync::Arc;

use super::{Coordinator, Host};
use crate::{MemoryError, Region};

pub trait PreparedHostExit: Send {
    fn publish(&mut self) -> Result<(), MemoryError>;
    fn rollback(&mut self);
    fn finish(&mut self);
}

pub trait ExitHost: Host {
    type PreparedExit: PreparedHostExit;

    fn prepare_exit(&self, regions: &[Region]) -> Result<Self::PreparedExit, MemoryError>;
}

pub struct PreparedAddressExit<H: ExitHost> {
    coordinator: Arc<Coordinator<H>>,
    host: H::PreparedExit,
    generation: u64,
    regions: Vec<Region>,
    state: ExitState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ExitState {
    Prepared,
    Published,
    Complete,
}

impl<H: ExitHost> Coordinator<H> {
    pub fn prepare_exit(self: &Arc<Self>) -> Result<PreparedAddressExit<H>, MemoryError> {
        self.activity.begin_exit()?;
        let snapshot = self.ledger.snapshot();
        match self.host.prepare_exit(&snapshot.regions) {
            Ok(host) => Ok(PreparedAddressExit {
                coordinator: Arc::clone(self),
                host,
                generation: snapshot.generation,
                regions: snapshot.regions,
                state: ExitState::Prepared,
            }),
            Err(error) => {
                self.activity.thaw();
                Err(error)
            }
        }
    }
}

impl<H: ExitHost> PreparedAddressExit<H> {
    pub fn publish(&mut self) -> Result<(), MemoryError> {
        if self.state != ExitState::Prepared {
            return Err(MemoryError::InvariantViolation);
        }
        let mut transition = self.coordinator.transition();
        if self.coordinator.ledger.generation() != self.generation {
            return Err(MemoryError::InvariantViolation);
        }
        self.host.publish()?;
        match self.coordinator.ledger.replace(self.generation, Vec::new()) {
            Ok(generation) => {
                self.generation = generation;
                self.state = ExitState::Published;
                self.coordinator.publish_transition(&mut transition, generation);
                Ok(())
            }
            Err(error) => {
                self.host.rollback();
                Err(error)
            }
        }
    }

    pub fn rollback(&mut self) -> Result<(), MemoryError> {
        if self.state == ExitState::Complete {
            return Err(MemoryError::InvariantViolation);
        }
        if self.state == ExitState::Published {
            let mut transition = self.coordinator.transition();
            self.host.rollback();
            self.generation = self.coordinator.ledger.replace(self.generation, self.regions.clone())?;
            self.coordinator.publish_transition(&mut transition, self.generation);
        }
        self.state = ExitState::Complete;
        self.coordinator.activity.thaw();
        Ok(())
    }

    pub fn finish(&mut self) {
        if self.state != ExitState::Published {
            return;
        }
        self.host.finish();
        self.coordinator
            .pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.state = ExitState::Complete;
        self.coordinator.activity.terminate();
    }
}

impl<H: ExitHost> Drop for PreparedAddressExit<H> {
    fn drop(&mut self) {
        if self.state == ExitState::Complete {
            return;
        }
        if self.state == ExitState::Published {
            let mut transition = self.coordinator.transition();
            self.host.rollback();
            if let Ok(generation) = self.coordinator.ledger.replace(self.generation, self.regions.clone()) {
                self.coordinator.publish_transition(&mut transition, generation);
            }
        }
        self.coordinator.activity.thaw();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use hl_isa::{AddressRange, GuestAddress};

    use super::*;
    use crate::{Backing, FileIdentity, MapRequest, Placement, Protection};

    #[derive(Debug)]
    struct TestHost {
        state: Arc<HostState>,
    }

    #[derive(Debug, Default)]
    struct HostState {
        exited: AtomicBool,
        fail: AtomicBool,
        calls: Mutex<Vec<&'static str>>,
    }

    struct HostExit {
        state: Arc<HostState>,
        published: bool,
    }

    impl Host for TestHost {
        fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
            Ok(1)
        }
        fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
            Ok(2)
        }
        fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
            Ok(3)
        }
        fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
            Ok(())
        }
        fn rollback(&self, _: u64) {}
    }

    impl ExitHost for TestHost {
        type PreparedExit = HostExit;

        fn prepare_exit(&self, _: &[Region]) -> Result<HostExit, MemoryError> {
            self.state.calls.lock().unwrap().push("prepare");
            Ok(HostExit {
                state: Arc::clone(&self.state),
                published: false,
            })
        }
    }

    impl PreparedHostExit for HostExit {
        fn publish(&mut self) -> Result<(), MemoryError> {
            self.state.calls.lock().unwrap().push("publish");
            if self.state.fail.swap(false, Ordering::AcqRel) {
                return Err(MemoryError::InvariantViolation);
            }
            self.state.exited.store(true, Ordering::Release);
            self.published = true;
            Ok(())
        }

        fn rollback(&mut self) {
            self.state.calls.lock().unwrap().push("rollback");
            if self.published {
                self.state.exited.store(false, Ordering::Release);
                self.published = false;
            }
        }

        fn finish(&mut self) {
            self.state.calls.lock().unwrap().push("finish");
        }
    }

    fn fixture(fail: bool) -> (Arc<Coordinator<TestHost>>, Arc<HostState>) {
        let state = Arc::new(HostState::default());
        state.fail.store(fail, Ordering::Release);
        let coordinator = Arc::new(Coordinator::new(TestHost {
            state: Arc::clone(&state),
        }));
        coordinator
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0x1000)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ,
                backing: Backing::File {
                    identity: FileIdentity { device: 1, object: 2 },
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        (coordinator, state)
    }

    #[test]
    fn rollback_restores_space() {
        let (coordinator, state) = fixture(false);
        let before = coordinator.snapshot();
        let mut exit = coordinator.prepare_exit().unwrap();
        exit.publish().unwrap();
        assert!(coordinator.ledger().regions().is_empty());
        exit.rollback().unwrap();
        assert_eq!(coordinator.snapshot().regions, before.regions);
        assert!(!state.exited.load(Ordering::Acquire));
        assert_eq!(
            state.calls.lock().unwrap().as_slice(),
            &["prepare", "publish", "rollback"]
        );
    }

    #[test]
    fn publish_failure_preserves() {
        let (coordinator, _) = fixture(true);
        let before = coordinator.snapshot();
        let mut exit = coordinator.prepare_exit().unwrap();
        assert_eq!(exit.publish(), Err(MemoryError::InvariantViolation));
        exit.rollback().unwrap();
        assert_eq!(coordinator.snapshot().regions, before.regions);
    }

    #[test]
    fn stale_generation_rejected() {
        let (coordinator, state) = fixture(false);
        let snapshot = coordinator.snapshot();
        let mut exit = coordinator.prepare_exit().unwrap();
        coordinator
            .ledger
            .replace(snapshot.generation, snapshot.regions)
            .unwrap();
        assert_eq!(exit.publish(), Err(MemoryError::InvariantViolation));
        assert!(!state.exited.load(Ordering::Acquire));
        exit.rollback().unwrap();
    }

    #[test]
    fn mutation_waits_rollback() {
        let (coordinator, _) = fixture(false);
        let exit = coordinator.prepare_exit().unwrap();
        let worker = Arc::clone(&coordinator);
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            send.send(worker.protect(
                AddressRange::nonempty(GuestAddress::new(0x1000), 4096).unwrap(),
                Protection::READ,
            ))
            .unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(20)).is_err());
        drop(exit);
        assert_eq!(receive.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
        thread.join().unwrap();
    }

    #[test]
    fn finish_is_terminal() {
        let (coordinator, state) = fixture(false);
        let mut exit = coordinator.prepare_exit().unwrap();
        exit.publish().unwrap();
        exit.finish();
        assert_eq!(
            state.calls.lock().unwrap().as_slice(),
            &["prepare", "publish", "finish"]
        );
        assert_eq!(
            coordinator.map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0x2000)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ,
                backing: Backing::File {
                    identity: FileIdentity { device: 1, object: 3 },
                    shared: false,
                },
                backing_offset: 0,
            }),
            Err(MemoryError::NoAddressSpace)
        );
        assert!(matches!(coordinator.prepare_exit(), Err(MemoryError::NoAddressSpace)));
        assert_eq!(exit.rollback(), Err(MemoryError::InvariantViolation));
    }

    #[test]
    fn concurrent_rejected() {
        let (coordinator, state) = fixture(false);
        let exit = coordinator.prepare_exit().unwrap();
        assert!(matches!(
            coordinator.prepare_exit(),
            Err(MemoryError::InvariantViolation)
        ));
        assert_eq!(state.calls.lock().unwrap().as_slice(), &["prepare"]);
        drop(exit);

        let mut replacement = coordinator.prepare_exit().unwrap();
        replacement.rollback().unwrap();
        assert_eq!(state.calls.lock().unwrap().as_slice(), &["prepare", "prepare"]);
    }

    #[test]
    fn finish_order_guard() {
        let (coordinator, state) = fixture(false);
        let mut exit = coordinator.prepare_exit().unwrap();
        exit.finish();
        assert!(state.calls.lock().unwrap().iter().all(|call| *call != "finish"));
        drop(exit);
        assert!(
            coordinator
                .map(MapRequest {
                    placement: Placement::Fixed(GuestAddress::new(0x2000)),
                    length: 4096,
                    alignment: 4096,
                    protection: Protection::READ,
                    backing: Backing::File {
                        identity: FileIdentity { device: 1, object: 3 },
                        shared: false,
                    },
                    backing_offset: 0,
                })
                .is_ok()
        );
    }
}
