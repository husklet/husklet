use std::sync::Arc;

use hl_event::{EventResourceKey, SignalQueue, TimerClockSource, WatchSource};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceError {
    Unsupported,
    NotFound,
    Interrupted,
    Failed,
}

pub trait TimerEventSource: std::fmt::Debug + Send + Sync {
    fn clock(&self) -> Result<(EventResourceKey, Arc<dyn TimerClockSource>), SourceError>;
}

pub trait SignalEventSource: std::fmt::Debug + Send + Sync {
    fn queue(&self) -> Result<(EventResourceKey, Arc<dyn SignalQueue>), SourceError>;
}

pub trait WatchEventSource: std::fmt::Debug + Send + Sync {
    fn watches(&self) -> Result<(EventResourceKey, Arc<dyn WatchSource>), SourceError>;
}
