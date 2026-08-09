use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Execution-side effects required when memory projection becomes necessary.
pub trait ProjectionControl: Send + Sync {
    fn activate(&self);
    fn begin_transition(&self);
    fn end_transition(&self);
}

/// One-way gate from architectural addresses to projected memory storage.
///
/// Generations are monotonic. A stale observation cannot alter the gate, and
/// activation is published once before its control callback runs. An inactive
/// observation advances the generation but cannot disable an already active
/// projection because existing translated code may still contain guards.
pub struct MemoryProjection<Q, C> {
    query: Option<Q>,
    control: C,
    generation: AtomicU64,
    active: AtomicBool,
}

impl<Q, C> MemoryProjection<Q, C>
where
    Q: Fn(u64, u64) -> u64 + Send + Sync,
    C: ProjectionControl,
{
    pub fn new(control: C) -> Self {
        Self {
            query: None,
            control,
            generation: AtomicU64::new(0),
            active: AtomicBool::new(false),
        }
    }

    pub fn install(&mut self, query: Q, active: bool, generation: u64) {
        self.query = Some(query);
        self.observe(generation, active);
    }

    pub fn observe(&self, generation: u64, active: bool) {
        let mut seen = self.generation.load(Ordering::Acquire);
        while generation > seen {
            match self
                .generation
                .compare_exchange_weak(seen, generation, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => seen = current,
            }
        }
        if generation < seen {
            hl_log::hl_debug!(
                hl_log::tag::CPU,
                "projection observation dropped as stale generation={generation} seen={seen} active={active}"
            );
            return;
        }
        if active && !self.active.swap(true, Ordering::AcqRel) {
            hl_log::hl_debug!(hl_log::tag::CPU, "projection activated generation={generation}");
            self.control.activate();
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn resolve(&self, address: u64, size: u64) -> u64 {
        self.query.as_ref().map_or(0, |query| query(address, size))
    }

    pub fn transition(&self) -> ProjectionTransition<'_, C> {
        self.begin_transition();
        ProjectionTransition { control: &self.control }
    }

    pub fn begin_transition(&self) {
        self.control.begin_transition();
    }

    pub fn end_transition(&self) {
        self.control.end_transition();
    }
}

pub struct ProjectionTransition<'a, C: ProjectionControl> {
    control: &'a C,
}

impl<C: ProjectionControl> Drop for ProjectionTransition<'_, C> {
    fn drop(&mut self) {
        self.control.end_transition();
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{MemoryProjection, ProjectionControl};

    #[derive(Default)]
    struct Capture {
        activations: AtomicUsize,
        beginnings: AtomicUsize,
        endings: AtomicUsize,
    }

    struct Control(Arc<Capture>);

    impl ProjectionControl for Control {
        fn activate(&self) {
            self.0.activations.fetch_add(1, Ordering::Relaxed);
        }

        fn begin_transition(&self) {
            self.0.beginnings.fetch_add(1, Ordering::Relaxed);
        }

        fn end_transition(&self) {
            self.0.endings.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn projection_lifecycle() {
        let capture = Arc::new(Capture::default());
        let mut projection = MemoryProjection::new(Control(Arc::clone(&capture)));

        assert!(!projection.is_active());
        assert_eq!(projection.resolve(7, 5), 0);

        projection.install(|address, size| address + size, true, 4);
        assert!(projection.is_active());
        assert_eq!(capture.activations.load(Ordering::Relaxed), 1);
        assert_eq!(projection.resolve(7, 5), 12);

        projection.observe(3, false);
        assert!(projection.is_active());

        projection.observe(5, false);
        assert!(projection.is_active());
        assert_eq!(capture.activations.load(Ordering::Relaxed), 1);

        {
            let _transition = projection.transition();
            assert_eq!(capture.beginnings.load(Ordering::Relaxed), 1);
            assert_eq!(capture.endings.load(Ordering::Relaxed), 0);
        }
        assert_eq!(capture.endings.load(Ordering::Relaxed), 1);
    }
}
