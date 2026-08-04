use super::{Descriptor, DescriptorSyscalls, HostError};
use std::ffi::CStr;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchInterest(u32);

impl WatchInterest {
    pub const MODIFY: Self = Self(1);
    pub const CREATE: Self = Self(2);
    pub const DELETE: Self = Self(4);
    pub const MOVE: Self = Self(8);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchToken(i32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchEvent {
    pub token: WatchToken,
    pub interests: WatchInterest,
    pub cookie: u32,
    pub name: Vec<u8>,
}

pub trait WatchSyscalls: DescriptorSyscalls {
    fn watch_create(&self) -> Result<i32, HostError>;
    fn watch_add(&self, descriptor: i32, path: &CStr, interests: u32) -> Result<i32, HostError>;
    fn watch_remove(&self, descriptor: i32, watch: i32) -> Result<(), HostError>;
    fn watch_read(&self, descriptor: i32, events: &mut Vec<WatchEvent>) -> Result<(), HostError>;
}

pub struct FileWatch<S: WatchSyscalls> {
    descriptor: Descriptor<S>,
}

impl<S: WatchSyscalls> FileWatch<S> {
    pub fn create(syscalls: Arc<S>) -> Result<Self, HostError> {
        let raw = syscalls.watch_create()?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn add(&self, path: &CStr, interests: WatchInterest) -> Result<WatchToken, HostError> {
        self.descriptor
            .syscalls()
            .watch_add(self.descriptor.raw(), path, interests.0)
            .map(WatchToken)
    }

    pub fn remove(&self, token: WatchToken) -> Result<(), HostError> {
        self.descriptor.syscalls().watch_remove(self.descriptor.raw(), token.0)
    }

    pub fn read(&self) -> Result<Vec<WatchEvent>, HostError> {
        let mut events = Vec::new();
        self.descriptor
            .syscalls()
            .watch_read(self.descriptor.raw(), &mut events)?;
        Ok(events)
    }
}

impl WatchEvent {
    pub(crate) fn native(token: i32, interests: u32, cookie: u32, name: Vec<u8>) -> Self {
        Self {
            token: WatchToken(token),
            interests: WatchInterest(interests),
            cookie,
            name,
        }
    }
}
