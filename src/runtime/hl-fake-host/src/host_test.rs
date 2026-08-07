use crate::*;
use hl_network::SocketHostIo;
use hl_provider::{ProviderTransport, TransportError};
use hl_time::{Deadline, MonotonicClock};
use std::sync::Arc;
use std::thread;

#[test]
fn transcript_and_failure() {
    let host = FakeHost::new(7);
    let storage = StorageAdapter::new(host.clone(), 3);
    let file = storage.create(b"abcdef".to_vec()).unwrap();
    let mut output = [0; 6];
    assert_eq!(storage.read(file, 0, &mut output).unwrap(), 3);
    host.fail_at(3, Fault::Interrupted);
    assert_eq!(
        storage.read(file, 0, &mut output),
        Err(FakeHostError::Fault {
            fault: Fault::Interrupted,
            capability: "file",
            operation: "read",
            resource: file.0,
        })
    );
    assert_eq!(storage.read(file, 0, &mut output).unwrap(), 3);
    assert_eq!(
        host.transcript()
            .iter()
            .map(|call| (call.sequence, call.operation, call.completed, call.fault))
            .collect::<Vec<_>>(),
        [
            (1, "open", 0, None),
            (2, "read", 3, None),
            (3, "read", 0, Some(Fault::Interrupted)),
            (4, "read", 3, None),
        ]
    );
}

#[test]
fn virtual_clock_advances() {
    let host = FakeHost::new(1);
    let clock = VirtualClock::new(host, 10, 20);
    assert_eq!(clock.monotonic_now().unwrap().nanoseconds(), 10);
    clock.sleep_until(Deadline::from_nanoseconds(100)).unwrap();
    assert_eq!(clock.monotonic_now().unwrap().nanoseconds(), 100);
}

#[test]
fn provider_transport_preserves() {
    let host = FakeHost::new(2);
    let endpoint = ProviderEndpoint::new(host.clone(), 2).unwrap();
    let mut output = [0; 4];
    assert_eq!(endpoint.read(&mut output), Err(TransportError::WouldBlock));
    assert_eq!(endpoint.write(b"abcd").unwrap(), 2);
    host.fail_at(host.transcript().len() + 1, Fault::Interrupted);
    assert_eq!(endpoint.read(&mut output), Err(TransportError::Interrupted));
    assert_eq!(endpoint.read(&mut output).unwrap(), 2);
    assert_eq!(&output[..2], b"ab");
}

#[test]
fn resource_accounting_detects() {
    let host = FakeHost::new(3);
    let storage = StorageAdapter::new(host.clone(), 8);
    let file = storage.create(Vec::new()).unwrap();
    let directory = storage.create_directory(vec![b"z".to_vec(), b"a".to_vec()]).unwrap();
    assert_eq!(
        storage.directory_snapshot(directory).unwrap(),
        [b"a".to_vec(), b"z".to_vec()]
    );
    let sockets = SocketAdapter::new(host.clone(), 8);
    let socket = sockets.open().unwrap();
    assert_eq!(host.resources().get(ResourceKind::File), 1);
    assert_eq!(host.resources().get(ResourceKind::Socket), 1);
    storage.close(file).unwrap();
    storage.close_directory(directory).unwrap();
    sockets.close(socket);
    assert!(host.resources().is_empty());
}

#[test]
fn barriers_and_hosts() {
    let first = FakeHost::new(11);
    let second = FakeHost::new(12);
    let waiter = {
        let first = first.clone();
        thread::spawn(move || first.wait_barrier("run"))
    };
    second.release_barrier("run");
    assert!(first.transcript().is_empty());
    first.release_barrier("run");
    waiter.join().unwrap();
    assert_eq!(first.identity(), 11);
    assert_eq!(second.identity(), 12);
}

#[test]
fn socket_endpoint_has() {
    let host = FakeHost::new(4);
    let sockets = Arc::new(SocketAdapter::new(host, 2));
    let token = sockets.open().unwrap();
    assert!(sockets.readiness(token).writable);
    assert_eq!(sockets.write(token, b"xyz", true).unwrap(), 2);
    assert!(sockets.readiness(token).readable);
    let mut output = [0; 4];
    assert_eq!(sockets.read(token, &mut output, true).unwrap(), 2);
    assert_eq!(&output[..2], b"xy");
}

#[test]
fn process_and_differential() {
    let host = FakeHost::new(5);
    let processes = ProcessAdapter::new(host.clone());
    let process = processes.spawn(ProcessExit::Code(3)).unwrap();
    processes.terminate(process, 9).unwrap();
    assert_eq!(processes.wait(process).unwrap(), ProcessExit::Signal(9));
    processes.close(process).unwrap();
    assert_eq!(
        host.differential_transcript()
            .iter()
            .map(|row| row.split('\t').nth(1).unwrap())
            .collect::<Vec<_>>(),
        ["process", "process", "process", "process"]
    );
    assert!(host.resources().is_empty());
}
