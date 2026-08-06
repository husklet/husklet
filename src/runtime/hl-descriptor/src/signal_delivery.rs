//! Signal-driven I/O ownership retained by an open file description.
use crate::StatusFlags;
use crate::model::OpenDescription;
use std::sync::atomic::Ordering;
/// Linux signal-delivery owner retained by an open file description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalOwner {
    Process(i32),
    Group(i32),
    Thread(i32),
}

/// Current signal-driven I/O delivery state for one open description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalDelivery {
    pub owner: SignalOwner,
    pub signal: u8,
}

/// Weak observation capability used by an OFD-owned readiness callback.
#[derive(Clone, Debug)]
pub struct SignalSource(pub(crate) std::sync::Weak<OpenDescription>);

impl SignalSource {
    /// Returns current ownership independently of the `O_ASYNC` flag.
    #[must_use]
    pub fn notification(&self) -> Option<SignalDelivery> {
        let description = self.0.upgrade()?;
        if description.retired.load(Ordering::Acquire) {
            return None;
        }
        let state = description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(SignalDelivery {
            owner: state.owner,
            signal: state.signal,
        })
    }

    /// Returns delivery state only while signal-driven I/O remains armed.
    #[must_use]
    pub fn delivery(&self) -> Option<SignalDelivery> {
        let description = self.0.upgrade()?;
        if description.retired.load(Ordering::Acquire) {
            return None;
        }
        let state = description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status.contains(StatusFlags::ASYNC).then_some(SignalDelivery {
            owner: state.owner,
            signal: state.signal,
        })
    }
}

impl SignalOwner {
    #[must_use]
    pub const fn from_legacy(owner: i32) -> Self {
        if owner < 0 {
            Self::Group(owner.saturating_abs())
        } else {
            Self::Process(owner)
        }
    }

    #[must_use]
    pub const fn legacy(self) -> i32 {
        match self {
            Self::Process(identity) | Self::Thread(identity) => identity,
            Self::Group(identity) => identity.saturating_neg(),
        }
    }
}
