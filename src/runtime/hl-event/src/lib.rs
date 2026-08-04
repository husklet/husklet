//! Pollable Linux event objects and readiness state machines.

#![forbid(unsafe_code)]

mod catalog;
mod checkpoint;
mod checkpoint_activity;
mod epoll;
mod eventfd;
mod inotify;
mod signalfd;
mod status;
mod timerfd;

pub use catalog::{EventCatalog, EventCatalogError};
pub use checkpoint::{
    EVENT_CHECKPOINT_OBJECT_MAXIMUM, EVENT_CHECKPOINT_VERSION, EpollTargetCheckpoint, EventCatalogRestore,
    EventCheckpointError, EventCheckpointImage, EventCheckpointRebind, EventObjectCheckpoint, EventObjectId,
    EventObjectState, EventResourceKey, InotifyWatchCheckpoint,
};
pub use eventfd::{
    EventFd, EventFdError, EventFdFlags, EventFdSnapshot, EventFdStatus, EventInterest, EventSubscription,
};
pub use inotify::model::{
    InotifyError, InotifyEventSnapshot, InotifyLimits, InotifyMask, InotifySnapshot, InotifyStatus,
    InotifyWatchSnapshot, WatchBinding, WatchNodeIdentity, WatchPathIdentity, WatchRequest, WatchSourceError,
    WatchSourceEvent,
};
pub use inotify::{Inotify, PreparedInotifyRead, WatchSource, WatchSourceObserver, WatchSourceSubscription};
pub use signalfd::{
    PreparedSignalSelection, SIGNALFD_RECORD_SIZE, SignalFd, SignalFdError, SignalFdFlags, SignalFdSnapshot,
    SignalFdStatus, SignalInfo, SignalMask, SignalObserver, SignalQueue, SignalQueueError, SignalSubscription,
};
pub use status::EventStatus;
pub use timerfd::{
    CreateFlags, CreateFlags as TimerFdCreateFlags, PreparedTimerRead, SetFlags, SetFlags as TimerFdSetFlags,
    TimerClockSource, TimerFd, TimerFdClock, TimerFdError, TimerFdSnapshot, TimerFdStatus, TimerSetting,
};

#[cfg(test)]
mod catalog_test;
#[cfg(test)]
#[path = "epoll/test.rs"]
mod epoll_test;
#[cfg(test)]
mod signalfd_test;
#[cfg(test)]
mod test;
#[cfg(test)]
#[path = "timerfd/test.rs"]
mod timerfd_test;
pub use epoll::model::{
    EpollError, EpollEvent, EpollInterest, EpollSnapshot, EpollStatus, EpollWatchKey, EpollWatchSnapshot,
};
pub use epoll::{Epoll, EpollBatch};
