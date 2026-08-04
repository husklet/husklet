use std::sync::Arc;
use std::thread;

use hl_descriptor::{DescriptorFlags, DescriptorTable, OperationLease, StatusFlags};
use hl_event::{Epoll, EpollInterest};

use crate::{GraphError, OwnershipGraph};

struct Fixture {
    table: Arc<DescriptorTable>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            table: Arc::new(DescriptorTable::new(64).unwrap()),
        }
    }

    fn epoll(&self) -> (i32, Arc<Epoll>) {
        let epoll = Arc::new(Epoll::new());
        let number = self
            .table
            .commit(
                self.table.reserve(0).unwrap(),
                epoll.clone(),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        (number, epoll)
    }

    fn lease(&self, number: i32) -> OperationLease {
        self.table.pin(number).unwrap()
    }

    fn interest() -> EpollInterest {
        EpollInterest::from_bits(EpollInterest::READ)
    }
}

#[test]
fn self_cycle_loop() {
    let fixture = Fixture::new();
    let graph = OwnershipGraph::new(8).unwrap();
    let (a_number, a) = fixture.epoll();
    let (b_number, b) = fixture.epoll();
    let (c_number, c) = fixture.epoll();
    assert_eq!(
        graph.add(
            &fixture.lease(a_number),
            &a,
            fixture.lease(a_number),
            Fixture::interest(),
            0,
        ),
        Err(GraphError::InvalidArgument)
    );
    graph
        .add(
            &fixture.lease(a_number),
            &a,
            fixture.lease(b_number),
            Fixture::interest(),
            1,
        )
        .unwrap();
    graph
        .add(
            &fixture.lease(b_number),
            &b,
            fixture.lease(c_number),
            Fixture::interest(),
            2,
        )
        .unwrap();
    assert_eq!(
        graph.add(
            &fixture.lease(c_number),
            &c,
            fixture.lease(a_number),
            Fixture::interest(),
            3,
        ),
        Err(GraphError::Loop)
    );
    assert_eq!(graph.snapshot().edges.len(), 2);
}

#[test]
fn delete_nested_edges() {
    let fixture = Fixture::new();
    let graph = OwnershipGraph::new(8).unwrap();
    let (a_number, a) = fixture.epoll();
    let (b_number, _) = fixture.epoll();
    let source = fixture.lease(a_number);
    let target = fixture.lease(b_number);
    graph.add(&source, &a, target.clone(), Fixture::interest(), 1).unwrap();
    graph.delete(&source, &a, &target).unwrap();
    assert!(graph.snapshot().edges.is_empty());

    graph.add(&source, &a, target.clone(), Fixture::interest(), 2).unwrap();
    graph.close(target.description_identity());
    assert!(graph.snapshot().edges.is_empty());
}

#[test]
fn descriptor_graph_identity() {
    let fixture = Fixture::new();
    let graph = OwnershipGraph::new(8).unwrap();
    let (source_number, source_epoll) = fixture.epoll();
    let (target_number, _) = fixture.epoll();
    let source = fixture.lease(source_number);
    let target = fixture.lease(target_number);
    let retired = target.description_identity();
    graph
        .add(&source, &source_epoll, target, Fixture::interest(), 1)
        .unwrap();
    graph.close(retired);
    fixture.table.close(target_number).unwrap();
    let (replacement_number, _) = fixture.epoll();
    let replacement = fixture.lease(replacement_number).description_identity();
    assert_ne!(retired, replacement);
    assert!(graph.snapshot().edges.is_empty());
}

#[test]
fn concurrent_acyclic_snapshot() {
    let fixture = Fixture::new();
    let graph = Arc::new(OwnershipGraph::new(32).unwrap());
    let (source_number, source_epoll) = fixture.epoll();
    let targets = (0..16).map(|_| fixture.epoll().0).collect::<Vec<_>>();
    let workers = targets
        .into_iter()
        .enumerate()
        .map(|(data, target)| {
            let graph = Arc::clone(&graph);
            let table = Arc::clone(&fixture.table);
            let epoll = Arc::clone(&source_epoll);
            thread::spawn(move || {
                graph
                    .add(
                        &table.pin(source_number).unwrap(),
                        &epoll,
                        table.pin(target).unwrap(),
                        Fixture::interest(),
                        data as u64,
                    )
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let snapshot = graph.snapshot();
    assert_eq!(snapshot.edges.len(), 16);
    assert!(snapshot.edges.iter().all(|edge| edge.watches == 1));
}

#[test]
fn traversal_epoll_mutation() {
    let fixture = Fixture::new();
    let graph = OwnershipGraph::new(2).unwrap();
    let (a_number, a) = fixture.epoll();
    let (b_number, b) = fixture.epoll();
    let (c_number, _) = fixture.epoll();
    graph
        .add(
            &fixture.lease(a_number),
            &a,
            fixture.lease(b_number),
            Fixture::interest(),
            1,
        )
        .unwrap();
    assert_eq!(
        graph.add(
            &fixture.lease(b_number),
            &b,
            fixture.lease(c_number),
            Fixture::interest(),
            2,
        ),
        Err(GraphError::ResourceLimit)
    );
    assert!(b.snapshot().watches.is_empty());
}
