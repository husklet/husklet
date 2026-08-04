use std::sync::{Arc, Barrier};
use std::thread;

use hl_descriptor::{DescriptorError, DescriptorFlags, ExactDuplicate, OpenFileDescription, StatusFlags};
use hl_event::{Epoll, EpollError, EpollInterest, EventCatalog};
use hl_event::{SignalFdFlags, SignalMask};
use hl_ipc::Pipe;
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use crate::{event::CatalogBoundEvent, Control, ControlError, GraphError, RuntimeDescriptorTable, TaskSignalQueue};

struct ControlFixture {
    control: Arc<Control>,
    table: Arc<RuntimeDescriptorTable>,
}

impl ControlFixture {
    fn new() -> Self {
        let (control, table) = Control::new(64, 64).unwrap();
        Self {
            control: Arc::new(control),
            table: Arc::new(table),
        }
    }

    fn epoll(&self, close_on_exec: bool) -> i32 {
        let flags = if close_on_exec {
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
        } else {
            DescriptorFlags::default()
        };
        self.control.create_epoll(&self.table, flags).unwrap()
    }

    fn interest() -> EpollInterest {
        EpollInterest::from_bits(EpollInterest::READ)
    }

    fn add(&self, source: i32, target: i32) {
        self.control
            .add(&self.table, source, target, Self::interest(), 1)
            .unwrap();
    }
}

#[test]
fn add_coordinated_graph() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(false);
    fixture.add(source, target);
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
    fixture
        .control
        .modify(&fixture.table, source, target, ControlFixture::interest(), 9)
        .unwrap();
    fixture.control.delete(&fixture.table, source, target).unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn failed_graph_reference() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(false);
    fixture.add(source, target);
    assert_eq!(
        fixture
            .control
            .add(&fixture.table, source, target, ControlFixture::interest(), 2,),
        Err(ControlError::Graph(GraphError::Event(EpollError::AlreadyExists,)))
    );
    assert_eq!(fixture.control.graph_snapshot().edges[0].watches, 1);
}

#[test]
fn target_graph_retirement() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(false);
    fixture.add(source, target);
    let target_alias = fixture
        .control
        .duplicate(&fixture.table, target, 0, DescriptorFlags::default())
        .unwrap();
    fixture.control.close(&fixture.table, target).unwrap();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
    fixture.control.close(&fixture.table, target_alias).unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());

    let second_target = fixture.epoll(false);
    fixture.add(source, second_target);
    let source_alias = fixture
        .control
        .duplicate(&fixture.table, source, 0, DescriptorFlags::default())
        .unwrap();
    fixture.control.close(&fixture.table, source).unwrap();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
    fixture.control.close(&fixture.table, source_alias).unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn transferred_epoll_retains_pipe() {
    let fixture = ControlFixture::new();
    let child = fixture.control.fork(&fixture.table);
    let pipe = Pipe::new(false);
    let reader = child
        .descriptor_table()
        .install(
            0,
            pipe.reader.clone() as Arc<dyn OpenFileDescription>,
            DescriptorFlags::default(),
        )
        .unwrap();
    let writer = child
        .descriptor_table()
        .install(
            0,
            pipe.writer.clone() as Arc<dyn OpenFileDescription>,
            DescriptorFlags::default(),
        )
        .unwrap();
    // Production event syscalls install this lifecycle wrapper rather than the
    // raw epoll object. Transfer ownership must survive that boundary.
    let object = Arc::new(Epoll::new());
    let bound = Arc::new(CatalogBoundEvent::new(
        object.clone(),
        Arc::new(EventCatalog::new(8).unwrap()),
    ));
    let child_descriptors = child.descriptor_table();
    let prepared = child_descriptors
        .prepare_open(
            0,
            bound.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let identity = prepared.description_identity();
    fixture.control.register_epoll(identity, object);
    bound.bind_epoll(fixture.control.clone(), identity);
    let epoll = prepared.publish();
    fixture
        .control
        .add(&child, epoll, reader, ControlFixture::interest(), 0x5151)
        .unwrap();
    let queued = child.descriptor_table().export_description(epoll).unwrap();
    let received = fixture
        .table
        .descriptor_table()
        .install_description(0, &queued, DescriptorFlags::default())
        .unwrap();
    drop(queued);

    assert_eq!(child.descriptor_table().pin(writer).unwrap().write(b"z").unwrap(), 1);
    fixture.control.close(&child, reader).unwrap();
    fixture.control.close(&child, writer).unwrap();
    fixture.control.close(&child, epoll).unwrap();
    assert_eq!(
        fixture
            .control
            .wait(&fixture.table, received, 1, Some(std::time::Duration::ZERO))
            .unwrap()[0]
            .data,
        0x5151,
    );
    fixture.control.close(&fixture.table, received).unwrap();
}

#[test]
fn forked_last_close() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(false);
    fixture.add(source, target);
    let child = fixture.control.fork(&fixture.table);
    fixture.control.close(&fixture.table, target).unwrap();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
    fixture.control.close(&child, target).unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn exec_final_sources() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(true);
    let target = fixture.epoll(true);
    fixture.add(source, target);
    let target_alias = fixture
        .control
        .duplicate(&fixture.table, target, 0, DescriptorFlags::default())
        .unwrap();
    let closed = fixture.control.exec_sweep(&fixture.table);
    assert!(closed.contains(&source));
    assert!(closed.contains(&target));
    assert!(fixture.control.graph_snapshot().edges.is_empty());
    assert!(fixture.control.snapshot(&fixture.table, target_alias).is_ok());
}

#[test]
fn duplicate_final_ofd() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let watched = fixture.epoll(false);
    let replacement = fixture.epoll(false);
    fixture.add(source, watched);
    fixture
        .control
        .duplicate_exact(&fixture.table, replacement, watched, ExactDuplicate::Dup2)
        .unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn range_lifecycle() {
    let fixture = ControlFixture::new();
    let outside = fixture.epoll(false);
    let first = fixture.epoll(false);
    let second = fixture.epoll(false);
    fixture.add(first, second);

    fixture
        .control
        .close_range(&fixture.table, first as u32, second as u32, true)
        .unwrap();
    assert!(
        fixture
            .control
            .snapshot(&fixture.table, first)
            .unwrap()
            .flags
            .closes_on_exec()
    );
    assert!(
        !fixture
            .control
            .snapshot(&fixture.table, outside)
            .unwrap()
            .flags
            .closes_on_exec()
    );

    fixture
        .control
        .close_range(&fixture.table, first as u32, second as u32, false)
        .unwrap();
    assert_eq!(
        fixture.control.snapshot(&fixture.table, first),
        Err(ControlError::Descriptor(DescriptorError::BadDescriptor)),
    );
    assert_eq!(
        fixture.control.snapshot(&fixture.table, second),
        Err(ControlError::Descriptor(DescriptorError::BadDescriptor)),
    );
    assert!(fixture.control.snapshot(&fixture.table, outside).is_ok());
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn range_unshares_caller() {
    let fixture = ControlFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(false);
    fixture.add(source, target);
    let sibling = fixture.control.share(&fixture.table);

    let caller = fixture
        .control
        .unshare_range(&sibling, target as u32, target as u32, false)
        .unwrap();
    assert_eq!(
        fixture.control.snapshot(&caller, target),
        Err(ControlError::Descriptor(DescriptorError::BadDescriptor)),
    );
    assert!(fixture.control.snapshot(&fixture.table, target).is_ok());
    assert!(fixture.control.snapshot(&sibling, target).is_ok());
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);

    let source_parent = fixture.control.snapshot(&fixture.table, source).unwrap();
    let source_caller = fixture.control.snapshot(&caller, source).unwrap();
    assert_eq!(source_parent.description_identity, source_caller.description_identity,);
    caller.descriptor_table().set_offset(source, 73).unwrap();
    assert_eq!(fixture.control.snapshot(&fixture.table, source).unwrap().offset, 73,);

    let private_flags = fixture
        .control
        .unshare_range(&sibling, source as u32, source as u32, true)
        .unwrap();
    assert!(
        fixture
            .control
            .snapshot(&private_flags, source)
            .unwrap()
            .flags
            .closes_on_exec()
    );
    assert!(
        !fixture
            .control
            .snapshot(&fixture.table, source)
            .unwrap()
            .flags
            .closes_on_exec()
    );
    drop(private_flags);

    assert!(matches!(
        fixture.control.unshare_range(&sibling, 9, 8, false),
        Err(ControlError::Descriptor(DescriptorError::InvalidArgument)),
    ));
    assert!(fixture.control.snapshot(&fixture.table, target).is_ok());
    fixture.control.close(&fixture.table, target).unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn close_terminal_state() {
    for _ in 0..128 {
        let fixture = ControlFixture::new();
        let source = fixture.epoll(false);
        let target = fixture.epoll(false);
        let barrier = Arc::new(Barrier::new(3));
        let adder = {
            let control = Arc::clone(&fixture.control);
            let table = Arc::clone(&fixture.table);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                control.add(&table, source, target, ControlFixture::interest(), 1)
            })
        };
        let closer = {
            let control = Arc::clone(&fixture.control);
            let table = Arc::clone(&fixture.table);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                control.close(&table, target)
            })
        };
        barrier.wait();
        let add = adder.join().unwrap();
        let close = closer.join().unwrap();
        assert!(close.is_ok());
        assert!(matches!(
            add,
            Ok(_) | Err(ControlError::Descriptor(DescriptorError::BadDescriptor))
        ));
        assert!(fixture.control.graph_snapshot().edges.is_empty());
    }
}

#[test]
fn signalfd_close_exec() {
    let fixture = ControlFixture::new();
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            max_threads: 2,
            max_groups: 2,
            max_pending_signals: 2,
            online_cpus: 1,
        })
        .unwrap(),
    );
    let (_, thread_id) = tasks
        .create_init(ProcessCredentials::new(1, 1, &[], 2).unwrap(), ProcessLimits::empty())
        .unwrap();
    let queue = Arc::new(TaskSignalQueue::new(tasks, thread_id));
    let number = fixture
        .control
        .create_signalfd(
            &fixture.table,
            SignalMask::from_bits(1_u64 << 9),
            SignalFdFlags::from_bits(SignalFdFlags::NONBLOCKING | SignalFdFlags::CLOSE_ON_EXEC),
            queue,
        )
        .unwrap();
    assert!(
        fixture
            .control
            .snapshot(&fixture.table, number)
            .unwrap()
            .flags
            .closes_on_exec()
    );
}

#[test]
fn candidate_routing() {
    let fixture = ControlFixture::new();
    let descriptor = fixture.epoll(true);
    let source = fixture.table.descriptor_table();
    let candidate = Arc::new(source.fork());
    candidate.close_on_exec();
    let image = fixture.control.exec_image(&fixture.table, candidate.clone());

    assert!(source.pin(descriptor).is_ok());
    assert!(candidate.pin(descriptor).is_err());
    assert!(image.descriptor_table().pin(descriptor).is_err());
    assert!(fixture.table.descriptor_table().pin(descriptor).is_ok());
}
