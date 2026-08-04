use std::thread;
use std::time::Duration;

use hl_descriptor::{DescriptorFlags, DescriptorTable, Readiness, StatusFlags};

use crate::{Epoll, EpollInterest, Inotify, InotifyError, InotifyLimits, InotifyMask};

use super::test_support::Fixture;

#[test]
fn watch_flags_owned() {
    let fixture = Fixture::new(true);
    assert_eq!(
        fixture
            .inotify
            .add_watch(b"", InotifyMask::from_bits(InotifyMask::CREATE)),
        Err(InotifyError::InvalidArgument)
    );
    assert_eq!(
        fixture
            .inotify
            .add_watch(b"/file", InotifyMask::from_bits(InotifyMask::ONLY_DIRECTORY)),
        Err(InotifyError::InvalidArgument)
    );
    assert_eq!(
        fixture.inotify.add_watch(
            b"/file",
            InotifyMask::from_bits(InotifyMask::CREATE | InotifyMask::ONLY_DIRECTORY,),
        ),
        Err(InotifyError::NotDirectory)
    );
    assert!(
        fixture
            .inotify
            .add_watch(
                b"/dir",
                InotifyMask::from_bits(InotifyMask::CREATE | InotifyMask::ONLY_DIRECTORY | InotifyMask::DONT_FOLLOW,),
            )
            .is_ok()
    );
}

#[test]
fn same_node_rejects() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.watch(b"/file", InotifyMask::CREATE);
    let alias_descriptor = fixture.watch(b"/alias", InotifyMask::DELETE | InotifyMask::MASK_ADD);
    assert_eq!(alias_descriptor, descriptor);
    let token = fixture.source.token();
    let mask = fixture.source.mask(token);
    assert!(mask.contains(InotifyMask::CREATE));
    assert!(mask.contains(InotifyMask::DELETE));
    assert_eq!(
        fixture.inotify.add_watch(
            b"/file",
            InotifyMask::from_bits(InotifyMask::MODIFY | InotifyMask::MASK_CREATE,),
        ),
        Err(InotifyError::AlreadyExists)
    );
    assert_eq!(fixture.watch(b"/file", InotifyMask::MODIFY), descriptor);
    assert_eq!(fixture.source.mask(token).bits(), InotifyMask::MODIFY);
}

#[test]
fn records_ordered_name() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.watch(b"/dir", InotifyMask::MOVED_FROM | InotifyMask::MOVED_TO);
    let token = fixture.source.token();
    let cookie = fixture.inotify.next_rename_cookie().unwrap();
    fixture
        .source
        .emit(token, InotifyMask::MOVED_FROM, cookie, b"old", false);
    fixture.source.emit(
        token,
        InotifyMask::MOVED_TO | InotifyMask::IS_DIRECTORY,
        cookie,
        b"long-name",
        false,
    );
    let bytes = fixture.read_all();
    assert_eq!(Fixture::i32_at(&bytes, 0), descriptor);
    assert_eq!(Fixture::u32_at(&bytes, 8), cookie);
    assert_eq!(Fixture::u32_at(&bytes, 12), 4);
    assert_eq!(&bytes[16..19], b"old");
    let second = Fixture::record_size(&bytes, 0);
    assert_eq!(second, 20);
    assert_eq!(Fixture::i32_at(&bytes, second), descriptor);
    assert_eq!(Fixture::u32_at(&bytes, second + 8), cookie);
    assert_eq!(Fixture::u32_at(&bytes, second + 12), 12);
    assert_eq!(&bytes[second + 16..second + 25], b"long-name");
}

#[test]
fn partial_buffer_record() {
    let fixture = Fixture::new(true);
    fixture.watch(b"/dir", InotifyMask::CREATE);
    fixture
        .source
        .emit(fixture.source.token(), InotifyMask::CREATE, 0, b"child", false);
    let mut header = [0_u8; 16];
    assert_eq!(fixture.inotify.read(&mut header), Err(InotifyError::InvalidArgument));
    assert!(
        fixture
            .inotify
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    let bytes = fixture.read_all();
    assert_eq!(&bytes[16..21], b"child");
}

#[test]
fn oneshot_queues_token() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.watch(b"/file", InotifyMask::MODIFY | InotifyMask::ONESHOT);
    let stale = fixture.source.token();
    fixture.source.emit(stale, InotifyMask::MODIFY, 0, b"", false);
    let bytes = fixture.read_all();
    assert_eq!(Fixture::i32_at(&bytes, 0), descriptor);
    let second = Fixture::record_size(&bytes, 0);
    assert_eq!(Fixture::u32_at(&bytes, second + 4), InotifyMask::IGNORED);
    let reused = fixture.watch(b"/dir", InotifyMask::CREATE);
    assert_eq!(reused, descriptor);
    let fresh = fixture.source.token();
    assert_ne!(fresh, stale);
    fixture.source.emit(stale, InotifyMask::MODIFY, 0, b"", false);
    assert_eq!(fixture.inotify.read(&mut [0_u8; 32]), Err(InotifyError::WouldBlock));
}

#[test]
fn overflow_single_consumed() {
    let limits = InotifyLimits {
        watches: 2,
        queued_events: 3,
        queued_bytes: 64,
        name_bytes: 16,
    };
    let fixture = Fixture::with_limits(true, limits);
    fixture.watch(b"/dir", InotifyMask::CREATE);
    let token = fixture.source.token();
    for name in [b"a".as_slice(), b"b", b"c", b"d"] {
        fixture.source.emit(token, InotifyMask::CREATE, 0, name, false);
    }
    let bytes = fixture.read_all();
    let mut offset = 0;
    let mut overflows = 0;
    while offset < bytes.len() {
        if Fixture::u32_at(&bytes, offset + 4) & InotifyMask::QUEUE_OVERFLOW != 0 {
            overflows += 1;
            assert_eq!(Fixture::i32_at(&bytes, offset), -1);
        }
        offset += Fixture::record_size(&bytes, offset);
    }
    assert_eq!(overflows, 1);
}

#[test]
fn exclusion_filter_local() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.watch(b"/dir", InotifyMask::DELETE | InotifyMask::EXCLUDE_UNLINKED);
    let token = fixture.source.token();
    fixture.source.emit(token, InotifyMask::DELETE, 0, b"gone", true);
    assert_eq!(fixture.inotify.read(&mut [0_u8; 64]), Err(InotifyError::WouldBlock));
    fixture.source.emit(token, InotifyMask::UNMOUNT, 0, b"", false);
    let bytes = fixture.read_all();
    assert_eq!(Fixture::i32_at(&bytes, 0), descriptor);
    let second = Fixture::record_size(&bytes, 0);
    assert_eq!(Fixture::u32_at(&bytes, second + 4), InotifyMask::IGNORED);
}

#[test]
fn blocking_read_subscription() {
    let fixture = Fixture::new(false);
    fixture.watch(b"/dir", InotifyMask::CREATE);
    let table = DescriptorTable::new(4).unwrap();
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            fixture.inotify.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let epoll = Epoll::new();
    epoll
        .add(
            table.pin(number).unwrap(),
            EpollInterest::from_bits(EpollInterest::READ),
            77,
        )
        .unwrap();
    fixture
        .source
        .emit(fixture.source.token(), InotifyMask::CREATE, 0, b"x", false);
    assert!(
        epoll
            .wait(1, Some(Duration::from_millis(20)))
            .unwrap()
            .iter()
            .any(|event| event.data == 77)
    );
    let _ = fixture.read_all();
    let reader = fixture.inotify.clone();
    let thread = thread::spawn(move || {
        let mut output = [0_u8; 64];
        reader.read(&mut output)
    });
    thread::sleep(Duration::from_millis(10));
    fixture
        .source
        .emit(fixture.source.token(), InotifyMask::CREATE, 0, b"y", false);
    assert!(thread.join().unwrap().is_ok());
    table.close(number).unwrap();
    assert!(!fixture.source.removes().is_empty());
}

#[test]
fn close_cancels_read() {
    let fixture = Fixture::new(false);
    let reader = fixture.inotify.clone();
    let (started_send, started_receive) = std::sync::mpsc::channel();
    let (result_send, result_receive) = std::sync::mpsc::channel();
    let thread = thread::spawn(move || {
        started_send.send(()).unwrap();
        let result = reader.read(&mut [0_u8; 64]);
        result_send.send(result).unwrap();
    });
    started_receive.recv_timeout(Duration::from_secs(1)).unwrap();
    hl_descriptor::OpenFileDescription::close(fixture.inotify.as_ref());
    assert_eq!(
        result_receive.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(InotifyError::Retired),
    );
    thread.join().unwrap();
}

#[test]
fn snapshot_restores_queue() {
    let fixture = Fixture::new(true);
    fixture.watch(b"/dir", InotifyMask::CREATE | InotifyMask::MOVED_TO);
    let cookie = fixture.inotify.next_rename_cookie().unwrap();
    fixture
        .source
        .emit(fixture.source.token(), InotifyMask::MOVED_TO, cookie, b"child", false);
    let snapshot = fixture.inotify.snapshot();
    let restored = Inotify::from_snapshot(&snapshot, fixture.source.clone()).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
}
