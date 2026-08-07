use std::sync::Arc;
use std::thread;

use super::*;
use crate::protocol::{HEADER_SIZE, Header};
use crate::test_support::Endpoint;

struct Fixture;

impl Fixture {
    fn limits(in_flight: usize) -> ClientLimits {
        ClientLimits::new(64, in_flight).unwrap()
    }

    fn client(chunk: usize, in_flight: usize) -> (Arc<Provider<Endpoint>>, Endpoint) {
        let (client, server) = Endpoint::pair(chunk);
        (
            Arc::new(Provider::new(client, Self::limits(in_flight)).unwrap()),
            server,
        )
    }

    fn malformed_header(case: usize, request: u64) -> [u8; HEADER_SIZE] {
        let mut header = Header::encode(FrameKind::Reply, 0, request).unwrap();
        match case {
            0 => header[0] = 0,
            1 => header[6..8].copy_from_slice(&99_u16.to_le_bytes()),
            2 => header[8..12].copy_from_slice(&65_u32.to_le_bytes()),
            _ => unreachable!(),
        }
        header
    }
}

#[test]
fn c_header_layout() {
    let bytes = Header::encode(FrameKind::Request, 0x1122, 0x0102_0304_0506_0708).unwrap();
    assert_eq!(&bytes[0..4], &[0x52, 0x50, 0x4c, 0x48]);
    assert_eq!(&bytes[4..8], &[1, 0, 3, 0]);
    assert_eq!(&bytes[8..12], &[0x22, 0x11, 0, 0]);
    assert_eq!(&bytes[12..20], &[8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(&bytes[20..32], &[0; 12]);
}

#[test]
fn partial_byte_transport() {
    let (provider, server) = Fixture::client(1, 2);
    server.interrupt_reads(3);
    server.block_writes(3);
    let first = provider.begin(b"one").unwrap();
    let second = provider.begin(b"two").unwrap();
    let peer = thread::spawn(move || {
        let left = server.receive_frame();
        let right = server.receive_frame();
        assert_eq!((left.0, left.2.as_slice()), (FrameKind::Request, &b"one"[..]));
        assert_eq!((right.0, right.2.as_slice()), (FrameKind::Request, &b"two"[..]));
        server.send_frame(FrameKind::Reply, right.1, b"second");
        server.send_frame(FrameKind::Reply, left.1, b"first");
    });
    assert_eq!(provider.wait(first).unwrap().payload, b"first");
    assert_eq!(provider.wait(second).unwrap().payload, b"second");
    peer.join().unwrap();
    provider.close();
}

#[test]
fn checkpoint_freeze_rejects() {
    let (provider, server) = Fixture::client(1, 1);
    let ticket = provider.begin(b"pending").unwrap();
    let request = server.receive_frame();
    assert_eq!(provider.freeze_checkpoint(), Err(ProviderError::CheckpointBusy));
    server.send_frame(FrameKind::Reply, request.1, b"done");
    assert_eq!(provider.wait(ticket).unwrap().payload, b"done");
    provider.freeze_checkpoint().unwrap();
    let image = provider.checkpoint_client().unwrap();
    assert_eq!(image.request_generations, [1]);
    provider.thaw_checkpoint();
    provider.close();
}

#[test]
fn client_retries_interruption() {
    let (client, server) = Endpoint::pair(2);
    client.interrupt_reads(2);
    client.block_writes(2);
    let provider = Provider::new(client, Fixture::limits(1)).unwrap();
    let ticket = provider.begin(b"request").unwrap();
    let request = server.receive_frame();
    assert_eq!(request.2, b"request");
    server.send_frame(FrameKind::Reply, request.1, b"reply");
    assert_eq!(provider.wait(ticket).unwrap().payload, b"reply");
    provider.close();
}

#[test]
fn failed_send_releases() {
    let (client, _server) = Endpoint::pair(8);
    client.fail_writes(1);
    let provider = Provider::new(client, Fixture::limits(1)).unwrap();
    assert_eq!(
        provider.begin(b"failed"),
        Err(ProviderError::Transport(TransportError::Failed))
    );
    assert_eq!(provider.begin(b"cannot-reuse"), Err(ProviderError::Closed));
    provider.close();
}

#[test]
fn capacity_cancellation_late() {
    let (provider, server) = Fixture::client(4, 1);
    let first = provider.begin(b"cancel").unwrap();
    assert!(matches!(provider.begin(b"full"), Err(ProviderError::Capacity)));
    let request = server.receive_frame();
    provider.cancel(first).unwrap();
    assert_eq!(server.receive_frame().0, FrameKind::Cancel);
    server.send_frame(FrameKind::Reply, request.1, b"late");
    assert_eq!(provider.wait(first), Err(ProviderError::Canceled));

    let second = provider.begin(b"next").unwrap();
    let request = server.receive_frame();
    server.send_frame(FrameKind::Reply, request.1, b"ok");
    assert_eq!(provider.wait(second).unwrap().payload, b"ok");
    assert_eq!(
        provider.wait(first),
        Err(ProviderError::InvalidTicket(first.request().get()))
    );
    assert_eq!(provider.late_replies(), 1);
    provider.close();
}

#[test]
fn close_wakes_every() {
    let (provider, server) = Fixture::client(8, 2);
    let first = provider.begin(b"a").unwrap();
    let second = provider.begin(b"b").unwrap();
    server.receive_frame();
    server.receive_frame();
    let left = {
        let provider = Arc::clone(&provider);
        thread::spawn(move || provider.wait(first))
    };
    let right = {
        let provider = Arc::clone(&provider);
        thread::spawn(move || provider.wait(second))
    };
    provider.close();
    assert_eq!(left.join().unwrap(), Err(ProviderError::Closed));
    assert_eq!(right.join().unwrap(), Err(ProviderError::Closed));
}

#[test]
fn malformed_unknown_oversized() {
    for mutation in 0..4 {
        let (provider, server) = Fixture::client(32, 1);
        let ticket = provider.begin(b"pending").unwrap();
        let request = server.receive_frame();
        if mutation == 3 {
            server.shutdown();
        } else {
            let header = Fixture::malformed_header(mutation, request.1);
            server.write_all(&header);
        }
        assert!(provider.wait(ticket).is_err());
        assert!(provider.begin(b"after").is_err());
        provider.close();
    }
}

#[test]
fn reversed_reply_model() {
    const COUNT: usize = 32;
    let (provider, server) = Fixture::client(3, COUNT);
    let tickets: Vec<_> = (0..COUNT)
        .map(|index| provider.begin(&[index as u8]).unwrap())
        .collect();
    let peer = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..COUNT {
            requests.push(server.receive_frame());
        }
        for request in requests.into_iter().rev() {
            server.send_frame(FrameKind::Reply, request.1, &request.2);
        }
    });
    for (index, ticket) in tickets.into_iter().enumerate() {
        assert_eq!(provider.wait(ticket).unwrap().payload, [index as u8]);
        assert_eq!(
            provider.wait(ticket),
            Err(ProviderError::InvalidTicket(ticket.request().get()))
        );
    }
    peer.join().unwrap();
    provider.close();
}

#[test]
fn request_payload_and() {
    assert!(ClientLimits::new(0, 1).is_err());
    assert!(ClientLimits::new(1, 0).is_err());
    let (provider, _server) = Fixture::client(8, 1);
    assert_eq!(
        provider.begin(&[0; 65]),
        Err(ProviderError::PayloadTooLarge { size: 65, maximum: 64 })
    );
    provider.close();
}

#[test]
fn reply_preserves_payload() {
    let (provider, server) = Fixture::client(8, 1);
    let ticket = provider.begin(b"errno").unwrap();
    let request = server.receive_frame();
    let mut payload = vec![0xff];
    payload.extend_from_slice(&11_i32.to_le_bytes());
    payload.extend_from_slice(&[0, 0]);
    server.send_frame(FrameKind::Reply, request.1, &payload);
    let reply = provider.wait(ticket).unwrap();
    assert_eq!(reply.linux_errno, 11);
    assert_eq!(reply.payload, payload);
    provider.close();
}
