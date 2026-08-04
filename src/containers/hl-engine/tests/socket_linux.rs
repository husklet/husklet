#![cfg(target_os = "linux")]

use hl_engine::native_host::{
    EventInterest, GenerationToken, HostError, LinuxHost, NativeSocket, PollSet, ShutdownDirection, SocketAddress,
    SocketDomain, SocketOption, SocketType,
};
use std::sync::Arc;

fn wait_for(poll: &PollSet<LinuxHost>, expected: GenerationToken) {
    let events = poll.wait(1_000, 8).unwrap();
    assert!(events.iter().any(|event| event.token == expected));
}

#[test]
fn ipv4_nonblocking_connect() {
    let host = Arc::new(LinuxHost);
    let listener = NativeSocket::create(Arc::clone(&host), SocketDomain::Ipv4, SocketType::Stream).unwrap();
    listener.set_option(SocketOption::ReuseAddress, true).unwrap();
    listener.bind(&SocketAddress::Ipv4Loopback(0)).unwrap();
    listener.listen(8).unwrap();
    let address = listener.local_address().unwrap();

    let poll = PollSet::create(Arc::clone(&host)).unwrap();
    let listener_token = GenerationToken::new(1, 1).unwrap();
    poll.add(&listener, EventInterest::READABLE, listener_token).unwrap();

    let client = NativeSocket::create(Arc::clone(&host), SocketDomain::Ipv4, SocketType::Stream).unwrap();
    client.set_option(SocketOption::NoDelay, true).unwrap();
    let client_token = GenerationToken::new(2, 1).unwrap();
    poll.add(&client, EventInterest::WRITABLE, client_token).unwrap();
    let connect = client.connect(&address);
    assert!(
        matches!(connect, Ok(()) | Err(HostError::WouldBlock)),
        "unexpected connect result: {connect:?}"
    );
    wait_for(&poll, client_token);
    assert_eq!(client.pending_error().unwrap(), None);
    wait_for(&poll, listener_token);
    let accepted = listener.accept().unwrap();

    assert_eq!(client.send(b"socket").unwrap(), 6);
    let accepted_token = GenerationToken::new(3, 1).unwrap();
    poll.add(&accepted, EventInterest::READABLE, accepted_token).unwrap();
    wait_for(&poll, accepted_token);
    let mut bytes = [0; 16];
    assert_eq!(accepted.receive(&mut bytes).unwrap(), 6);
    assert_eq!(&bytes[..6], b"socket");
    client.shutdown(ShutdownDirection::Write).unwrap();
    wait_for(&poll, accepted_token);
    assert_eq!(accepted.receive(&mut bytes).unwrap(), 0);
}

#[test]
fn unix_abstract_socket() {
    let host = Arc::new(LinuxHost);
    let poll = PollSet::create(Arc::clone(&host)).unwrap();
    let name = format!("hl-engine-{}-{:?}", std::process::id(), std::thread::current().id());
    let address = SocketAddress::unix_abstract(name.as_bytes()).unwrap();
    let listener = NativeSocket::create(Arc::clone(&host), SocketDomain::Unix, SocketType::Stream).unwrap();
    listener.bind(&address).unwrap();
    listener.listen(2).unwrap();
    let stale = GenerationToken::new(7, 1).unwrap();
    poll.add(&listener, EventInterest::READABLE, stale).unwrap();
    drop(listener);

    let replacement = NativeSocket::create(Arc::clone(&host), SocketDomain::Unix, SocketType::Stream).unwrap();
    let fresh = GenerationToken::new(7, 2).unwrap();
    poll.add(&replacement, EventInterest::WRITABLE, fresh).unwrap();
    let events = poll.wait(100, 8).unwrap();
    assert!(events.iter().all(|event| event.token != stale));
    assert!(events.iter().any(|event| event.token == fresh));
}
