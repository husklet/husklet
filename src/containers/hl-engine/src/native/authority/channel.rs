use crate::engine::EngineError;
use crate::native::HostDescriptor;

pub(super) const FRAME_LIMIT: u32 = 1024 * 1024;
const INFLIGHT_LIMIT: u16 = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Channel {
    descriptor: HostDescriptor,
    health: HostDescriptor,
    transfer: HostDescriptor,
    session: [u64; 2],
    frame_limit: u32,
    inflight_limit: u16,
}

impl Channel {
    pub fn new(
        descriptor: HostDescriptor,
        health: HostDescriptor,
        transfer: HostDescriptor,
        session: [u64; 2],
        frame_limit: u32,
        inflight_limit: u16,
    ) -> Result<Self, EngineError> {
        if session == [0, 0]
            || frame_limit == 0
            || frame_limit > FRAME_LIMIT
            || inflight_limit == 0
            || inflight_limit > INFLIGHT_LIMIT
        {
            return Err(EngineError::AuthorityFailed);
        }
        Ok(Self {
            descriptor,
            health,
            transfer,
            session,
            frame_limit,
            inflight_limit,
        })
    }

    #[must_use]
    pub const fn descriptor(self) -> HostDescriptor {
        self.descriptor
    }
    #[must_use]
    pub const fn health(self) -> HostDescriptor {
        self.health
    }
    #[must_use]
    pub const fn transfer(self) -> HostDescriptor {
        self.transfer
    }
    #[must_use]
    pub const fn session(self) -> [u64; 2] {
        self.session
    }
    #[must_use]
    pub const fn frame_limit(self) -> u32 {
        self.frame_limit
    }
    #[must_use]
    pub const fn inflight_limit(self) -> u16 {
        self.inflight_limit
    }
}

/// Consumer-owned port which creates one private authority session per launch.
pub trait Access: Send + Sync {
    fn open(&self, domain: [u64; 2]) -> Result<Channel, EngineError>;
    fn commit(&self, _: Channel) -> Result<(), EngineError> {
        Ok(())
    }
    fn rollback(&self, _: Channel) {}
}
