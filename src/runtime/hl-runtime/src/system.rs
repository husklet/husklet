use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// Container-visible resource values shared by syscalls and virtual files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceSnapshot {
    pub uptime_seconds: u64,
    /// Successful process creations since this runtime instance started.
    pub process_creations: u64,
    pub loads: [u64; 3],
    pub total_memory: u64,
    pub free_memory: u64,
    /// Explicit CPU quota; `None` keeps `cpu.max` unlimited even when topology is finite.
    pub cpu_limit: Option<usize>,
}

/// One coherent observation of the guest-visible boot and resource tuple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemView {
    pub boot: [u8; 16],
    pub resources: ResourceSnapshot,
}

impl Default for ResourceSnapshot {
    fn default() -> Self {
        Self {
            uptime_seconds: 0,
            process_creations: 0,
            loads: [0; 3],
            total_memory: 0,
            free_memory: 0,
            cpu_limit: None,
        }
    }
}

impl ResourceSnapshot {
    /// Container-visible memory when no host or cgroup observation exists.
    ///
    /// The fallback matches the retained engine and remains distinct from the
    /// zero sentinel used to render an unlimited cgroup.
    #[must_use]
    pub fn visible_memory(self) -> (u64, u64) {
        const FALLBACK_TOTAL: u64 = 8_u64 << 30;
        if self.total_memory == 0 {
            (FALLBACK_TOTAL, FALLBACK_TOTAL / 4)
        } else {
            (self.total_memory, self.free_memory.min(self.total_memory))
        }
    }
}

/// Instance-scoped authority for resource projections.
pub struct SystemAuthority {
    state: Mutex<SystemState>,
}

struct SystemState {
    resources: ResourceSnapshot,
    boot: [u8; 16],
    sequence: u64,
    generation: u64,
    reservation: Option<SystemLaunchReservation>,
    observation_order: u64,
    next_route: u64,
    routes: HashMap<u64, RouteEntry>,
}

#[derive(Clone, Copy)]
struct SystemLaunchReservation {
    generation: u64,
    boot: [u8; 16],
    resources: ResourceSnapshot,
    external_forks: u64,
    external_uptime: Option<u64>,
    external_free: Option<(u64, u64)>,
    construction_free: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchToken(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemObservationError {
    Retired,
    InvalidResources,
    OrderExhausted,
}

pub struct SystemObservationHandle {
    authority: Arc<SystemAuthority>,
    route: u64,
}

#[derive(Clone, Copy)]
struct RouteEntry {
    state: ObservationRoute,
    handles: usize,
}

#[derive(Clone, Copy)]
enum ObservationRoute {
    Pending(LaunchToken),
    Live,
    Retired,
}

/// Failure to prepare or publish a complete system launch identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemLaunchError {
    InvalidBootKey,
    InvalidResources,
    GenerationExhausted,
    SequenceExhausted,
    LaunchBusy,
    ObservationOrderExhausted,
}

/// A validated launch update that has not yet changed the live system view.
///
/// Dropping this value aborts the update without writing to the authority.
pub struct SystemLaunchUpdate {
    authority: Arc<SystemAuthority>,
    generation: u64,
    staged: Option<([u8; 16], ResourceSnapshot)>,
    routes: Vec<u64>,
}

impl SystemAuthority {
    pub fn new(snapshot: ResourceSnapshot) -> Result<Self, SystemLaunchError> {
        Self::validate_resources(snapshot)?;
        Ok(Self {
            state: Mutex::new(SystemState {
                resources: snapshot,
                boot: Self::identity(b"hl-engine"),
                sequence: 1,
                generation: 1,
                reservation: None,
                observation_order: 0,
                next_route: 0,
                routes: HashMap::new(),
            }),
        })
    }

    pub fn snapshot(&self) -> ResourceSnapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resources
    }

    /// Observes boot identity and resources under one authority lock.
    pub fn view(&self) -> SystemView {
        let state = self.state();
        SystemView {
            boot: state.boot,
            resources: state.resources,
        }
    }

    pub fn replace(&self, snapshot: ResourceSnapshot) -> Result<(), SystemLaunchError> {
        Self::validate_resources(snapshot)?;
        let mut state = self.state();
        if state.reservation.is_some() {
            return Err(SystemLaunchError::LaunchBusy);
        }
        state.resources = snapshot;
        state.generation = state.generation.saturating_add(1);
        Ok(())
    }

    pub fn observe_uptime(&self, seconds: u64) {
        let mut state = self.state();
        state.resources.uptime_seconds = state.resources.uptime_seconds.max(seconds);
        if let Some(reservation) = &mut state.reservation {
            reservation.external_uptime = Some(reservation.external_uptime.map_or(seconds, |old| old.max(seconds)));
        } else {
            state.generation = state.generation.saturating_add(1);
        }
    }

    pub fn observe_fork(&self) {
        let mut state = self.state();
        state.resources.process_creations = state.resources.process_creations.saturating_add(1);
        if let Some(reservation) = &mut state.reservation {
            reservation.external_forks = reservation.external_forks.saturating_add(1);
        } else {
            state.generation = state.generation.saturating_add(1);
        }
    }

    pub fn observe_free_memory(&self, bytes: u64) -> Result<(), SystemLaunchError> {
        let mut state = self.state();
        Self::observe_free_memory_state(&mut state, bytes)
    }

    fn observe_free_memory_state(state: &mut SystemState, bytes: u64) -> Result<(), SystemLaunchError> {
        let live = ResourceSnapshot {
            free_memory: bytes,
            ..state.resources
        };
        Self::validate_resources(live)?;
        if state.reservation.is_some() {
            let staged = ResourceSnapshot {
                free_memory: bytes,
                ..state.reservation.expect("checked reservation").resources
            };
            Self::validate_resources(staged)?;
            let order = Self::next_observation_order(state)?;
            state.resources = live;
            state.reservation.as_mut().expect("checked reservation").external_free = Some((order, bytes));
        } else {
            Self::next_observation_order(state)?;
            state.resources = live;
            state.generation = state.generation.saturating_add(1);
        }
        Ok(())
    }

    #[must_use]
    pub fn boot_identity(&self) -> [u8; 16] {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .boot
    }

    pub fn set_boot_key(&self, key: &[u8]) -> Result<(), SystemLaunchError> {
        if key.is_empty() {
            return Err(SystemLaunchError::InvalidBootKey);
        }
        let mut state = self.state();
        if state.reservation.is_some() {
            return Err(SystemLaunchError::LaunchBusy);
        }
        state.boot = Self::identity(key);
        state.sequence = 1;
        state.generation = state.generation.saturating_add(1);
        Ok(())
    }

    /// Validates and copies a complete launch view without publishing it.
    ///
    /// # Errors
    ///
    /// Returns an error when the boot key is empty, the CPU limit is zero, or
    /// the bounded free-memory value exceeds total memory.
    pub fn prepare_launch(
        self: &Arc<Self>,
        boot_key: &[u8],
        resources: ResourceSnapshot,
    ) -> Result<SystemLaunchUpdate, SystemLaunchError> {
        if boot_key.is_empty() {
            return Err(SystemLaunchError::InvalidBootKey);
        }
        Self::validate_resources(resources)?;
        let mut state = self.state();
        if state.reservation.is_some() {
            return Err(SystemLaunchError::LaunchBusy);
        }
        let generation = Self::next_generation(state.generation)?;
        state.reservation = Some(SystemLaunchReservation {
            generation,
            boot: Self::identity(boot_key),
            resources,
            external_forks: 0,
            external_uptime: None,
            external_free: None,
            construction_free: None,
        });
        Ok(SystemLaunchUpdate {
            authority: Arc::clone(self),
            generation,
            staged: Some((Self::identity(boot_key), resources)),
            routes: Vec::new(),
        })
    }

    #[must_use]
    pub fn random_identity(&self) -> Result<[u8; 16], SystemLaunchError> {
        let (boot, serial) = {
            let mut state = self.state();
            let serial = state.sequence;
            state.sequence = state
                .sequence
                .checked_add(1)
                .ok_or(SystemLaunchError::SequenceExhausted)?;
            (state.boot, serial)
        };
        let mut identity = [0; 16];
        for (index, chunk) in identity.chunks_exact_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (boot, serial, index).hash(&mut hasher);
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
        Ok(identity)
    }

    fn validate_resources(resources: ResourceSnapshot) -> Result<(), SystemLaunchError> {
        if resources.cpu_limit == Some(0)
            || (resources.total_memory == 0 && resources.free_memory != 0)
            || (resources.total_memory != 0 && resources.free_memory > resources.total_memory)
        {
            Err(SystemLaunchError::InvalidResources)
        } else {
            Ok(())
        }
    }

    const fn next_generation(generation: u64) -> Result<u64, SystemLaunchError> {
        match generation.checked_add(1) {
            Some(generation) => Ok(generation),
            None => Err(SystemLaunchError::GenerationExhausted),
        }
    }

    fn next_observation_order(state: &mut SystemState) -> Result<u64, SystemLaunchError> {
        state.observation_order = state
            .observation_order
            .checked_add(1)
            .ok_or(SystemLaunchError::ObservationOrderExhausted)?;
        Ok(state.observation_order)
    }

    fn observe_launch_free(
        state: &mut SystemState,
        token: LaunchToken,
        bytes: u64,
    ) -> Result<(), SystemObservationError> {
        let Some(reservation) = state.reservation else {
            return Err(SystemObservationError::Retired);
        };
        if reservation.generation != token.0 {
            return Err(SystemObservationError::Retired);
        }
        let resources = ResourceSnapshot {
            free_memory: bytes,
            ..reservation.resources
        };
        Self::validate_resources(resources).map_err(|_| SystemObservationError::InvalidResources)?;
        let order = Self::next_observation_order(state).map_err(|_| SystemObservationError::OrderExhausted)?;
        let reservation = state.reservation.as_mut().expect("validated launch reservation");
        reservation.resources = resources;
        reservation.construction_free = Some((order, bytes));
        Ok(())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SystemState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn identity(key: &[u8]) -> [u8; 16] {
        let mut identity = [0; 16];
        for (index, chunk) in identity.chunks_exact_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (key, index).hash(&mut hasher);
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
        identity
    }
}

impl SystemObservationHandle {
    pub fn observe_free_memory(&self, bytes: u64) -> Result<(), SystemObservationError> {
        let mut state = self.authority.state();
        let route = state
            .routes
            .get(&self.route)
            .map(|entry| entry.state)
            .unwrap_or(ObservationRoute::Retired);
        match route {
            ObservationRoute::Pending(token) => SystemAuthority::observe_launch_free(&mut state, token, bytes),
            ObservationRoute::Live => {
                SystemAuthority::observe_free_memory_state(&mut state, bytes).map_err(|error| match error {
                    SystemLaunchError::ObservationOrderExhausted => SystemObservationError::OrderExhausted,
                    _ => SystemObservationError::InvalidResources,
                })
            }
            ObservationRoute::Retired => Err(SystemObservationError::Retired),
        }
    }
}

impl Clone for SystemObservationHandle {
    fn clone(&self) -> Self {
        let mut state = self.authority.state();
        let entry = state.routes.get_mut(&self.route).expect("live handle owns its route");
        entry.handles = entry
            .handles
            .checked_add(1)
            .expect("route handle count is bounded by memory");
        Self {
            authority: Arc::clone(&self.authority),
            route: self.route,
        }
    }
}

impl Drop for SystemObservationHandle {
    fn drop(&mut self) {
        let mut state = self.authority.state();
        let Some(entry) = state.routes.get_mut(&self.route) else {
            return;
        };
        entry.handles -= 1;
        if entry.handles == 0 {
            state.routes.remove(&self.route);
        }
    }
}

impl SystemLaunchUpdate {
    pub fn construction_observer(&mut self) -> SystemObservationHandle {
        let mut state = self.authority.state();
        let route = loop {
            let candidate = state.next_route;
            state.next_route = state.next_route.wrapping_add(1);
            if !state.routes.contains_key(&candidate) {
                break candidate;
            }
        };
        state.routes.insert(
            route,
            RouteEntry {
                state: ObservationRoute::Pending(LaunchToken(self.generation)),
                handles: 1,
            },
        );
        self.routes.push(route);
        SystemObservationHandle {
            authority: Arc::clone(&self.authority),
            route,
        }
    }

    /// Atomically publishes boot identity, resources, and the reset sequence.
    ///
    /// # Errors
    ///
    /// The exclusive preparation permit makes this final publication infallible.
    pub fn commit(self) {
        self.commit_with(|| {});
    }

    fn commit_with(mut self, before_routes: impl FnOnce()) {
        self.staged.take().expect("launch permit commits once");
        let mut state = self
            .authority
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reservation = state.reservation.take().expect("launch reservation exists");
        assert_eq!(reservation.generation, self.generation);
        let generation = self.generation;
        let mut resources = reservation.resources;
        resources.process_creations = resources.process_creations.saturating_add(reservation.external_forks);
        if let Some(uptime) = reservation.external_uptime {
            resources.uptime_seconds = resources.uptime_seconds.max(uptime);
        }
        if let Some((external_order, external_free)) = reservation.external_free {
            if reservation
                .construction_free
                .is_none_or(|(construction_order, _)| external_order > construction_order)
            {
                resources.free_memory = external_free;
            }
        }
        state.boot = reservation.boot;
        state.resources = resources;
        state.sequence = 1;
        state.generation = generation;
        before_routes();
        for route in &self.routes {
            if let Some(entry) = state.routes.get_mut(route) {
                entry.state = ObservationRoute::Live;
            }
        }
    }
}

impl Drop for SystemLaunchUpdate {
    fn drop(&mut self) {
        if self.staged.is_none() {
            return;
        }
        let mut state = self
            .authority
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .reservation
            .is_some_and(|reservation| reservation.generation == self.generation)
        {
            state.reservation = None;
        }
        for route in &self.routes {
            if let Some(entry) = state.routes.get_mut(route) {
                entry.state = ObservationRoute::Retired;
            }
        }
    }
}

impl Default for SystemAuthority {
    fn default() -> Self {
        Self::new(ResourceSnapshot::default()).expect("default resources are valid")
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourceSnapshot, SystemAuthority, SystemLaunchError, SystemObservationError};
    use std::sync::{Arc, Barrier};

    #[test]
    fn visible_memory_distinguishes_absent_observation_from_limit() {
        assert_eq!(ResourceSnapshot::default().visible_memory(), (8_u64 << 30, 2_u64 << 30));
        assert_eq!(
            ResourceSnapshot {
                total_memory: 4096,
                free_memory: 8192,
                ..ResourceSnapshot::default()
            }
            .visible_memory(),
            (4096, 4096),
        );
    }

    #[test]
    fn successful_forks_accumulate() {
        let system = Arc::new(SystemAuthority::default());
        system.observe_fork();
        system.observe_fork();
        assert_eq!(system.snapshot().process_creations, 2);
    }

    #[test]
    fn concurrent_memory_observation_preserves_other_fields() {
        let system = Arc::new(
            SystemAuthority::new(ResourceSnapshot {
                total_memory: 4096,
                free_memory: 4096,
                ..ResourceSnapshot::default()
            })
            .unwrap(),
        );
        let start = Arc::new(Barrier::new(3));
        let resources = Arc::clone(&system);
        let resource_start = Arc::clone(&start);
        let memory = std::thread::spawn(move || {
            resource_start.wait();
            for free in 0..1000 {
                resources.observe_free_memory(free).unwrap();
            }
        });
        let activity = Arc::clone(&system);
        let activity_start = Arc::clone(&start);
        let counters = std::thread::spawn(move || {
            activity_start.wait();
            for uptime in 0..1000 {
                activity.observe_uptime(uptime);
                activity.observe_fork();
            }
        });
        start.wait();
        memory.join().unwrap();
        counters.join().unwrap();
        let snapshot = system.snapshot();
        assert_eq!(snapshot.free_memory, 999);
        assert_eq!(snapshot.uptime_seconds, 999);
        assert_eq!(snapshot.process_creations, 1000);
    }

    #[test]
    fn boot_identity_is_stable() {
        let first = SystemAuthority::default();
        let second = SystemAuthority::default();
        first.set_boot_key(b"container-a").unwrap();
        second.set_boot_key(b"container-a").unwrap();
        assert_eq!(first.boot_identity(), first.boot_identity());
        assert_eq!(first.boot_identity(), second.boot_identity());
        second.set_boot_key(b"container-b").unwrap();
        assert_ne!(first.boot_identity(), second.boot_identity());
    }

    #[test]
    fn random_identity_is_fresh() {
        let system = Arc::new(SystemAuthority::default());
        assert_ne!(system.random_identity().unwrap(), system.random_identity().unwrap());
    }

    #[test]
    fn launch_validation_and_abort_do_not_publish() {
        let system = Arc::new(SystemAuthority::default());
        let before = {
            let state = system.state.lock().unwrap();
            (state.boot, state.resources, state.sequence, state.generation)
        };
        assert!(matches!(
            system.prepare_launch(b"", ResourceSnapshot::default()),
            Err(SystemLaunchError::InvalidBootKey)
        ));
        assert!(matches!(
            system.prepare_launch(
                b"next",
                ResourceSnapshot {
                    total_memory: 1,
                    free_memory: 2,
                    ..ResourceSnapshot::default()
                }
            ),
            Err(SystemLaunchError::InvalidResources)
        ));
        drop(system.prepare_launch(b"next", ResourceSnapshot::default()).unwrap());
        let after = {
            let state = system.state.lock().unwrap();
            (state.boot, state.resources, state.sequence, state.generation)
        };
        assert_eq!(after, before);
    }

    #[test]
    fn launch_commit_resets_sequence() {
        let system = Arc::new(SystemAuthority::default());
        let expected = Arc::new(SystemAuthority::default());
        expected.set_boot_key(b"next").unwrap();
        let expected_first = expected.random_identity().unwrap();
        let resources = ResourceSnapshot {
            total_memory: 8192,
            free_memory: 4096,
            ..ResourceSnapshot::default()
        };
        let winner = system.prepare_launch(b"next", resources).unwrap();
        winner.commit();
        assert_eq!(system.snapshot(), resources);
        assert_eq!(system.random_identity().unwrap(), expected_first);
    }

    #[test]
    fn launch_observation_routes_commit_and_abort_by_origin() {
        let system = Arc::new(
            SystemAuthority::new(ResourceSnapshot {
                total_memory: 100,
                free_memory: 100,
                ..ResourceSnapshot::default()
            })
            .unwrap(),
        );
        let staged = ResourceSnapshot {
            total_memory: 200,
            free_memory: 200,
            ..ResourceSnapshot::default()
        };
        let mut aborted = system.prepare_launch(b"aborted", staged).unwrap();
        let retired = aborted.construction_observer();
        retired.observe_free_memory(150).unwrap();
        system.observe_fork();
        system.observe_uptime(9);
        system.observe_free_memory(80).unwrap();
        drop(aborted);
        assert_eq!(retired.observe_free_memory(70), Err(SystemObservationError::Retired));
        assert_eq!(system.snapshot().free_memory, 80);
        assert_eq!(system.snapshot().process_creations, 1);
        assert_eq!(system.snapshot().uptime_seconds, 9);

        let mut committed = system.prepare_launch(b"committed", staged).unwrap();
        let live = committed.construction_observer();
        live.observe_free_memory(150).unwrap();
        system.observe_free_memory(70).unwrap();
        system.observe_fork();
        system.observe_uptime(11);
        committed.commit();
        assert_eq!(system.snapshot().free_memory, 70);
        assert_eq!(system.snapshot().process_creations, 1);
        assert_eq!(system.snapshot().uptime_seconds, 11);
        live.observe_free_memory(60).unwrap();
        assert_eq!(system.snapshot().free_memory, 60);
    }

    #[test]
    fn promotion_is_atomic() {
        let system = Arc::new(
            SystemAuthority::new(ResourceSnapshot {
                total_memory: 200,
                free_memory: 200,
                ..ResourceSnapshot::default()
            })
            .unwrap(),
        );
        let mut update = system.prepare_launch(b"next", system.snapshot()).unwrap();
        let observer = update.construction_observer();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let commit_entered = Arc::clone(&entered);
        let commit_release = Arc::clone(&release);
        let commit = std::thread::spawn(move || {
            update.commit_with(|| {
                commit_entered.wait();
                commit_release.wait();
            });
        });
        entered.wait();
        let (sent, received) = std::sync::mpsc::channel();
        let observation = std::thread::spawn(move || sent.send(observer.observe_free_memory(150)).unwrap());
        assert!(received.recv_timeout(std::time::Duration::from_millis(20)).is_err());
        release.wait();
        commit.join().unwrap();
        assert_eq!(
            received.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            Ok(())
        );
        observation.join().unwrap();
        assert_eq!(system.snapshot().free_memory, 150);
    }

    #[test]
    fn routes_change_together() {
        let system = Arc::new(
            SystemAuthority::new(ResourceSnapshot {
                total_memory: 200,
                free_memory: 200,
                ..ResourceSnapshot::default()
            })
            .unwrap(),
        );
        let mut committed = system.prepare_launch(b"committed", system.snapshot()).unwrap();
        let first = committed.construction_observer();
        let second = committed.construction_observer();
        committed.commit();
        assert_eq!(first.observe_free_memory(180), Ok(()));
        assert_eq!(second.observe_free_memory(170), Ok(()));

        let mut aborted = system.prepare_launch(b"aborted", system.snapshot()).unwrap();
        let first = aborted.construction_observer();
        let second = aborted.construction_observer();
        drop(aborted);
        assert_eq!(first.observe_free_memory(160), Err(SystemObservationError::Retired));
        assert_eq!(second.observe_free_memory(160), Err(SystemObservationError::Retired));
    }

    #[test]
    fn launch_reservation_stages_observer_without_blocking() {
        let system = Arc::new(SystemAuthority::default());
        let update = system.prepare_launch(b"next", ResourceSnapshot::default()).unwrap();
        let observer = Arc::clone(&system);
        let (sent, received) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            observer.observe_fork();
            sent.send(()).unwrap();
        });
        received.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        assert_eq!(system.snapshot().process_creations, 1);
        update.commit();
        assert_eq!(system.snapshot().process_creations, 1);
    }

    #[test]
    fn launch_observers_never_see_a_mixed_tuple() {
        let system = Arc::new(
            SystemAuthority::new(ResourceSnapshot {
                total_memory: 100,
                free_memory: 50,
                ..ResourceSnapshot::default()
            })
            .unwrap(),
        );
        system.set_boot_key(b"old").unwrap();
        let old_boot = system.boot_identity();
        let next_resources = ResourceSnapshot {
            total_memory: 200,
            free_memory: 75,
            ..ResourceSnapshot::default()
        };
        let next_boot = SystemAuthority::identity(b"new");
        let start = Arc::new(Barrier::new(2));
        let observer_system = Arc::clone(&system);
        let observer_start = Arc::clone(&start);
        let observer = std::thread::spawn(move || {
            observer_start.wait();
            for _ in 0..10_000 {
                let state = observer_system.state.lock().unwrap();
                assert!(
                    (state.boot == old_boot && state.resources.total_memory == 100)
                        || (state.boot == next_boot && state.resources == next_resources)
                );
            }
        });
        let update = system.prepare_launch(b"new", next_resources).unwrap();
        start.wait();
        update.commit();
        observer.join().unwrap();
    }

    #[test]
    fn public_mutations_validate_and_exhaust_without_partial_write() {
        assert!(matches!(
            SystemAuthority::new(ResourceSnapshot {
                cpu_limit: Some(0),
                ..ResourceSnapshot::default()
            }),
            Err(SystemLaunchError::InvalidResources)
        ));
        let system = Arc::new(SystemAuthority::default());
        assert_eq!(system.set_boot_key(b""), Err(SystemLaunchError::InvalidBootKey));
        assert_eq!(
            system.replace(ResourceSnapshot {
                total_memory: 1,
                free_memory: 2,
                ..ResourceSnapshot::default()
            }),
            Err(SystemLaunchError::InvalidResources)
        );
        {
            let mut state = system.state.lock().unwrap();
            state.generation = u64::MAX;
        }
        assert!(matches!(
            system.prepare_launch(b"next", ResourceSnapshot::default()),
            Err(SystemLaunchError::GenerationExhausted)
        ));
        system.observe_fork();
        assert_eq!(system.snapshot().process_creations, 1);
        {
            let mut state = system.state.lock().unwrap();
            state.sequence = u64::MAX;
        }
        assert_eq!(system.random_identity(), Err(SystemLaunchError::SequenceExhausted));
    }
}
