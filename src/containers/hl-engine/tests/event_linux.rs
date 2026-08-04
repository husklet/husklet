#![cfg(target_os = "linux")]

use hl_engine::native_host::{
    EventCounter, EventInterest, EventMode, FileWatch, GenerationToken, HostError, LinuxHost, NativeSignal,
    NativeSignalMask, NativeSignalSource, NativeTimer, PollSet, ThreadSignalMask, TimerSetting, WatchInterest,
};
use std::sync::Arc;

#[test]
fn eventfd_epoll_edge() {
    let host = Arc::new(LinuxHost);
    let counter = EventCounter::create(Arc::clone(&host), 0, EventMode::Counter).unwrap();
    let poll = PollSet::create(Arc::clone(&host)).unwrap();
    let token = GenerationToken::new(9, 17).unwrap();
    poll.add(
        &counter,
        EventInterest::READABLE
            .union(EventInterest::EDGE)
            .union(EventInterest::ONESHOT),
        token,
    )
    .unwrap();
    assert!(poll.wait(0, 4).unwrap().is_empty());
    counter.write(3).unwrap();
    let ready = poll.wait(1000, 4).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].token, token);
    assert!(ready[0].readable);
    assert!(poll.wait(0, 4).unwrap().is_empty());
    assert_eq!(counter.read(), Ok(3));
    assert_eq!(counter.read(), Err(HostError::WouldBlock));
}

#[test]
fn inotify_reports_created() {
    let host = Arc::new(LinuxHost);
    let directory = std::env::temp_dir().join(format!("hl-watch-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
    let watch = FileWatch::create(host).unwrap();
    let token = watch.add(&path, WatchInterest::CREATE).unwrap();
    assert_eq!(watch.read(), Err(HostError::WouldBlock));
    std::fs::write(directory.join("created"), b"x").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let events = watch.read().unwrap();
    assert!(events.iter().any(|event| event.name == b"created"));
    watch.remove(token).unwrap();
    std::fs::remove_file(directory.join("created")).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn timerfd_is_nonblocking() {
    let host = Arc::new(LinuxHost);
    let timer = NativeTimer::create(host).unwrap();
    assert_eq!(timer.read_expirations(), Err(HostError::WouldBlock));
    timer
        .set(TimerSetting {
            initial_ns: 1_000_000,
            interval_ns: 0,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match timer.read_expirations() {
            Ok(count) => {
                assert!(count >= 1);
                break;
            }
            Err(HostError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::yield_now();
            }
            result => panic!("timer did not expire: {result:?}"),
        }
    }
}

#[test]
fn eventfd_semaphore_reads() {
    let host = Arc::new(LinuxHost);
    let counter = EventCounter::create(Arc::clone(&host), 2, EventMode::Semaphore).unwrap();
    let poll = PollSet::create(Arc::clone(&host)).unwrap();
    let token = GenerationToken::new(1, 1).unwrap();
    poll.add(&counter, EventInterest::READABLE, token).unwrap();
    assert_eq!(counter.read(), Ok(1));
    poll.remove(&counter).unwrap();
    assert!(poll.wait(0, 1).unwrap().is_empty());
    assert_eq!(counter.read(), Ok(1));
}

#[test]
fn signalfd_is_nonblocking() {
    let host = Arc::new(LinuxHost);
    let signal = NativeSignal::new(10).unwrap();
    let mask = NativeSignalMask::default().with(signal);
    {
        let blocked = ThreadSignalMask::block(Arc::clone(&host), mask).unwrap();
        assert!(!blocked.was_blocked(signal));
        let source = NativeSignalSource::create(Arc::clone(&host), mask).unwrap();
        assert_eq!(source.read(), Err(HostError::WouldBlock));
        blocked.raise(signal).unwrap();
        let info = source.read().unwrap();
        assert_eq!(info.signal, signal);
        assert_eq!(info.process_id, std::process::id());
        blocked.restore().unwrap();
    }

    let blocked_again = ThreadSignalMask::block(Arc::clone(&host), mask).unwrap();
    assert!(!blocked_again.was_blocked(signal));
    blocked_again.raise(signal).unwrap();
    let source = NativeSignalSource::create(host, mask).unwrap();
    assert_eq!(source.read().unwrap().signal, signal);
}
