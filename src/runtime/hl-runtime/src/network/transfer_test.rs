use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptorTable, OpenFileDescription, StatusFlags};

use super::{
    HostControl, HostSend, HostSendResult, ImportedDescription, ImportedTransfer, TransferCommitError,
    TransferPublication,
};
use crate::RuntimeNetworkError;

#[derive(Debug)]
struct Object;

impl OpenFileDescription for Object {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Bound,
    Copied,
    Committed,
    RolledBack,
}

struct Publication {
    events: Arc<Mutex<Vec<Event>>>,
}

struct RejectedPublication {
    events: Arc<Mutex<Vec<Event>>>,
}

impl TransferPublication for Publication {
    fn bind(&mut self, identities: &[hl_descriptor::DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        assert_eq!(identities.len(), 2);
        self.events.lock().unwrap().push(Event::Bound);
        Ok(())
    }

    fn commit(self: Box<Self>) {
        self.events.lock().unwrap().push(Event::Committed);
    }

    fn rollback(self: Box<Self>) {
        self.events.lock().unwrap().push(Event::RolledBack);
    }
}

impl TransferPublication for RejectedPublication {
    fn bind(&mut self, _: &[hl_descriptor::DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        self.events.lock().unwrap().push(Event::Bound);
        Err(RuntimeNetworkError::Failed)
    }

    fn commit(self: Box<Self>) {
        panic!("rejected publication cannot commit");
    }

    fn rollback(self: Box<Self>) {
        self.events.lock().unwrap().push(Event::RolledBack);
    }
}

fn imported(events: Arc<Mutex<Vec<Event>>>) -> ImportedTransfer {
    ImportedTransfer::new(
        vec![
            ImportedDescription {
                object: Arc::new(Object),
                status: StatusFlags::default(),
            },
            ImportedDescription {
                object: Arc::new(Object),
                status: StatusFlags::default(),
            },
        ],
        Box::new(Publication { events }),
    )
}

#[test]
fn file_transfer_publishes() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let table = DescriptorTable::new(8).unwrap();
    let prepared = imported(events.clone()).prepare(&table, true).unwrap();
    let numbers = prepared
        .publish_after(|numbers| {
            assert_eq!(numbers, [0, 1]);
            assert!(table.pin(0).is_err());
            events.lock().unwrap().push(Event::Copied);
            Ok::<_, ()>(())
        })
        .unwrap();

    assert_eq!(numbers, [0, 1]);
    assert!(table.pin(0).is_ok());
    assert!(table.pin(1).is_ok());
    assert_eq!(*events.lock().unwrap(), [Event::Bound, Event::Copied, Event::Committed]);
}

#[test]
fn copyout_rolls_back() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let table = DescriptorTable::new(8).unwrap();
    let prepared = imported(events.clone()).prepare(&table, false).unwrap();
    let result = prepared.publish_after(|_| {
        events.lock().unwrap().push(Event::Copied);
        Err("fault")
    });

    assert_eq!(result, Err(TransferCommitError::Copyout("fault")));
    assert!(table.pin(0).is_err());
    assert!(table.pin(1).is_err());
    assert_eq!(
        *events.lock().unwrap(),
        [Event::Bound, Event::Copied, Event::RolledBack]
    );
}

#[test]
fn record_zero_rights() {
    let request = HostSend {
        payload: Vec::new(),
        address: None,
        controls: vec![HostControl::Rights(vec![7_u8])],
        nonblocking: false,
        record: true,
    };
    let result = HostSendResult {
        count: 0,
        rights_consumed: true,
    };

    assert!(request.record);
    assert!(result.rights_consumed);
}

#[test]
fn binding_rolls_back() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let table = DescriptorTable::new(8).unwrap();
    let transfer = ImportedTransfer::new(
        vec![ImportedDescription {
            object: Arc::new(Object),
            status: StatusFlags::default(),
        }],
        Box::new(RejectedPublication { events: events.clone() }),
    );
    let result = transfer
        .prepare(&table, false)
        .unwrap()
        .publish_after(|_| Ok::<_, ()>(()));

    assert_eq!(result, Err(TransferCommitError::Runtime(RuntimeNetworkError::Failed)));
    assert!(table.pin(0).is_err());
    assert_eq!(*events.lock().unwrap(), [Event::Bound, Event::RolledBack]);
}
