use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{
    Identity, XATTR_NAME_MAXIMUM, XATTR_VALUE_MAXIMUM, XattrError, XattrFlags, XattrHost, XattrMutation, XattrName,
    Xattrs,
};

#[derive(Clone)]
struct FakeXattrHost {
    state: Arc<Mutex<FakeState>>,
}

struct FakeState {
    next: u64,
    staged: HashMap<u64, Vec<u8>>,
    failure: Option<Failure>,
    transcript: Vec<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Failure {
    Stage,
    Commit,
}

impl FakeXattrHost {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                next: 1,
                staged: HashMap::new(),
                failure: None,
                transcript: Vec::new(),
            })),
        }
    }

    fn fail(&self, failure: Failure) {
        self.state.lock().unwrap().failure = Some(failure);
    }

    fn transcript(&self) -> Vec<String> {
        self.state.lock().unwrap().transcript.clone()
    }
}

impl XattrHost for FakeXattrHost {
    type Transaction = u64;

    fn begin_xattr(&self, _file: Identity) -> Result<Self::Transaction, XattrError> {
        let mut state = self.state.lock().unwrap();
        let transaction = state.next;
        state.next += 1;
        state.staged.insert(transaction, Vec::new());
        state.transcript.push(format!("begin:{transaction}"));
        Ok(transaction)
    }

    fn stage_xattr(&self, transaction: Self::Transaction, mutation: XattrMutation<'_>) -> Result<(), XattrError> {
        let mut state = self.state.lock().unwrap();
        if state.failure == Some(Failure::Stage) {
            return Err(XattrError::Host);
        }
        let name = match mutation {
            XattrMutation::Set { name, .. } | XattrMutation::Remove { name } => name,
        };
        state.staged.insert(transaction, name.as_bytes().to_vec());
        state.transcript.push(format!("stage:{transaction}"));
        Ok(())
    }

    fn commit_xattr(&self, transaction: Self::Transaction) -> Result<(), XattrError> {
        let mut state = self.state.lock().unwrap();
        state.transcript.push(format!("commit:{transaction}"));
        if state.failure == Some(Failure::Commit) {
            return Err(XattrError::Host);
        }
        state.staged.remove(&transaction);
        Ok(())
    }

    fn rollback_xattr(&self, transaction: Self::Transaction) {
        let mut state = self.state.lock().unwrap();
        state.staged.remove(&transaction);
        state.transcript.push(format!("rollback:{transaction}"));
    }
}

struct XattrFixture;

impl XattrFixture {
    fn store(host: FakeXattrHost) -> Xattrs<FakeXattrHost> {
        Xattrs::new(host, Identity { device: 7, inode: 11 })
    }

    fn name(bytes: &[u8]) -> XattrName {
        XattrName::new(bytes).unwrap()
    }
}

#[test]
fn flags_create_contract() {
    let host = FakeXattrHost::new();
    let xattrs = XattrFixture::store(host.clone());
    let name = XattrFixture::name(b"user.k");
    assert_eq!(XattrFlags::from_bits(3), Err(XattrError::InvalidFlags));
    assert_eq!(xattrs.set(&name, b"x", XattrFlags::Replace), Err(XattrError::NoData));
    xattrs.set(&name, b"hello", XattrFlags::Upsert).unwrap();
    assert_eq!(
        xattrs.set(&name, b"x", XattrFlags::Create),
        Err(XattrError::AlreadyExists)
    );
    assert_eq!(host.transcript().len(), 3);
}

#[test]
fn get_probe_missing() {
    let xattrs = XattrFixture::store(FakeXattrHost::new());
    let name = XattrFixture::name(b"user.k");
    assert_eq!(xattrs.get(&name, None), Err(XattrError::NoData));
    xattrs.set(&name, b"hello", XattrFlags::Upsert).unwrap();
    assert_eq!(xattrs.get(&name, None), Ok(5));
    assert_eq!(xattrs.get(&name, Some(&mut [])), Ok(5));
    assert_eq!(xattrs.get(&name, Some(&mut [0; 2])), Err(XattrError::Range));
    xattrs.set(&name, b"world!", XattrFlags::Upsert).unwrap();
    let mut output = [0_u8; 6];
    assert_eq!(xattrs.get(&name, Some(&mut output)), Ok(6));
    assert_eq!(&output, b"world!");
    xattrs.remove(&name).unwrap();
    assert_eq!(xattrs.remove(&name), Err(XattrError::NoData));
}

#[test]
fn list_probe_deterministic() {
    let xattrs = XattrFixture::store(FakeXattrHost::new());
    xattrs
        .set(&XattrFixture::name(b"user.z"), b"1", XattrFlags::Upsert)
        .unwrap();
    xattrs
        .set(&XattrFixture::name(b"user.a"), b"2", XattrFlags::Upsert)
        .unwrap();
    assert_eq!(xattrs.list(None), Ok(14));
    assert_eq!(xattrs.list(Some(&mut [0_u8; 1])), Err(XattrError::Range));
    let mut output = [0_u8; 14];
    assert_eq!(xattrs.list(Some(&mut output)), Ok(14));
    assert_eq!(&output, b"user.a\0user.z\0");
}

#[test]
fn invalid_utf8_identity() {
    let xattrs = XattrFixture::store(FakeXattrHost::new());
    let high = XattrFixture::name(b"user.\xff");
    let slash = XattrFixture::name(b"user.a/b");
    xattrs.set(&high, b"high", XattrFlags::Create).unwrap();
    xattrs.set(&slash, b"slash", XattrFlags::Create).unwrap();

    let mut value = [0; 5];
    assert_eq!(xattrs.get(&high, Some(&mut value)), Ok(4));
    assert_eq!(&value[..4], b"high");
    let mut names = vec![0; xattrs.list(None).unwrap()];
    let length = xattrs.list(Some(&mut names)).unwrap();
    assert_eq!(&names[..length], b"user.a/b\0user.\xff\0");
    xattrs.remove(&high).unwrap();
    assert_eq!(xattrs.get(&high, None), Err(XattrError::NoData));
}

#[test]
fn name_value_mutation() {
    let host = FakeXattrHost::new();
    assert_eq!(XattrName::new(b""), Err(XattrError::InvalidName));
    assert_eq!(
        XattrName::new(&vec![b'x'; XATTR_NAME_MAXIMUM + 1]),
        Err(XattrError::InvalidName)
    );
    let xattrs = XattrFixture::store(host.clone());
    assert_eq!(
        xattrs.set(
            &XattrFixture::name(b"user.large"),
            &vec![0; XATTR_VALUE_MAXIMUM + 1],
            XattrFlags::Upsert,
        ),
        Err(XattrError::ValueTooLarge)
    );
    assert!(host.transcript().is_empty());
}

#[test]
fn stage_commit_change() {
    for failure in [Failure::Stage, Failure::Commit] {
        let host = FakeXattrHost::new();
        let xattrs = XattrFixture::store(host.clone());
        let name = XattrFixture::name(b"user.k");
        host.fail(failure);
        assert_eq!(xattrs.set(&name, b"value", XattrFlags::Upsert), Err(XattrError::Host));
        assert_eq!(xattrs.get(&name, None), Err(XattrError::NoData));
        assert!(host.transcript().iter().any(|event| event.starts_with("rollback:")));
    }
}
