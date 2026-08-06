use std::io::{IoSlice, IoSliceMut};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use hl_descriptor::{
    DescriptorFlags, DescriptorTable, ObjectError, ObjectKind, OpenFileDescription, OperationContext, Readiness,
    StatusFlags,
};

use crate::{NamedFifo, NamedFifoCatalog, NamedFifoKey, NamedFifoOpen, NamedFifoOpenError, PIPE_BUF, Pipe};

#[test]
fn named_fifo_rendezvous_in_both_orders() {
    let fifo = NamedFifo::new(41);
    let reader = match fifo.open_reader(false) {
        NamedFifoOpen::Waiting(wait) => wait,
        NamedFifoOpen::Ready(_) => panic!("reader must wait without a writer"),
    };
    assert!(!reader.ready());
    let writer = match fifo.open_writer(false).unwrap() {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("writer must match waiting reader"),
    };
    assert!(reader.ready());
    let reader = reader.complete().unwrap();
    assert_eq!(writer.write(b"fifo"), Ok(4));
    let mut bytes = [0; 4];
    assert_eq!(reader.read(&mut bytes), Ok(4));
    assert_eq!(&bytes, b"fifo");
    reader.close();
    writer.close();

    let writer = match fifo.open_writer(false).unwrap() {
        NamedFifoOpen::Waiting(wait) => wait,
        NamedFifoOpen::Ready(_) => panic!("writer must wait without a reader"),
    };
    let reader = match fifo.open_reader(false) {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("reader must match waiting writer"),
    };
    assert!(writer.ready());
    writer.complete().unwrap().close();
    reader.close();
}

#[test]
fn named_fifo_nonblocking_and_unlink_lifetime() {
    let fifo = NamedFifo::new(73);
    assert!(matches!(fifo.open_writer(true), Err(NamedFifoOpenError::NoReader)));
    let reader = match fifo.open_reader(true) {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("nonblocking reader cannot wait"),
    };
    let writer = match fifo.open_writer(true).unwrap() {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("matched nonblocking writer cannot wait"),
    };
    fifo.unlink();
    assert!(!fifo.reclaimable());
    assert_eq!(fifo.status().identity, 73);
    reader.close();
    assert!(!fifo.reclaimable());
    writer.close();
    assert!(fifo.reclaimable());
}

#[test]
fn named_fifo_waiters_block_checkpoint_and_rollback_counts() {
    let fifo = NamedFifo::new(9);
    let wait = match fifo.open_reader(false) {
        NamedFifoOpen::Waiting(wait) => wait,
        NamedFifoOpen::Ready(_) => panic!("reader must wait"),
    };
    assert_eq!(fifo.snapshot(), Err(crate::PipeCreateError::Busy));
    drop(wait);
    let snapshot = fifo.snapshot().unwrap();
    assert_eq!(snapshot.identity, 9);
    assert_eq!(snapshot.pipe.readers, 0);
    assert_eq!(snapshot.pipe.writers, 0);
    assert!(snapshot.validate().is_ok());
}

#[test]
fn named_fifo_catalog_preserves_inode_identity_and_unlinked_opens() {
    let catalog = NamedFifoCatalog::new();
    let key = NamedFifoKey { device: 3, inode: 7 };
    let first = catalog.open(key);
    let reopened = catalog.open(key);
    assert!(Arc::ptr_eq(&first, &reopened));
    catalog.unlink(key, false);
    assert!(first.status().linked);
    assert!(Arc::ptr_eq(&first, &catalog.open(key)));
    let reader = match first.open_reader(true) {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("nonblocking reader cannot wait"),
    };
    catalog.unlink(key, true);
    assert!(!first.status().linked);
    assert!(!first.reclaimable());
    let replacement = catalog.open(key);
    assert_ne!(first.status().identity, replacement.status().identity);
    reader.close();
    assert!(first.reclaimable());
}

#[test]
fn named_fifo_wait_lane_wakes_on_peer_open() {
    let fifo = Arc::new(NamedFifo::new(101));
    let wait = match fifo.open_writer(false).unwrap() {
        NamedFifoOpen::Waiting(wait) => wait,
        NamedFifoOpen::Ready(_) => panic!("writer must wait without reader"),
    };
    let worker = thread::spawn(move || wait.wait());
    let reader = match fifo.open_reader(false) {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("reader must match waiting writer"),
    };
    let writer = worker.join().unwrap();
    assert_eq!(fifo.status().waiting, 0);
    reader.close();
    writer.close();
}

#[test]
fn named_fifo_readwrite_open_publishes_both_sides_atomically() {
    let fifo = NamedFifo::new(102);
    let (reader, writer) = fifo.open_readwrite(false);
    assert_eq!(fifo.status().readers, 1);
    assert_eq!(fifo.status().writers, 1);
    let peer = match fifo.open_writer(false).unwrap() {
        NamedFifoOpen::Ready(endpoint) => endpoint,
        NamedFifoOpen::Waiting(_) => panic!("duplex reader must admit a writer"),
    };
    assert_eq!(peer.write(b"duplex"), Ok(6));
    let mut output = [0; 6];
    assert_eq!(reader.read(&mut output), Ok(6));
    assert_eq!(&output, b"duplex");
    peer.close();
    reader.close();
    writer.close();
    assert_eq!(fifo.status().readers, 0);
    assert_eq!(fifo.status().writers, 0);
}

#[test]
fn named_fifo_catalog_keeps_linked_fifo_alive_between_opens() {
    let catalog = NamedFifoCatalog::new();
    let key = NamedFifoKey { device: 4, inode: 5 };
    let identity = catalog.open(key).status().identity;
    assert_eq!(catalog.open(key).status().identity, identity);
}

struct InstalledPipe {
    table: DescriptorTable,
    read_fd: i32,
    write_fd: i32,
}

fn install_pipe(capacity: usize, nonblocking: bool) -> InstalledPipe {
    let pipe = Pipe::with_capacity(capacity, nonblocking).unwrap();
    let table = DescriptorTable::new(32).unwrap();
    let nonblocking_flag = if nonblocking { StatusFlags::NONBLOCKING } else { 0 };
    let read_fd = table
        .commit(
            table.reserve(0).unwrap(),
            pipe.reader,
            StatusFlags::from_bits(nonblocking_flag),
            DescriptorFlags::default(),
        )
        .unwrap();
    let write_fd = table
        .commit(
            table.reserve(0).unwrap(),
            pipe.writer,
            StatusFlags::from_bits(1 | nonblocking_flag),
            DescriptorFlags::default(),
        )
        .unwrap();
    InstalledPipe {
        table,
        read_fd,
        write_fd,
    }
}

#[test]
fn operations_run_through() {
    let pipe = install_pipe(PIPE_BUF * 2, false);
    assert_eq!(pipe.table.snapshot(pipe.read_fd).unwrap().kind, ObjectKind::Pipe);
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    assert_eq!(writer.write(b"alive"), Ok(5));
    let mut output = [0_u8; 8];
    assert_eq!(reader.read(&mut output), Ok(5));
    assert_eq!(&output[..5], b"alive");
}

#[test]
fn vector_pipe_transfer() {
    let pipe = install_pipe(PIPE_BUF, false);
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let input = [IoSlice::new(b"ab"), IoSlice::new(b"cdef")];
    let context = OperationContext {
        actor: None,
        cancellation: None,
    };
    assert_eq!(writer.write_vector_context(&input, context), Ok(6));
    let mut left = [0; 1];
    let mut right = [0; 5];
    let mut output = [IoSliceMut::new(&mut left), IoSliceMut::new(&mut right)];
    assert_eq!(reader.read_vector_context(&mut output, context), Ok(6));
    assert_eq!(&left, b"a");
    assert_eq!(&right, b"bcdef");
}

#[test]
fn wrong_endpoint_operations() {
    let pipe = install_pipe(PIPE_BUF, true);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let mut output = [0_u8; 1];
    assert_eq!(reader.write(b"x"), Err(ObjectError::BadDescriptor));
    assert_eq!(writer.read(&mut output), Err(ObjectError::BadDescriptor));
}

#[test]
fn nonblocking_empty_read() {
    let pipe = install_pipe(PIPE_BUF, true);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let mut output = [0_u8; 1];
    assert_eq!(reader.read(&mut output), Err(ObjectError::WouldBlock));
    assert_eq!(writer.write(&vec![1; PIPE_BUF]), Ok(PIPE_BUF));
    assert_eq!(writer.write(b"x"), Err(ObjectError::WouldBlock));
}

#[test]
fn pipe_buf_writes() {
    let pipe = install_pipe(PIPE_BUF * 2, true);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    assert_eq!(writer.write(&vec![1; PIPE_BUF + 1]), Ok(PIPE_BUF + 1));
    assert_eq!(writer.write(&vec![2; PIPE_BUF]), Err(ObjectError::WouldBlock));
    assert_eq!(writer.write(&vec![3; PIPE_BUF + 1]), Ok(PIPE_BUF - 1));
    let mut output = vec![0; PIPE_BUF + 1];
    assert_eq!(reader.read(&mut output), Ok(PIPE_BUF + 1));
    assert!(output.iter().all(|byte| *byte == 1));
}

#[test]
fn a_large_nonblocking() {
    let pipe = install_pipe(PIPE_BUF * 2, true);
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    assert_eq!(writer.write(&vec![1; PIPE_BUF]), Ok(PIPE_BUF));
    assert_eq!(writer.write(&vec![2; PIPE_BUF * 2]), Ok(PIPE_BUF));
}

#[test]
fn duplicate_keeps_the() {
    let pipe = install_pipe(PIPE_BUF, false);
    let duplicate = pipe
        .table
        .duplicate(pipe.write_fd, 0, DescriptorFlags::default())
        .unwrap();
    pipe.table.close(pipe.write_fd).unwrap();
    let writer = pipe.table.pin(duplicate).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    assert_eq!(writer.write(b"d"), Ok(1));
    let mut output = [0_u8; 1];
    assert_eq!(reader.read(&mut output), Ok(1));
    assert_eq!(output[0], b'd');
    drop(writer);
    pipe.table.close(duplicate).unwrap();
    assert_eq!(reader.read(&mut output), Ok(0));
}

#[test]
fn buffered_data_and() {
    let pipe = install_pipe(PIPE_BUF, false);
    pipe.table.pin(pipe.write_fd).unwrap().write(b"ab").unwrap();
    pipe.table.close(pipe.write_fd).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let interests = Readiness::from_bits(Readiness::READ);
    let ready = reader.readiness(interests);
    assert!(ready.contains(Readiness::READ));
    assert!(ready.contains(Readiness::HANGUP));
    let mut output = [0_u8; 8];
    assert_eq!(reader.read(&mut output), Ok(2));
    assert_eq!(&output[..2], b"ab");
    assert_eq!(reader.read(&mut output), Ok(0));
    assert_eq!(reader.readiness(interests).bits(), Readiness::HANGUP);
}

#[test]
fn forked_writer_retirement_publishes_eof_with_stale_lease() {
    let pipe = install_pipe(PIPE_BUF, false);
    let child = pipe.table.fork();
    let retained = child.pin(pipe.write_fd).unwrap();
    pipe.table.close(pipe.write_fd).unwrap();
    child.close(pipe.write_fd).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    assert_eq!(
        reader.readiness(Readiness::from_bits(Readiness::READ)).bits(),
        Readiness::HANGUP
    );
    let mut output = [0_u8; 1];
    assert_eq!(reader.read(&mut output), Ok(0));
    drop(retained);
}

#[test]
fn closing_readers_produces() {
    let pipe = install_pipe(PIPE_BUF, false);
    pipe.table.close(pipe.read_fd).unwrap();
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    assert_eq!(writer.write(b"x"), Err(ObjectError::BrokenPipe));
    assert_eq!(
        writer.readiness(Readiness::from_bits(Readiness::WRITE)).bits(),
        Readiness::ERROR
    );
}

#[test]
fn fragmented_capacity_does_not_publish_write_readiness() {
    let pipe = install_pipe(PIPE_BUF * 2, false);
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    writer.write(&vec![1; PIPE_BUF]).unwrap();
    let mut byte = [0_u8; 1];
    reader.read(&mut byte).unwrap();
    writer.write(b"x").unwrap();

    assert_eq!(writer.readiness(Readiness::from_bits(Readiness::WRITE)).bits(), 0);

    reader.read(&mut vec![0; PIPE_BUF]).unwrap();
    assert_eq!(
        writer.readiness(Readiness::from_bits(Readiness::WRITE)).bits(),
        Readiness::WRITE,
    );
}

#[test]
fn blocking_reader_wakes() {
    let pipe = install_pipe(PIPE_BUF, false);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    let started = Arc::new(Barrier::new(2));
    let thread_started = started.clone();
    let thread = thread::spawn(move || {
        thread_started.wait();
        let mut output = [0_u8; 1];
        reader.read(&mut output).map(|count| (count, output[0]))
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    writer.write(b"w").unwrap();
    assert_eq!(thread.join().unwrap(), Ok((1, b'w')));
}

#[test]
fn multiple_pipe_sleepers_balance_after_wake() {
    let pipe = Pipe::new(false);
    let first = pipe.reader.clone();
    let second = pipe.reader.clone();
    let first_wait = std::thread::spawn(move || {
        let mut byte = [0_u8; 1];
        first.read(&mut byte)
    });
    let second_wait = std::thread::spawn(move || {
        let mut byte = [0_u8; 1];
        second.read(&mut byte)
    });
    for _ in 0..10_000 {
        if pipe.reader.sleeper_count() == 2 {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(pipe.reader.sleeper_count(), 2);
    assert_eq!(pipe.writer.write(b"ab"), Ok(2));
    assert_eq!(first_wait.join().unwrap(), Ok(1));
    assert_eq!(second_wait.join().unwrap(), Ok(1));
    assert_eq!(pipe.reader.sleeper_count(), 0);
}

#[test]
fn closing_the_last() {
    let pipe = install_pipe(PIPE_BUF, false);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let started = Arc::new(Barrier::new(2));
    let thread_started = started.clone();
    let thread = thread::spawn(move || {
        thread_started.wait();
        let mut output = [0_u8; 1];
        reader.read(&mut output)
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    pipe.table.close(pipe.write_fd).unwrap();
    assert_eq!(thread.join().unwrap(), Ok(0));
}

#[test]
fn closing_the_epipe() {
    let pipe = install_pipe(PIPE_BUF, false);
    let writer = pipe.table.pin(pipe.write_fd).unwrap();
    writer.write(&vec![1; PIPE_BUF]).unwrap();
    let started = Arc::new(Barrier::new(2));
    let thread_started = started.clone();
    let thread = thread::spawn(move || {
        thread_started.wait();
        writer.write(b"x")
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    pipe.table.close(pipe.read_fd).unwrap();
    assert_eq!(thread.join().unwrap(), Err(ObjectError::BrokenPipe));
}

#[test]
fn retiring_a_blocked() {
    let pipe = install_pipe(PIPE_BUF, false);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    let started = Arc::new(Barrier::new(2));
    let thread_started = started.clone();
    let thread = thread::spawn(move || {
        thread_started.wait();
        let mut output = [0_u8; 1];
        reader.read(&mut output)
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    pipe.table.close(pipe.read_fd).unwrap();
    assert_eq!(thread.join().unwrap(), Err(ObjectError::Retired));
}

#[test]
fn set_status_flags() {
    let pipe = install_pipe(PIPE_BUF, false);
    let reader = pipe.table.pin(pipe.read_fd).unwrap();
    reader
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let mut output = [0_u8; 1];
    assert_eq!(reader.read(&mut output), Err(ObjectError::WouldBlock));
}

#[test]
fn fstat_shape_is() {
    let pipe = Pipe::new(false);
    let status = pipe.reader.status();
    assert_eq!(status.mode, 0o010_600);
    assert_eq!(status.size, 0);
    assert_eq!(status.link_count, 1);
}

#[test]
fn capacity_resize_rounds() {
    let pipe = Pipe::with_capacity(PIPE_BUF * 2, true).unwrap();
    assert_eq!(pipe.writer.resize_capacity(PIPE_BUF + 1), Ok(PIPE_BUF * 2));
    pipe.writer.write(&vec![1; PIPE_BUF + 1]).unwrap();
    assert_eq!(pipe.reader.resize_capacity(PIPE_BUF), Err(ObjectError::Busy));
    assert_eq!(pipe.reader.capacity(), PIPE_BUF * 2);
}

#[test]
fn capacity_resize_preserves() {
    let pipe = install_pipe(PIPE_BUF, true);
    let duplicate = pipe
        .table
        .duplicate(pipe.write_fd, 0, DescriptorFlags::default())
        .unwrap();
    assert_eq!(
        pipe.table.pin(duplicate).unwrap().set_pipe_capacity(PIPE_BUF + 1),
        Ok(PIPE_BUF * 2)
    );
    assert_eq!(pipe.table.pin(pipe.read_fd).unwrap().pipe_capacity(), Ok(PIPE_BUF * 2));
}

#[test]
fn packet_pipe_preserves() {
    let pipe = Pipe::new_packet(true);
    pipe.writer.write(b"abc").unwrap();
    pipe.writer.write(b"12345").unwrap();
    let mut first = [0_u8; 2];
    assert_eq!(pipe.reader.read(&mut first), Ok(2));
    assert_eq!(&first, b"ab");
    let mut second = [0_u8; 8];
    assert_eq!(pipe.reader.read(&mut second), Ok(5));
    assert_eq!(&second[..5], b"12345");
}

#[test]
fn pipe_write_atomicity_capability() {
    use hl_descriptor::OpenFileDescription;

    let stream = Pipe::new(false);
    let packet = Pipe::new_packet(false);
    assert_eq!(stream.writer.atomic_write_limit(), Some(PIPE_BUF));
    assert_eq!(packet.writer.atomic_write_limit(), Some(PIPE_BUF));
}

#[test]
fn packet_mode_and() {
    let pipe = Pipe::new_packet(true);
    pipe.writer.write(b"one").unwrap();
    pipe.writer.write(b"two").unwrap();
    let restored = Pipe::restore(&pipe.snapshot().unwrap()).unwrap();
    let mut output = [0_u8; 8];
    assert_eq!(restored.reader.read(&mut output), Ok(3));
    assert_eq!(&output[..3], b"one");
    assert_eq!(restored.reader.read(&mut output), Ok(3));
    assert_eq!(&output[..3], b"two");
}

#[test]
fn large_nonblocking_packet() {
    let pipe = Pipe::with_capacity(PIPE_BUF * 2, true).unwrap();
    let packet = Pipe::new_packet(true);
    assert_eq!(packet.writer.resize_capacity(PIPE_BUF * 2), Ok(PIPE_BUF * 2));
    assert_eq!(packet.writer.write(&vec![7; PIPE_BUF + 3]), Ok(PIPE_BUF));
    let mut output = vec![0; PIPE_BUF * 2];
    assert_eq!(packet.reader.read(&mut output), Ok(PIPE_BUF));
    assert!(output[..PIPE_BUF].iter().all(|byte| *byte == 7));
    assert_eq!(pipe.reader.capacity(), PIPE_BUF * 2);
}
