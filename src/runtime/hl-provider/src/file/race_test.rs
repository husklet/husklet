use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use hl_descriptor::{DescriptorFlags, DescriptorTable, OpenFileDescription};

use super::*;
use crate::test_support::Endpoint;

struct RaceFixture;

impl RaceFixture {
    fn client() -> (Arc<Provider<Endpoint>>, ProjectedFiles<Endpoint>, Endpoint) {
        let (client, server) = Endpoint::pair(2);
        let provider = Arc::new(Provider::new(client, ClientLimits::new(256, 8).unwrap()).unwrap());
        let files = ProjectedFiles::new(Arc::clone(&provider), 8, 3).unwrap();
        (provider, files, server)
    }

    fn open_reply(server: &Endpoint, remote: u64) {
        let request = server.receive_frame();
        assert_eq!(request.2[0], 1);
        let mut reply = vec![1];
        reply.extend_from_slice(&remote.to_le_bytes());
        server.send_frame(FrameKind::Reply, request.1, &reply);
    }

    fn close_reply(server: &Endpoint, remote: u64) {
        let request = server.receive_frame();
        assert_eq!(request.2[0], 7);
        assert_eq!(u64::from_le_bytes(request.2[1..9].try_into().unwrap()), remote);
        server.send_frame(FrameKind::Reply, request.1, &[7]);
    }

    #[allow(clippy::needless_pass_by_value)]
    fn read_reply(server: &Endpoint, request: (FrameKind, u64, Vec<u8>), byte: u8) {
        assert_eq!(request.2[0], 2);
        let reply = [2, 1, 0, 0, 0, byte];
        server.send_frame(FrameKind::Reply, request.1, &reply);
    }
}

#[test]
fn reads_complete_against() {
    let (provider, files, server) = RaceFixture::client();
    let peer = thread::spawn(move || {
        RaceFixture::open_reply(&server, 51);
        let first = server.receive_frame();
        let second = server.receive_frame();
        let first_offset = u64::from_le_bytes(first.2[9..17].try_into().unwrap());
        let second_offset = u64::from_le_bytes(second.2[9..17].try_into().unwrap());
        RaceFixture::read_reply(&server, second, second_offset as u8);
        RaceFixture::read_reply(&server, first, first_offset as u8);
        RaceFixture::close_reply(&server, 51);
    });
    let file = files.open_service(1, FileAccess::Read, b"/out-of-order").unwrap();
    let left = {
        let file = Arc::clone(&file);
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            file.read_at(7, &mut byte).map(|_| byte[0])
        })
    };
    let right = {
        let file = Arc::clone(&file);
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            file.read_at(9, &mut byte).map(|_| byte[0])
        })
    };
    let mut results = [left.join().unwrap().unwrap(), right.join().unwrap().unwrap()];
    results.sort_unstable();
    assert_eq!(results, [7, 9]);
    drop(file);
    peer.join().unwrap();
    provider.close();
}

#[test]
fn close_read_race() {
    let (provider, files, server) = RaceFixture::client();
    let (read_seen_tx, read_seen_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let peer = thread::spawn(move || {
        RaceFixture::open_reply(&server, 61);
        let read = server.receive_frame();
        read_seen_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        RaceFixture::read_reply(&server, read, b'x');
        RaceFixture::close_reply(&server, 61);
        RaceFixture::open_reply(&server, 62);
        RaceFixture::close_reply(&server, 62);
    });

    let old = files.open_service(1, FileAccess::Read, b"/old").unwrap();
    let table = Arc::new(DescriptorTable::new(1).unwrap());
    let object: Arc<dyn OpenFileDescription> = old.clone();
    let descriptor = table.install(0, object, DescriptorFlags::default()).unwrap();
    let lease = table.pin(descriptor).unwrap();
    let old_generation = lease.descriptor_generation();
    let reader = thread::spawn(move || {
        let mut byte = [0_u8; 1];
        (lease.read(&mut byte), byte)
    });
    read_seen_rx.recv().unwrap();
    table.close(descriptor).unwrap();
    assert!(matches!(old.read_at(0, &mut [0_u8; 1]), Err(FileError::Retired)));
    release_tx.send(()).unwrap();
    assert_eq!(reader.join().unwrap(), (Ok(1), [b'x']));
    drop(old);

    let replacement = files.open_service(2, FileAccess::Read, b"/replacement").unwrap();
    let replacement_object: Arc<dyn OpenFileDescription> = replacement.clone();
    assert_eq!(
        table
            .install(0, replacement_object, DescriptorFlags::default())
            .unwrap(),
        descriptor
    );
    assert!(table.pin(descriptor).unwrap().descriptor_generation() > old_generation);
    table.close(descriptor).unwrap();
    drop(replacement);
    drop(table);
    peer.join().unwrap();
    provider.close();
}

#[test]
fn access_mode_rejects() {
    let (provider, files, server) = RaceFixture::client();
    let peer = thread::spawn(move || {
        RaceFixture::open_reply(&server, 71);
        RaceFixture::close_reply(&server, 71);
        RaceFixture::open_reply(&server, 72);
        RaceFixture::close_reply(&server, 72);
    });
    let read_only = files.open_service(1, FileAccess::Read, b"/read").unwrap();
    assert_eq!(read_only.write_at(0, b"x"), Err(FileError::Linux(9)));
    drop(read_only);
    let write_only = files.open_service(2, FileAccess::Write, b"/write").unwrap();
    assert_eq!(write_only.read_at(0, &mut [0_u8; 1]), Err(FileError::Linux(9)));
    drop(write_only);
    peer.join().unwrap();
    provider.close();
}
