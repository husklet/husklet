use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEADLINE_LIMIT: usize = 1024;

struct State {
    next: u64,
    stopped: bool,
    deadlines: BTreeSet<(u64, u64)>,
    tokens: BTreeMap<u64, u64>,
    callbacks: BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>,
}

struct QueueState {
    origin: Instant,
    descriptor: i32,
    state: Mutex<State>,
    changed: Condvar,
    scheduled: Condvar,
}

struct InterruptionWake(Arc<QueueState>);

impl hl_sync::InterruptionWake for InterruptionWake {
    fn wake(&self) {
        let _state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        self.0.changed.notify_all();
    }
}

pub(in crate::ffi::linux::execution) struct Queue {
    inner: Arc<QueueState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Queue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        formatter
            .debug_struct("Queue")
            .field("registrations", &state.tokens.len())
            .finish()
    }
}

impl Queue {
    pub(in crate::ffi::linux::execution) fn new() -> io::Result<Arc<Self>> {
        // SAFETY: eventfd accepts scalar values, returns one owned descriptor,
        // retains no Rust storage, and cannot unwind into Rust.
        let descriptor = unsafe {
            eventfd(
                0,
                super::super::super::abi::O_NONBLOCK | super::super::super::abi::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let inner = Arc::new(QueueState {
            origin: Instant::now(),
            descriptor,
            state: Mutex::new(State {
                next: 1,
                stopped: false,
                deadlines: BTreeSet::new(),
                tokens: BTreeMap::new(),
                callbacks: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            scheduled: Condvar::new(),
        });
        Ok(Arc::new(Self {
            inner,
            worker: Mutex::new(None),
        }))
    }

    pub(in crate::ffi::linux::execution) fn descriptor(&self) -> i32 {
        self.inner.descriptor
    }

    pub(in crate::ffi::linux::execution) fn elapsed(&self) -> Duration {
        self.inner.origin.elapsed()
    }

    pub(in crate::ffi::linux::execution) fn schedule(&self, deadline: u64) -> Result<u64, hl_time::ClockError> {
        self.insert(deadline, None)
    }

    pub(in crate::ffi::linux::execution) fn schedule_callback(
        &self,
        deadline: u64,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<u64, hl_time::ClockError> {
        self.insert(deadline, Some(callback))
    }

    fn insert(&self, deadline: u64, callback: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<u64, hl_time::ClockError> {
        self.start_worker()?;
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.stopped || state.tokens.len() == DEADLINE_LIMIT {
            return Err(hl_time::ClockError::Failed);
        }
        let token = state.next;
        state.next = state.next.wrapping_add(1).max(1);
        state.tokens.insert(token, deadline);
        if let Some(callback) = callback {
            state.callbacks.insert(token, callback);
        }
        state.deadlines.insert((deadline, token));
        self.inner.scheduled.notify_one();
        Ok(token)
    }

    fn start_worker(&self) -> Result<(), hl_time::ClockError> {
        let mut worker = self.worker.lock().map_err(|_| hl_time::ClockError::Failed)?;
        if worker.is_some() {
            return Ok(());
        }
        let inner = Arc::clone(&self.inner);
        *worker = Some(
            std::thread::Builder::new()
                .name(String::from("hl-deadline"))
                .spawn(move || Self::serve(inner))
                .map_err(|_| hl_time::ClockError::Failed)?,
        );
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn cancel(&self, token: u64) {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(deadline) = state.tokens.remove(&token) {
            state.deadlines.remove(&(deadline, token));
            state.callbacks.remove(&token);
            self.inner.scheduled.notify_one();
        }
    }

    pub(in crate::ffi::linux::execution) fn wait_interruptible(
        &self,
        deadline: u64,
        interruption: &hl_sync::Interruption,
    ) -> Result<bool, ()> {
        self.wait_inner(deadline, Some(interruption))
    }

    fn wait_inner(&self, deadline: u64, interruption: Option<&hl_sync::Interruption>) -> Result<bool, ()> {
        let token = self.schedule(deadline).map_err(|_| ())?;
        let observer: Arc<dyn hl_sync::InterruptionWake> = Arc::new(InterruptionWake(Arc::clone(&self.inner)));
        let _observation = interruption.map(|value| value.observe(observer.clone()));
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        while !state.stopped
            && !interruption.is_some_and(hl_sync::Interruption::is_pending)
            && u64::try_from(self.inner.origin.elapsed().as_nanos()).unwrap_or(u64::MAX) < deadline
        {
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        let stopped = state.stopped;
        let interrupted = interruption.is_some_and(hl_sync::Interruption::take_pending);
        drop(state);
        self.cancel(token);
        if stopped { Err(()) } else { Ok(interrupted) }
    }

    pub(in crate::ffi::linux::execution) fn drain(&self) {
        let mut value = 0_u64;
        // SAFETY: value is uniquely writable for eight bytes, descriptor is
        // owned by self, and nonblocking read retains no pointer.
        let _ = unsafe { read(self.inner.descriptor, (&mut value as *mut u64).cast(), 8) };
    }

    fn serve(inner: Arc<QueueState>) {
        let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if state.stopped {
                return;
            }
            let Some((deadline, token)) = state.deadlines.first().copied() else {
                state = inner.scheduled.wait(state).unwrap_or_else(|error| error.into_inner());
                continue;
            };
            let now = u64::try_from(inner.origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if now < deadline {
                let wait = Duration::from_nanos(deadline - now);
                state = inner
                    .scheduled
                    .wait_timeout(state, wait)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
                continue;
            }
            state.deadlines.remove(&(deadline, token));
            state.tokens.remove(&token);
            let callback = state.callbacks.remove(&token);
            drop(state);
            if let Some(callback) = callback {
                callback();
            }
            let value = 1_u64;
            // SAFETY: value is readable for eight bytes, the worker shares the
            // live owned descriptor, and write retains no pointer. Callback
            // effects are published before readiness becomes observable.
            let _ = unsafe { write(inner.descriptor, (&value as *const u64).cast(), 8) };
            state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            inner.changed.notify_all();
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            state.stopped = true;
            self.inner.changed.notify_all();
            self.inner.scheduled.notify_all();
        }
        if let Some(worker) = self.worker.lock().unwrap_or_else(|error| error.into_inner()).take() {
            let _ = worker.join();
        }
        // SAFETY: the worker has joined and self surrenders its owned descriptor once.
        let _ = unsafe { close(self.inner.descriptor) };
    }
}

unsafe extern "C" {
    fn eventfd(initial: u32, flags: i32) -> i32;
    fn read(descriptor: i32, output: *mut core::ffi::c_void, length: usize) -> isize;
    fn write(descriptor: i32, input: *const core::ffi::c_void, length: usize) -> isize;
    fn close(descriptor: i32) -> i32;
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn worker_starts_with_first_deadline() {
        let queue = Queue::new().unwrap();
        assert!(queue.worker.lock().unwrap().is_none());
        let token = queue.schedule(u64::MAX).unwrap();
        assert!(queue.worker.lock().unwrap().is_some());
        queue.cancel(token);
    }

    #[test]
    fn callback_completes_before_readiness_is_published() {
        #[derive(Default)]
        struct Gate {
            entered: bool,
            released: bool,
        }

        let queue = Queue::new().unwrap();
        let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
        let callback_gate = Arc::clone(&gate);
        queue
            .schedule_callback(
                0,
                Arc::new(move || {
                    let (state, changed) = &*callback_gate;
                    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                    state.entered = true;
                    changed.notify_all();
                    while !state.released {
                        state = changed.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                }),
            )
            .unwrap();

        let (state, changed) = &*gate;
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        while !state.entered {
            state = changed.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        let mut descriptor = libc::pollfd {
            fd: queue.descriptor(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the call duration.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        state.released = true;
        changed.notify_all();
        drop(state);
        // SAFETY: descriptor points to one initialized pollfd for the call duration.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1_000) }, 1);
    }

    #[test]
    fn capacity_releases() {
        let queue = Queue::new().unwrap();
        let mut tokens = Vec::new();
        for _ in 0..DEADLINE_LIMIT {
            tokens.push(queue.schedule(u64::MAX).unwrap());
        }
        assert_eq!(queue.schedule(u64::MAX), Err(hl_time::ClockError::Failed));
        queue.cancel(tokens[0]);
        assert!(queue.schedule(u64::MAX).is_ok());
        drop(queue);
    }

    #[test]
    fn earlier_sleep_reschedules_worker() {
        let queue = Queue::new().unwrap();
        let (completed, receiver) = mpsc::channel();
        let origin = u64::try_from(queue.elapsed().as_nanos()).unwrap();
        let long_queue = Arc::clone(&queue);
        let long_completed = completed.clone();
        let long = std::thread::spawn(move || {
            assert!(
                !long_queue
                    .wait_interruptible(origin + 200_000_000, &hl_sync::Interruption::new())
                    .unwrap()
            );
            long_completed.send("long").unwrap();
        });
        std::thread::sleep(Duration::from_millis(10));
        let short_queue = Arc::clone(&queue);
        let short = std::thread::spawn(move || {
            assert!(
                !short_queue
                    .wait_interruptible(origin + 60_000_000, &hl_sync::Interruption::new())
                    .unwrap()
            );
            completed.send("short").unwrap();
        });

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), "short");
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), "long");
        long.join().unwrap();
        short.join().unwrap();
    }

    #[test]
    fn interruption_wakes_sleep() {
        let queue = Queue::new().unwrap();
        let interruption = hl_sync::Interruption::new();
        let worker_queue = Arc::clone(&queue);
        let worker_interruption = interruption.clone();
        let worker =
            std::thread::spawn(move || worker_queue.wait_interruptible(u64::MAX, &worker_interruption).unwrap());
        std::thread::sleep(Duration::from_millis(10));
        interruption.interrupt();
        assert!(worker.join().unwrap());
    }
}
