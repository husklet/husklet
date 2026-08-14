//! Channel bookkeeping and credit-based backpressure.
//!
//! Structure, rows, subscriptions, and bulk bytes ride separate channels so a
//! slow query cannot delay a button becoming enabled, and a large log cannot
//! delay a listing. A sender with no credit stops producing; nothing here ever
//! spins or drops silently.

use std::collections::BTreeMap;

use crate::frame::ChannelId;

/// What a channel carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    /// Correlated request and response.
    Call,
    /// Host-pushed state, which may coalesce.
    Subscription,
    /// A rendered surface: mutations out, input back.
    Interface,
    /// Bulk bytes, such as logs or file contents.
    Stream,
}

impl Purpose {
    /// Whether superseding values may replace queued ones. A state snapshot is
    /// wholly described by its newest value; a byte stream is not.
    #[must_use]
    pub const fn coalesces(self) -> bool {
        matches!(self, Self::Subscription)
    }
}

/// Why a channel operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Exhausted,
    Unknown(ChannelId),
    Duplicate(ChannelId),
    Reserved,
    WrongOrigin(ChannelId),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted => write!(formatter, "the channel limit of {} is reached", Channels::LIMIT),
            Self::Unknown(id) => write!(formatter, "channel {} is not open", id.raw()),
            Self::Duplicate(id) => write!(formatter, "channel {} is already open", id.raw()),
            Self::Reserved => write!(formatter, "the control channel cannot be opened or closed"),
            Self::WrongOrigin(id) => write!(formatter, "channel {} was opened by the other side", id.raw()),
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether a send may proceed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    /// Credit was consumed; send.
    Send,
    /// No credit. Stop producing and wait for the receiver to return some.
    Await,
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    purpose: Purpose,
    credit: u32,
    withheld: u64,
}

/// The open channels of one session.
#[derive(Debug, Default)]
pub struct Channels {
    open: BTreeMap<ChannelId, Channel>,
    next_host: u32,
}

impl Channels {
    /// Channels open at once, excluding control.
    pub const LIMIT: usize = 64;
    /// Frames a channel may send before the receiver returns credit.
    pub const CREDIT: u32 = 32;

    #[must_use]
    pub fn new() -> Self {
        Self {
            open: BTreeMap::new(),
            next_host: 2,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.open.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    #[must_use]
    pub fn purpose(&self, id: ChannelId) -> Option<Purpose> {
        self.open.get(&id).map(|channel| channel.purpose)
    }

    #[must_use]
    pub fn credit(&self, id: ChannelId) -> Option<u32> {
        self.open.get(&id).map(|channel| channel.credit)
    }

    /// Frames dropped by coalescing on this channel, so a receiver can be told
    /// that it is seeing the newest state rather than every state.
    #[must_use]
    pub fn withheld(&self, id: ChannelId) -> u64 {
        self.open.get(&id).map_or(0, |channel| channel.withheld)
    }

    /// Opens a host-originated channel, allocating the next even identifier.
    ///
    /// # Errors
    /// Returns `Refusal::Exhausted` at the limit.
    pub fn open(&mut self, purpose: Purpose) -> Result<ChannelId, Refusal> {
        if self.open.len() >= Self::LIMIT {
            return Err(Refusal::Exhausted);
        }
        let id = ChannelId::new(self.next_host);
        self.next_host = self.next_host.saturating_add(2);
        self.insert(id, purpose);
        Ok(id)
    }

    /// Accepts an extension-originated channel.
    ///
    /// # Errors
    /// Returns a refusal when the limit is reached, the identifier is already
    /// open, names the control channel, or has host parity.
    pub fn accept(&mut self, id: ChannelId, purpose: Purpose) -> Result<(), Refusal> {
        if id == ChannelId::CONTROL {
            return Err(Refusal::Reserved);
        }
        if id.is_host() {
            return Err(Refusal::WrongOrigin(id));
        }
        if self.open.contains_key(&id) {
            return Err(Refusal::Duplicate(id));
        }
        if self.open.len() >= Self::LIMIT {
            return Err(Refusal::Exhausted);
        }
        self.insert(id, purpose);
        Ok(())
    }

    /// # Errors
    /// Returns a refusal when the channel is not open or is the control channel.
    pub fn close(&mut self, id: ChannelId) -> Result<(), Refusal> {
        if id == ChannelId::CONTROL {
            return Err(Refusal::Reserved);
        }
        self.open.remove(&id).ok_or(Refusal::Unknown(id))?;
        Ok(())
    }

    /// Consumes one frame of credit.
    ///
    /// # Errors
    /// Returns `Refusal::Unknown` for a channel that is not open.
    pub fn reserve(&mut self, id: ChannelId) -> Result<Permission, Refusal> {
        let channel = self.open.get_mut(&id).ok_or(Refusal::Unknown(id))?;
        if channel.credit == 0 {
            if channel.purpose.coalesces() {
                channel.withheld = channel.withheld.saturating_add(1);
            }
            return Ok(Permission::Await);
        }
        channel.credit -= 1;
        Ok(Permission::Send)
    }

    /// Returns credit as the receiver consumes frames.
    ///
    /// # Errors
    /// Returns `Refusal::Unknown` for a channel that is not open.
    pub fn replenish(&mut self, id: ChannelId, frames: u32) -> Result<(), Refusal> {
        let channel = self.open.get_mut(&id).ok_or(Refusal::Unknown(id))?;
        channel.credit = channel.credit.saturating_add(frames).min(Self::CREDIT);
        Ok(())
    }

    /// Clears the coalescing tally after the superseding frame is sent.
    pub fn resolve(&mut self, id: ChannelId) -> u64 {
        self.open
            .get_mut(&id)
            .map_or(0, |channel| std::mem::take(&mut channel.withheld))
    }

    fn insert(&mut self, id: ChannelId, purpose: Purpose) {
        self.open.insert(
            id,
            Channel {
                purpose,
                credit: Self::CREDIT,
                withheld: 0,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelId, Channels, Permission, Purpose, Refusal};

    #[test]
    fn the_two_sides_allocate_without_colliding() {
        let mut channels = Channels::new();
        let host = channels.open(Purpose::Interface).expect("opened");
        assert!(host.is_host());

        let extension = ChannelId::new(3);
        assert!(!extension.is_host());
        channels.accept(extension, Purpose::Call).expect("accepted");
        assert_eq!(channels.len(), 2);
    }

    #[test]
    fn an_extension_cannot_claim_a_host_identifier_or_control() {
        let mut channels = Channels::new();
        assert_eq!(
            channels.accept(ChannelId::new(4), Purpose::Call),
            Err(Refusal::WrongOrigin(ChannelId::new(4)))
        );
        assert_eq!(
            channels.accept(ChannelId::CONTROL, Purpose::Call),
            Err(Refusal::Reserved)
        );
        assert_eq!(channels.close(ChannelId::CONTROL), Err(Refusal::Reserved));
    }

    #[test]
    fn the_open_channel_count_is_bounded() {
        let mut channels = Channels::new();
        for _ in 0..Channels::LIMIT {
            channels.open(Purpose::Call).expect("within the limit");
        }
        assert_eq!(channels.open(Purpose::Call), Err(Refusal::Exhausted));
        assert_eq!(
            channels.accept(ChannelId::new(9_999), Purpose::Call),
            Err(Refusal::Exhausted)
        );
    }

    #[test]
    fn a_sender_stops_when_credit_runs_out() {
        let mut channels = Channels::new();
        let id = channels.open(Purpose::Stream).expect("opened");

        for _ in 0..Channels::CREDIT {
            assert_eq!(channels.reserve(id), Ok(Permission::Send));
        }
        assert_eq!(
            channels.reserve(id),
            Ok(Permission::Await),
            "an exhausted sender waits rather than sending or spinning"
        );

        channels.replenish(id, 2).expect("open");
        assert_eq!(channels.reserve(id), Ok(Permission::Send));
    }

    #[test]
    fn credit_never_exceeds_the_advertised_window() {
        let mut channels = Channels::new();
        let id = channels.open(Purpose::Call).expect("opened");
        channels.replenish(id, u32::MAX).expect("open");
        assert_eq!(channels.credit(id), Some(Channels::CREDIT));
    }

    #[test]
    fn a_blocked_subscription_records_what_it_superseded() {
        let mut channels = Channels::new();
        let id = channels.open(Purpose::Subscription).expect("opened");
        for _ in 0..Channels::CREDIT {
            channels.reserve(id).expect("open");
        }

        channels.reserve(id).expect("open");
        channels.reserve(id).expect("open");
        assert_eq!(channels.withheld(id), 2, "a snapshot is described by its newest value");

        channels.replenish(id, 1).expect("open");
        assert_eq!(
            channels.resolve(id),
            2,
            "the receiver is told, not silently shortchanged"
        );
        assert_eq!(channels.withheld(id), 0);
    }

    #[test]
    fn a_byte_stream_never_coalesces() {
        let mut channels = Channels::new();
        let id = channels.open(Purpose::Stream).expect("opened");
        for _ in 0..Channels::CREDIT {
            channels.reserve(id).expect("open");
        }

        channels.reserve(id).expect("open");

        assert_eq!(channels.withheld(id), 0, "dropping bytes would corrupt the stream");
    }

    #[test]
    fn operating_on_a_closed_channel_is_refused() {
        let mut channels = Channels::new();
        let id = channels.open(Purpose::Call).expect("opened");
        channels.close(id).expect("closed");

        assert_eq!(channels.reserve(id), Err(Refusal::Unknown(id)));
        assert_eq!(channels.replenish(id, 1), Err(Refusal::Unknown(id)));
        assert_eq!(channels.close(id), Err(Refusal::Unknown(id)));
    }

    #[test]
    fn a_closed_identifier_is_never_reused() {
        let mut channels = Channels::new();
        let first = channels.open(Purpose::Call).expect("opened");
        channels.close(first).expect("closed");
        let second = channels.open(Purpose::Call).expect("opened");
        assert_ne!(first, second, "a late frame must not land on a new channel");
    }
}
