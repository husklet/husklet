use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use hl_descriptor::{
    CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError,
    OpenFileDescription, OperationCancellation, Readiness, ReadinessObserver, StatusFlags,
};

use crate::{
    SocketConnectError, SocketConnectStatus, SocketDescription, SocketHostError, SocketHostIo, SocketHostReadiness,
};

#[derive(Debug, Default)]
struct FakeSocketHost {
    reads: Mutex<VecDeque<Result<usize, SocketHostError>>>,
    writes: Mutex<VecDeque<Result<usize, SocketHostError>>>,
    nonblocking: Mutex<Vec<bool>>,
    closed: AtomicUsize,
    canceled: AtomicUsize,
    connects: Mutex<VecDeque<SocketConnectStatus>>,
    closed_tokens: Mutex<Vec<u64>>,
}

impl SocketHostIo for FakeSocketHost {
    type Token = u64;

    fn read(&self, _token: u64, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        self.nonblocking.lock().unwrap().push(nonblocking);
        let result = self.reads.lock().unwrap().pop_front().unwrap_or(Ok(0));
        if let Ok(length) = result {
            output[..length].fill(0x5a);
        }
        result
    }

    fn write(&self, _token: u64, _input: &[u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        self.nonblocking.lock().unwrap().push(nonblocking);
        self.writes.lock().unwrap().pop_front().unwrap_or(Ok(0))
    }

    fn readiness(&self, _token: u64) -> SocketHostReadiness {
        SocketHostReadiness {
            readable: true,
            priority: false,
            read_hangup: false,
            writable: true,
            error: false,
            hangup: false,
        }
    }

    fn start_connect(&self, _token: u64, _nonblocking: bool) -> SocketConnectStatus {
        self.connects
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(SocketConnectStatus::Connected)
    }

    fn poll_connect(&self, _token: u64) -> SocketConnectStatus {
        self.connects
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(SocketConnectStatus::Pending)
    }

    fn close(&self, token: u64) {
        self.closed_tokens.lock().unwrap().push(token);
        self.closed.fetch_add(1, Ordering::AcqRel);
    }

    fn cancel(&self, _token: u64) {
        self.canceled.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct Observer(AtomicUsize);

impl ReadinessObserver for Observer {
    fn readiness_changed(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

struct ConnectSubscription;

impl CancellationSubscription for ConnectSubscription {}

struct ConnectCancellation;

impl OperationCancellation for ConnectCancellation {
    fn interrupted(&self) -> bool {
        false
    }

    fn subscribe(&self, _: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        Box::new(ConnectSubscription)
    }
}

#[derive(Debug, Default)]
struct BlockingHost {
    canceled: Mutex<bool>,
    wake: Condvar,
    closed: AtomicUsize,
    entered: AtomicUsize,
}

impl SocketHostIo for BlockingHost {
    type Token = u64;

    fn read(&self, _token: u64, _output: &mut [u8], _nonblocking: bool) -> Result<usize, SocketHostError> {
        self.entered.fetch_add(1, Ordering::Release);
        let mut canceled = self.canceled.lock().unwrap();
        while !*canceled {
            canceled = self.wake.wait(canceled).unwrap();
        }
        Err(SocketHostError::Canceled)
    }

    fn write(&self, token: u64, _input: &[u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        self.read(token, &mut [], nonblocking)
    }

    fn readiness(&self, _token: u64) -> SocketHostReadiness {
        SocketHostReadiness::default()
    }

    fn start_connect(&self, _token: u64, _nonblocking: bool) -> SocketConnectStatus {
        SocketConnectStatus::Pending
    }

    fn poll_connect(&self, _token: u64) -> SocketConnectStatus {
        SocketConnectStatus::Pending
    }

    fn cancel(&self, _token: u64) {
        *self.canceled.lock().unwrap() = true;
        self.wake.notify_all();
    }

    fn close(&self, _token: u64) {
        self.closed.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn partial_io_and() {
    let host = Arc::new(FakeSocketHost::default());
    host.reads.lock().unwrap().extend([
        Ok(3),
        Err(SocketHostError::WouldBlock),
        Err(SocketHostError::Interrupted),
        Err(SocketHostError::Canceled),
    ]);
    host.writes
        .lock()
        .unwrap()
        .extend([Ok(2), Err(SocketHostError::BrokenPipe)]);
    let socket = SocketDescription::new(Arc::clone(&host), 7, StatusFlags::default());
    let mut output = [0; 8];
    assert_eq!(socket.read(&mut output), Ok(3));
    assert_eq!(&output[..3], &[0x5a; 3]);
    assert_eq!(socket.read(&mut output), Err(ObjectError::WouldBlock));
    assert_eq!(socket.read(&mut output), Err(ObjectError::Interrupted));
    assert_eq!(socket.read(&mut output), Err(ObjectError::Canceled));
    assert_eq!(socket.write(b"hello"), Ok(2));
    assert_eq!(socket.write(b"hello"), Err(ObjectError::BrokenPipe));
}

#[test]
fn duplicated_descriptors_share() {
    let host = Arc::new(FakeSocketHost::default());
    let socket = Arc::new(SocketDescription::new(Arc::clone(&host), 9, StatusFlags::default()));
    let table = DescriptorTable::new(32).unwrap();
    let fd = table.install(0, socket, DescriptorFlags::default()).unwrap();
    let alias = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    table
        .pin(fd)
        .unwrap()
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    assert_eq!(table.pin(alias).unwrap().object().write(b"x"), Ok(0));
    assert_eq!(*host.nonblocking.lock().unwrap(), vec![true]);
    table.close(fd).unwrap();
    assert_eq!(host.closed.load(Ordering::Acquire), 0);
    table.close(alias).unwrap();
    assert_eq!(host.closed.load(Ordering::Acquire), 1);
    assert_eq!(host.canceled.load(Ordering::Acquire), 1);
}

#[test]
fn readiness_subscription_quiesces() {
    let host = Arc::new(FakeSocketHost::default());
    let socket = SocketDescription::new(host, 11, StatusFlags::default());
    let observer = Arc::new(Observer::default());
    let subscription = socket.subscribe_readiness(observer.clone()).unwrap();
    socket.notify_readiness();
    assert_eq!(observer.0.load(Ordering::Acquire), 1);
    subscription.quiesce();
    socket.notify_readiness();
    assert_eq!(observer.0.load(Ordering::Acquire), 1);
    assert!(socket.readiness(Readiness::default()).contains(Readiness::READ));
}

#[test]
fn concurrent_close_method() {
    let host = Arc::new(FakeSocketHost::default());
    let socket = Arc::new(SocketDescription::new(Arc::clone(&host), 13, StatusFlags::default()));
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let socket = Arc::clone(&socket);
            std::thread::spawn(move || socket.close())
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(host.closed.load(Ordering::Acquire), 1);
}

#[test]
fn retirement_wakes_a() {
    let host = Arc::new(BlockingHost::default());
    let socket = Arc::new(SocketDescription::new(Arc::clone(&host), 17, StatusFlags::default()));
    let reader = {
        let socket = Arc::clone(&socket);
        std::thread::spawn(move || {
            let mut output = [0; 1];
            socket.read(&mut output)
        })
    };
    while host.entered.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    socket.retire();
    assert_eq!(reader.join().unwrap(), Err(ObjectError::Canceled));
    socket.close();
    assert_eq!(host.closed.load(Ordering::Acquire), 1);
}

#[test]
fn duplicated_aliases_share() {
    let host = Arc::new(FakeSocketHost::default());
    host.connects.lock().unwrap().extend([
        SocketConnectStatus::Pending,
        SocketConnectStatus::Failed(SocketConnectError::Refused),
    ]);
    let socket = Arc::new(SocketDescription::new(
        host,
        19,
        StatusFlags::from_bits(StatusFlags::NONBLOCKING),
    ));
    let table = DescriptorTable::new(8).unwrap();
    let fd = table.install(0, socket.clone(), DescriptorFlags::default()).unwrap();
    let alias = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    assert_eq!(socket.connect(), Err(SocketConnectError::InProgress));
    assert_eq!(socket.connect(), Err(SocketConnectError::Already));
    assert_eq!(socket.poll_connect(), Err(SocketConnectError::Refused));
    assert_eq!(socket.take_connect_error(), Some(SocketConnectError::Refused));
    assert_eq!(socket.take_connect_error(), None);
    table.close(fd).unwrap();
    table.close(alias).unwrap();
}

#[test]
fn connect_success_timeout() {
    let host = Arc::new(FakeSocketHost::default());
    host.connects.lock().unwrap().extend([
        SocketConnectStatus::Pending,
        SocketConnectStatus::Failed(SocketConnectError::TimedOut),
        SocketConnectStatus::Connected,
    ]);
    let socket = SocketDescription::new(host, 21, StatusFlags::from_bits(StatusFlags::NONBLOCKING));
    assert_eq!(socket.connect(), Err(SocketConnectError::InProgress));
    assert_eq!(socket.poll_connect(), Err(SocketConnectError::TimedOut));
    assert_eq!(socket.connect(), Err(SocketConnectError::TimedOut));
    assert_eq!(socket.take_connect_error(), Some(SocketConnectError::TimedOut));
    assert_eq!(socket.connect(), Ok(()));
    assert_eq!(socket.connect(), Err(SocketConnectError::Connected));
    socket.retire();
    assert_eq!(socket.poll_connect(), Err(SocketConnectError::Canceled));
}

#[test]
fn blocking_connect_waits() {
    let host = Arc::new(FakeSocketHost::default());
    host.connects
        .lock()
        .unwrap()
        .extend([SocketConnectStatus::Pending, SocketConnectStatus::Connected]);
    let socket = SocketDescription::new(host, 22, StatusFlags::default());
    assert_eq!(socket.connect_with_cancellation(&ConnectCancellation), Ok(()),);
    assert_eq!(socket.connect(), Err(SocketConnectError::Connected));
}

#[test]
fn accept_queue_is() {
    let host = Arc::new(FakeSocketHost::default());
    let listener = SocketDescription::new(Arc::clone(&host), 30, StatusFlags::default());
    listener.listen(2);
    let address = crate::SocketAddress::Inet4 {
        address: [127, 0, 0, 1],
        port: 80,
    };
    assert!(matches!(
        listener.accept(true, false),
        Err(crate::AcceptError::WouldBlock)
    ));
    listener.publish_accepted(31, address.clone(), address.clone()).unwrap();
    listener.publish_accepted(32, address.clone(), address.clone()).unwrap();
    assert_eq!(
        listener.publish_accepted(33, address.clone(), address.clone()),
        Err(crate::AcceptError::Backpressure)
    );
    assert_eq!(*host.closed_tokens.lock().unwrap(), vec![33]);
    assert!(listener.readiness(Readiness::default()).contains(Readiness::READ));
    let first = listener.accept(true, true).unwrap();
    let second = listener.accept(false, false).unwrap();
    assert!(first.descriptor_flags.closes_on_exec());
    assert!(!second.descriptor_flags.closes_on_exec());
    first.description.close();
    second.description.close();
    assert_eq!(*host.closed_tokens.lock().unwrap(), vec![33, 31, 32]);
}

#[test]
fn listener_retirement_drains() {
    let host = Arc::new(FakeSocketHost::default());
    let listener = Arc::new(SocketDescription::new(Arc::clone(&host), 40, StatusFlags::default()));
    listener.listen(2);
    let address = crate::SocketAddress::Inet6 {
        address: [0; 16],
        port: 9,
        scope: 0,
    };
    listener.publish_accepted(41, address.clone(), address).unwrap();
    listener.retire();
    assert!(matches!(
        listener.accept(false, false),
        Err(crate::AcceptError::Canceled)
    ));
    assert!(host.closed_tokens.lock().unwrap().contains(&41));
}

#[test]
fn sixteen_acceptors_follow() {
    let host = Arc::new(FakeSocketHost::default());
    let listener = Arc::new(SocketDescription::new(Arc::clone(&host), 50, StatusFlags::default()));
    listener.listen(16);
    let mut workers = Vec::new();
    for expected in 0..16_u16 {
        let worker_listener = Arc::clone(&listener);
        workers.push(std::thread::spawn(move || {
            let accepted = worker_listener.accept(false, false).unwrap();
            let crate::SocketAddress::Inet4 { port, .. } = accepted.local else {
                panic!("expected IPv4");
            };
            accepted.description.close();
            (expected, port)
        }));
        while listener.accept_waiting() != u64::from(expected + 1) {
            std::thread::yield_now();
        }
        listener.notify_accept_spurious();
    }
    for port in 0..16_u16 {
        let address = crate::SocketAddress::Inet4 {
            address: [127, 0, 0, 1],
            port,
        };
        listener
            .publish_accepted(100 + u64::from(port), address.clone(), address)
            .unwrap();
    }
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results, (0..16_u16).map(|port| (port, port)).collect::<Vec<_>>());
}

#[test]
fn retirement_racing_enqueue() {
    let host = Arc::new(FakeSocketHost::default());
    for token in 1_000..1_100_u64 {
        let listener = Arc::new(SocketDescription::new(
            Arc::clone(&host),
            token + 10_000,
            StatusFlags::default(),
        ));
        listener.listen(1);
        let publisher = {
            let listener = Arc::clone(&listener);
            std::thread::spawn(move || {
                let address = crate::SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: token as u16,
                };
                let _ = listener.publish_accepted(token, address.clone(), address);
            })
        };
        listener.retire();
        publisher.join().unwrap();
    }
    let closed = host.closed_tokens.lock().unwrap();
    for token in 1_000..1_100_u64 {
        assert_eq!(closed.iter().filter(|closed| **closed == token).count(), 1);
    }
}
