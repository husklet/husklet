use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration as HostDuration;

use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, Readiness, StatusFlags};
use hl_time::{ClockError, Duration, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use crate::{TimerClockSource, TimerFd, TimerFdClock, TimerFdCreateFlags, TimerFdError, TimerFdSetFlags, TimerSetting};

#[derive(Default)]
struct ManualClock {
    monotonic: AtomicU64,
    realtime: AtomicU64,
    generation: AtomicU64,
    wake_token: AtomicU64,
    scheduled: Mutex<Vec<(u64, u64)>>,
    callbacks: Mutex<Vec<(u64, u64, Arc<dyn Fn() + Send + Sync>)>>,
    canceled: Mutex<Vec<u64>>,
}

impl ManualClock {
    fn with_times(monotonic: u64, realtime: u64) -> Self {
        Self {
            monotonic: AtomicU64::new(monotonic),
            realtime: AtomicU64::new(realtime),
            generation: AtomicU64::new(0),
            wake_token: AtomicU64::new(0),
            scheduled: Mutex::new(Vec::new()),
            callbacks: Mutex::new(Vec::new()),
            canceled: Mutex::new(Vec::new()),
        }
    }

    fn advance_monotonic(&self, delta: u64) {
        self.monotonic.fetch_add(delta, Ordering::SeqCst);
    }

    fn advance_realtime(&self, delta: u64) {
        self.realtime.fetch_add(delta, Ordering::SeqCst);
    }

    fn set_realtime(&self, value: u64) {
        self.realtime.store(value, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn fire(&self, now: u64) {
        self.monotonic.store(now, Ordering::SeqCst);
        let callbacks = {
            let mut callbacks = self.callbacks.lock().unwrap();
            let mut ready = Vec::new();
            let mut pending = Vec::new();
            for callback in callbacks.drain(..) {
                if callback.0 <= now {
                    ready.push(callback.2);
                } else {
                    pending.push(callback);
                }
            }
            *callbacks = pending;
            ready
        };
        for callback in callbacks {
            callback();
        }
    }
}

impl MonotonicClock for ManualClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(
            self.monotonic.load(Ordering::SeqCst),
        ))
    }
}

impl RealtimeClock for ManualClock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::from_nanoseconds(self.realtime.load(Ordering::SeqCst)))
    }
}

impl TimerClockSource for ManualClock {
    fn realtime_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn schedule_callback(&self, deadline: u64, callback: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ClockError> {
        let token = self.wake_token.fetch_add(1, Ordering::SeqCst) + 1;
        self.scheduled.lock().unwrap().push((deadline, token));
        self.callbacks.lock().unwrap().push((deadline, token, callback));
        Ok(token)
    }
    fn cancel_wake(&self, token: u64) {
        self.canceled.lock().unwrap().push(token);
        self.callbacks.lock().unwrap().retain(|callback| callback.1 != token);
    }
}

struct TimerFixture;

impl TimerFixture {
    fn setting(value: u64, interval: u64) -> TimerSetting {
        TimerSetting {
            value: Duration::from_nanoseconds(value),
            interval: Duration::from_nanoseconds(interval),
        }
    }

    fn make(clock: Arc<ManualClock>, nonblocking: bool) -> TimerFd {
        let flags = if nonblocking {
            nonblocking_flags()
        } else {
            TimerFdCreateFlags::default()
        };
        TimerFd::new(TimerFdClock::Monotonic, flags, clock).unwrap()
    }

    fn read_count(timer: &TimerFd) -> Result<u64, TimerFdError> {
        let mut output = [0_u8; 8];
        timer.read(&mut output).map(|_| u64::from_ne_bytes(output))
    }
}

fn nonblocking_flags() -> TimerFdCreateFlags {
    TimerFdCreateFlags::from_bits(TimerFdCreateFlags::NONBLOCKING)
}

#[test]
fn expiry_notifies_epoll() {
    let clock = Arc::new(ManualClock::with_times(10, 100));
    let timer = Arc::new(TimerFixture::make(Arc::clone(&clock), true));
    let table = DescriptorTable::new(4).unwrap();
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            timer.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let epoll = Arc::new(crate::Epoll::new());
    epoll
        .add(
            table.pin(number).unwrap(),
            crate::EpollInterest::from_bits(crate::EpollInterest::READ),
            17,
        )
        .unwrap();
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(20, 0))
        .unwrap();
    let waiter = Arc::clone(&epoll);
    let thread = thread::spawn(move || waiter.wait(1, Some(HostDuration::from_secs(1))));
    thread::sleep(HostDuration::from_millis(10));
    clock.fire(30);
    let events = thread.join().unwrap().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, 17);
    assert!(events[0].readiness.contains(Readiness::READ));
}

#[test]
fn deadline_registration_replaced() {
    let clock = Arc::new(ManualClock::with_times(10, 100));
    let timer = TimerFixture::make(Arc::clone(&clock), true);
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(20, 0))
        .unwrap();
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(30, 0))
        .unwrap();
    assert_eq!(*clock.scheduled.lock().unwrap(), vec![(30, 1), (40, 2)]);
    assert_eq!(*clock.canceled.lock().unwrap(), vec![1]);
}

#[test]
fn prepared_read_commit() {
    let clock = Arc::new(ManualClock::default());
    let timer = TimerFixture::make(clock.clone(), true);
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(10, 10))
        .unwrap();
    clock.advance_monotonic(10);
    timer.notify_clock_changed();
    let prepared = timer.prepare_read().unwrap();
    assert_eq!(u64::from_ne_bytes(prepared.bytes()), 1);
    clock.advance_monotonic(20);
    timer.notify_clock_changed();
    timer.commit_read(prepared).unwrap();
    assert_eq!(TimerFixture::read_count(&timer), Ok(2));
}

#[test]
fn creation_validates_flags() {
    for id in [0, 1, 7, 8, 9] {
        assert!(TimerFdClock::from_linux_id(id).is_some());
    }
    assert_eq!(TimerFdClock::from_linux_id(2), None);
    let clock = Arc::new(ManualClock::default());
    assert_eq!(
        TimerFd::new(TimerFdClock::Monotonic, TimerFdCreateFlags::from_bits(4), clock,).unwrap_err(),
        TimerFdError::InvalidArgument
    );
    let flags = TimerFdCreateFlags::from_bits(TimerFdCreateFlags::NONBLOCKING | TimerFdCreateFlags::CLOSE_ON_EXEC);
    assert!(flags.closes_on_exec());
}

#[test]
fn relative_one_once() {
    let clock = Arc::new(ManualClock::with_times(100, 5_000));
    let timer = TimerFixture::make(clock.clone(), true);
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(10, 0))
        .unwrap();
    assert_eq!(timer.get_time().unwrap(), TimerFixture::setting(10, 0));
    assert_eq!(TimerFixture::read_count(&timer), Err(TimerFdError::WouldBlock));
    clock.advance_monotonic(10);
    timer.notify_clock_changed();
    assert!(
        timer
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    let mut output = [0xaa; 16];
    assert_eq!(timer.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output[..8].try_into().unwrap()), 1);
    assert_eq!(&output[8..], &[0xaa; 8]);
    assert_eq!(TimerFixture::read_count(&timer), Err(TimerFdError::WouldBlock));
}

#[test]
fn periodic_expirations_aligned() {
    let clock = Arc::new(ManualClock::default());
    let timer = TimerFixture::make(clock.clone(), true);
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(20, 100))
        .unwrap();
    clock.advance_monotonic(20);
    assert_eq!(TimerFixture::read_count(&timer), Ok(1));
    clock.advance_monotonic(350);
    assert_eq!(TimerFixture::read_count(&timer), Ok(3));
    assert_eq!(timer.get_time().unwrap(), TimerFixture::setting(50, 100));
}

#[test]
fn absolute_relative_linux() {
    let clock = Arc::new(ManualClock::with_times(100, 5_000));
    let monotonic = TimerFixture::make(clock.clone(), true);
    monotonic
        .set_time(
            TimerFdSetFlags::from_bits(TimerFdSetFlags::ABSOLUTE),
            TimerFixture::setting(150, 0),
        )
        .unwrap();
    clock.advance_monotonic(50);
    assert_eq!(TimerFixture::read_count(&monotonic), Ok(1));

    let realtime = TimerFd::new(TimerFdClock::Realtime, nonblocking_flags(), clock.clone()).unwrap();
    realtime
        .set_time(
            TimerFdSetFlags::from_bits(TimerFdSetFlags::ABSOLUTE),
            TimerFixture::setting(5_100, 0),
        )
        .unwrap();
    clock.advance_realtime(100);
    assert_eq!(TimerFixture::read_count(&realtime), Ok(1));

    let relative_realtime = TimerFd::new(TimerFdClock::Realtime, nonblocking_flags(), clock.clone()).unwrap();
    relative_realtime
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(10, 0))
        .unwrap();
    clock.advance_realtime(10_000);
    assert_eq!(
        TimerFixture::read_count(&relative_realtime),
        Err(TimerFdError::WouldBlock)
    );
    clock.advance_monotonic(10);
    assert_eq!(TimerFixture::read_count(&relative_realtime), Ok(1));
}

#[test]
fn realtime_discontinuity_timers() {
    let clock = Arc::new(ManualClock::with_times(0, 1_000));
    let timer = TimerFd::new(TimerFdClock::Realtime, nonblocking_flags(), clock.clone()).unwrap();
    let cancel_flags = TimerFdSetFlags::from_bits(TimerFdSetFlags::ABSOLUTE | TimerFdSetFlags::CANCEL_ON_SET);
    timer.set_time(cancel_flags, TimerFixture::setting(2_000, 20)).unwrap();
    clock.set_realtime(500);
    timer.notify_clock_changed();
    assert!(
        timer
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    assert_eq!(TimerFixture::read_count(&timer), Err(TimerFdError::Canceled));
    assert!(timer.snapshot().unwrap().canceled);

    timer.set_time(cancel_flags, TimerFixture::setting(1_000, 0)).unwrap();
    clock.advance_realtime(500);
    assert_eq!(TimerFixture::read_count(&timer), Ok(1));
}

#[test]
fn blocking_read_explicitly() {
    let clock = Arc::new(ManualClock::default());
    let timer = TimerFixture::make(clock.clone(), false);
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(10, 0))
        .unwrap();
    let reader = timer.clone();
    let started = Arc::new(Barrier::new(2));
    let reader_started = started.clone();
    let thread = thread::spawn(move || {
        reader_started.wait();
        TimerFixture::read_count(&reader)
    });
    started.wait();
    clock.advance_monotonic(10);
    timer.notify_clock_changed();
    assert_eq!(thread.join().unwrap(), Ok(1));

    let table = DescriptorTable::new(8).unwrap();
    let object = Arc::new(TimerFixture::make(Arc::new(ManualClock::default()), false));
    let number = table.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let waiter = object.clone();
    let thread = thread::spawn(move || TimerFixture::read_count(&waiter));
    thread::sleep(HostDuration::from_millis(10));
    table.close(number).unwrap();
    assert_eq!(thread.join().unwrap(), Err(TimerFdError::Retired));
}

#[test]
fn descriptor_lease_expirations() {
    let clock = Arc::new(ManualClock::default());
    let object = Arc::new(TimerFixture::make(clock.clone(), false));
    object
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(5, 0))
        .unwrap();
    let table = DescriptorTable::new(8).unwrap();
    let number = table.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let alias = table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let lease = table.pin(alias).unwrap();
    lease
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let mut output = [0_u8; 8];
    assert_eq!(lease.read(&mut output), Err(ObjectError::WouldBlock));
    clock.advance_monotonic(5);
    assert!(
        lease
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    assert_eq!(lease.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output), 1);
    assert_eq!(
        table.pin(number).unwrap().read(&mut output),
        Err(ObjectError::WouldBlock)
    );
}

#[test]
fn disarm_short_state() {
    let clock = Arc::new(ManualClock::with_times(50, 500));
    let timer = TimerFixture::make(clock, true);
    assert_eq!(
        timer.set_time(TimerFdSetFlags::from_bits(8), TimerFixture::setting(1, 0),),
        Err(TimerFdError::InvalidArgument)
    );
    timer
        .set_time(TimerFdSetFlags::default(), TimerFixture::setting(20, 4))
        .unwrap();
    let snapshot = timer.snapshot().unwrap();
    assert_eq!(snapshot.deadline_nanoseconds, Some(70));
    assert_eq!(snapshot.interval_nanoseconds, 4);
    let restored = TimerFd::from_snapshot(snapshot, Arc::new(ManualClock::with_times(50, 500))).unwrap();
    assert_eq!(restored.snapshot().unwrap(), snapshot);
    let mut short = [0_u8; 7];
    assert_eq!(timer.read(&mut short), Err(TimerFdError::InvalidArgument));
    assert_eq!(
        timer
            .set_time(TimerFdSetFlags::default(), TimerSetting::default())
            .unwrap(),
        TimerFixture::setting(20, 4)
    );
    assert_eq!(timer.get_time().unwrap(), TimerSetting::default());
    let status = timer.status();
    assert_eq!((status.mode, status.size, status.link_count), (0o100_600, 0, 1));
}
