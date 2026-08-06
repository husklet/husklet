use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};

use hl_execution::StepOutcome;

use super::threads::ThreadRun;

// Retained-oracle audit: `../engine/src/linux_abi/sentry.c` entry points
// `sentry_init`, `sentry_loop`, `sentry_ring_thread`, and `sentry_ring_loop`
// own one process-wide 64-lane array and spawn detached servicers over shared
// descriptor authority. A failed C `pthread_create` leaves a claimed lane
// unserviced and process exit is its only teardown. This Rust owner instead
// keeps subscription, queue endpoints, and joinable worker lifetime together;
// partial construction is cancelled and joined before launch failure escapes.
// Lane service and shutdown have no ISA- or host-specific branches.

// Match the retained sentry's bounded lane count: a guest process may have
// many legitimately parked syscalls whose peer threads must still run.
const WORKER_COUNT: usize = 64;
const INITIAL_WORKER_COUNT: usize = 4;
const WORKER_STACK_SIZE: usize = 512 * 1024;
enum Job {
    Dispatch(ThreadRun),
    Stop,
}

pub(super) enum Event {
    Completion(Completion),
    OrdinarySignal,
    Signal(hl_task::SignalActivityEvent),
}

pub(super) struct Completion {
    pub(super) run: ThreadRun,
    pub(super) outcome: StepOutcome,
}

pub(super) struct Pool {
    lanes: Mutex<Vec<Lane>>,
    next: AtomicUsize,
    events: mpsc::Receiver<Event>,
    completed: mpsc::Sender<Event>,
    _signals: hl_task::SignalActivitySubscription,
    workers: Mutex<Vec<JoinHandle<()>>>,
    #[cfg(test)]
    active: Arc<AtomicUsize>,
    #[cfg(test)]
    reject_next: AtomicBool,
}

struct Lane {
    jobs: mpsc::SyncSender<Job>,
    busy: Arc<AtomicBool>,
}

struct Worker;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WaiterStartError {
    // The task registry currently subscribes infallibly; retain a distinct
    // transaction phase so a future bounded subscriber cannot be flattened
    // into thread construction failure.
    #[cfg_attr(not(test), allow(dead_code))]
    Subscribe,
    Spawn,
    Ready,
}

struct StartTransaction {
    lanes: Vec<Lane>,
    workers: Vec<JoinHandle<()>>,
    signals: Option<hl_task::SignalActivitySubscription>,
}

impl StartTransaction {
    fn rollback(&mut self) {
        for lane in &self.lanes {
            let _ = lane.jobs.send(Job::Stop);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.lanes.clear();
        self.signals.take();
    }
}

impl Drop for StartTransaction {
    fn drop(&mut self) {
        self.rollback();
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum StartFailure {
    Subscribe,
    Spawn(usize),
    Ready(usize),
}

#[cfg(test)]
struct Active(Arc<AtomicUsize>);

#[cfg(test)]
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Worker {
    fn serve(
        receiver: mpsc::Receiver<Job>,
        busy: Arc<AtomicBool>,
        completed: mpsc::Sender<Event>,
        ready: mpsc::Sender<()>,
        #[cfg(test)] active: Arc<AtomicUsize>,
    ) {
        #[cfg(test)]
        let _active = {
            active.fetch_add(1, Ordering::AcqRel);
            Active(active)
        };
        if ready.send(()).is_err() {
            return;
        }
        while let Some(run) = Self::receive(&receiver) {
            let outcome = hl_runtime::dispatch_runtime_syscall(&run.machine, 1, run.router.as_ref());
            busy.store(false, Ordering::Release);
            if completed.send(Event::Completion(Completion { run, outcome })).is_err() {
                return;
            }
        }
    }

    fn receive(receiver: &mpsc::Receiver<Job>) -> Option<ThreadRun> {
        match receiver.recv() {
            Ok(Job::Dispatch(run)) => Some(run),
            Ok(Job::Stop) | Err(_) => None,
        }
    }
}

impl Pool {
    pub(super) fn new(tasks: &Arc<hl_task::TaskRegistry>) -> Result<Self, WaiterStartError> {
        Self::start(
            tasks,
            None,
            #[cfg(test)]
            None,
        )
    }

    fn start(
        tasks: &Arc<hl_task::TaskRegistry>,
        #[cfg(test)] failure: Option<StartFailure>,
        #[cfg(not(test))] _failure: Option<()>,
        #[cfg(test)] injected_active: Option<Arc<AtomicUsize>>,
    ) -> Result<Self, WaiterStartError> {
        let (completed, events) = mpsc::channel();
        let observer: Arc<dyn hl_task::SignalActivityWake> = Arc::new(SignalWake(completed.clone()));
        #[cfg(test)]
        if matches!(failure, Some(StartFailure::Subscribe)) {
            return Err(WaiterStartError::Subscribe);
        }
        let signals = tasks.subscribe_signal_activity(observer);
        let mut transaction = StartTransaction {
            lanes: Vec::with_capacity(WORKER_COUNT),
            workers: Vec::with_capacity(WORKER_COUNT),
            signals: Some(signals),
        };
        #[cfg(test)]
        let active = injected_active.unwrap_or_else(|| Arc::new(AtomicUsize::new(0)));
        for index in 0..INITIAL_WORKER_COUNT {
            #[cfg(test)]
            if matches!(failure, Some(StartFailure::Spawn(failed)) if failed == index) {
                return Err(WaiterStartError::Spawn);
            }
            let (lane, worker, started) = Self::spawn_lane_unchecked(
                index,
                completed.clone(),
                #[cfg(test)]
                Arc::clone(&active),
                #[cfg(test)]
                matches!(failure, Some(StartFailure::Ready(failed)) if failed == index),
            )
            .map_err(|()| WaiterStartError::Spawn)?;
            transaction.lanes.push(lane);
            transaction.workers.push(worker);
            started.recv().map_err(|_| WaiterStartError::Ready)?;
        }
        let lanes = std::mem::take(&mut transaction.lanes);
        let workers = std::mem::take(&mut transaction.workers);
        let signals = transaction.signals.take().ok_or(WaiterStartError::Subscribe)?;
        Ok(Self {
            lanes: Mutex::new(lanes),
            next: AtomicUsize::new(0),
            events,
            completed,
            _signals: signals,
            workers: Mutex::new(workers),
            #[cfg(test)]
            active,
            #[cfg(test)]
            reject_next: AtomicBool::new(false),
        })
    }

    fn spawn_lane(
        index: usize,
        completed: mpsc::Sender<Event>,
        #[cfg(test)] active: Arc<AtomicUsize>,
    ) -> Result<(Lane, JoinHandle<()>), ()> {
        let (lane, worker, started) = Self::spawn_lane_unchecked(
            index,
            completed,
            #[cfg(test)]
            active,
            #[cfg(test)]
            false,
        )?;
        if started.recv().is_err() {
            let _ = lane.jobs.send(Job::Stop);
            let _ = worker.join();
            return Err(());
        }
        Ok((lane, worker))
    }

    fn spawn_lane_unchecked(
        index: usize,
        completed: mpsc::Sender<Event>,
        #[cfg(test)] active: Arc<AtomicUsize>,
        #[cfg(test)] fail_ready: bool,
    ) -> Result<(Lane, JoinHandle<()>, mpsc::Receiver<()>), ()> {
        let (jobs, receiver) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let (ready, started) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(format!("hl-waiter-{index}"))
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                #[cfg(test)]
                if fail_ready {
                    return;
                }
                Worker::serve(
                    receiver,
                    worker_busy,
                    completed,
                    ready,
                    #[cfg(test)]
                    active,
                );
            })
            .map_err(|_| ())?;
        Ok((Lane { jobs, busy }, worker, started))
    }

    pub(super) fn dispatch(&self, run: ThreadRun) -> Result<(), ThreadRun> {
        #[cfg(test)]
        if self.reject_next.swap(false, Ordering::AcqRel) {
            return Err(run);
        }
        let mut lanes = self.lanes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = self.next.fetch_add(1, Ordering::Relaxed) % lanes.len();
        let mut run = run;
        for offset in 0..lanes.len() {
            let lane = &lanes[(start + offset) % lanes.len()];
            if lane
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            match lane.jobs.try_send(Job::Dispatch(run)) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(Job::Dispatch(returned)) |
mpsc::TrySendError::Disconnected(Job::Dispatch(returned))) => {
                    lane.busy.store(false, Ordering::Release);
                    run = returned;
                }
                Err(_) => unreachable!(),
            }
        }
        if lanes.len() < WORKER_COUNT {
            let index = lanes.len();
            let spawned = Self::spawn_lane(
                index,
                self.completed.clone(),
                #[cfg(test)]
                Arc::clone(&self.active),
            );
            let Ok((lane, worker)) = spawned else { return Err(run) };
            lane.busy.store(true, Ordering::Release);
            lane.jobs.send(Job::Dispatch(run)).map_err(|error| match error.0 {
                Job::Dispatch(run) => run,
                Job::Stop => unreachable!(),
            })?;
            lanes.push(lane);
            self.workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(worker);
            return Ok(());
        }
        Err(run)
    }

    #[cfg(test)]
    pub(super) fn reject_next(&self) {
        self.reject_next.store(true, Ordering::Release);
    }

    pub(super) fn completed(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub(super) fn wait(&self) -> Option<Event> {
        self.events.recv_timeout(std::time::Duration::from_millis(1)).ok()
    }

    pub(super) fn stop(mut self) {
        let lanes = self.lanes.get_mut().unwrap_or_else(|error| error.into_inner());
        for lane in lanes {
            let _ = lane.jobs.send(Job::Stop);
        }
        let workers = self.workers.get_mut().unwrap_or_else(|error| error.into_inner());
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

struct SignalWake(mpsc::Sender<Event>);

impl hl_task::SignalActivityWake for SignalWake {
    fn signal_activity_changed(&self) {
        let _ = self.0.send(Event::OrdinarySignal);
    }

    fn process_control_activity(&self, activity: hl_task::SignalActivityEvent) {
        let _ = self.0.send(Event::Signal(activity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Pool {
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        Pool::new(&tasks).unwrap()
    }

    #[test]
    fn workers_join() {
        for _ in 0..3 {
            let pool = pool();
            let active = Arc::clone(&pool.active);
            assert_eq!(active.load(Ordering::Acquire), INITIAL_WORKER_COUNT);
            pool.stop();
            assert_eq!(active.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn bounded_growth_joins() {
        let pool = pool();
        let active = Arc::clone(&pool.active);
        {
            let mut lanes = pool.lanes.lock().unwrap();
            let mut workers = pool.workers.lock().unwrap();
            for index in INITIAL_WORKER_COUNT..WORKER_COUNT {
                let (lane, worker) = Pool::spawn_lane(index, pool.completed.clone(), Arc::clone(&active)).unwrap();
                lanes.push(lane);
                workers.push(worker);
            }
        }
        assert_eq!(active.load(Ordering::Acquire), WORKER_COUNT);
        pool.stop();
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn partial_start_rolls_back_every_started_lane() {
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        for failure in [
            StartFailure::Spawn(0),
            StartFailure::Spawn(INITIAL_WORKER_COUNT / 2),
            StartFailure::Spawn(INITIAL_WORKER_COUNT - 1),
            StartFailure::Ready(0),
            StartFailure::Ready(INITIAL_WORKER_COUNT / 2),
            StartFailure::Ready(INITIAL_WORKER_COUNT - 1),
        ] {
            for _ in 0..8 {
                let active = Arc::new(AtomicUsize::new(0));
                let result = Pool::start(&tasks, Some(failure), Some(Arc::clone(&active)));
                assert!(matches!(result, Err(WaiterStartError::Spawn | WaiterStartError::Ready)));
                assert_eq!(active.load(Ordering::Acquire), 0);
            }
        }
        let pool = Pool::start(&tasks, None, None).unwrap();
        assert_eq!(pool.active.load(Ordering::Acquire), INITIAL_WORKER_COUNT);
        pool.stop();
    }

    #[test]
    fn subscription_failure_starts_no_lane() {
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        assert!(matches!(
            Pool::start(&tasks, Some(StartFailure::Subscribe), None),
            Err(WaiterStartError::Subscribe)
        ));
        let pool = Pool::start(&tasks, None, None).unwrap();
        assert_eq!(pool.active.load(Ordering::Acquire), INITIAL_WORKER_COUNT);
        pool.stop();
    }
}
