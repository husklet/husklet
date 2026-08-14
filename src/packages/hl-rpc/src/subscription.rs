//! What turns a host state change into an emission on the right channel.
//!
//! [`Channels`] says how much may be sent and [`Outbox`] says what happens when
//! a subscriber falls behind; neither knows which channel a topic belongs on.
//! That routing lives here, together with the [`Parcel`] of bytes a bulk stream
//! carries.
//!
//! State and bytes are separated because their failure modes are opposites. A
//! state subscription may drop superseded values, so its payload has to survive
//! being the only one that arrives. A byte stream may not drop anything, so its
//! producer has to stop instead.

use std::collections::{BTreeMap, BTreeSet};

use crate::channel::{Channels, Purpose, Refusal};
use crate::coding::Coding;
use crate::frame::ChannelId;
use crate::outbox::{Emission, Outbox};
use crate::session::{Session, Topic};

/// Which channel each followed topic is delivered on.
///
/// One channel per topic, so a busy container listing cannot delay an image
/// listing, and so coalescing on one topic cannot discard another's value.
#[derive(Debug)]
pub struct Subscriptions<T: Topic> {
    routes: BTreeMap<T, ChannelId>,
}

impl<T: Topic> Default for Subscriptions<T> {
    fn default() -> Self {
        Self {
            routes: BTreeMap::new(),
        }
    }
}

impl<T: Topic> Subscriptions<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Topics currently routed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// The channel a topic is delivered on, if it is open.
    #[must_use]
    pub fn channel(&self, topic: T) -> Option<ChannelId> {
        self.routes.get(&topic).copied()
    }

    /// Allocates the channel a topic will be delivered on.
    ///
    /// Asking twice returns the channel already allocated rather than a second
    /// one, so a repeated subscribe cannot consume the channel budget.
    ///
    /// # Errors
    /// Returns `Refusal::Exhausted` when the session has no channel left.
    pub fn open(&mut self, topic: T, channels: &mut Channels) -> Result<ChannelId, Refusal> {
        if let Some(existing) = self.routes.get(&topic) {
            return Ok(*existing);
        }
        let channel = channels.open(Purpose::Subscription)?;
        self.routes.insert(topic, channel);
        Ok(channel)
    }

    /// Releases the channel a topic was delivered on, discarding whatever it
    /// still held, and returns it. Closing has to return the channel to the
    /// budget, or a long-lived session that subscribes and unsubscribes
    /// repeatedly would exhaust itself without ever holding many subscriptions
    /// at once.
    ///
    /// `None` when the topic was not routed. A route is only ever created
    /// alongside its channel, so a channel that refuses to close means the two
    /// have drifted apart, and the route is dropped either way rather than left
    /// pointing at something the session no longer owns.
    pub fn close(&mut self, topic: T, channels: &mut Channels, outbox: &mut Outbox<T>) -> Option<ChannelId> {
        let channel = self.routes.remove(&topic)?;
        outbox.discard(channel);
        channels.close(channel).ok().map(|()| channel)
    }

    /// Routes one listing to the channel its topic is delivered on.
    ///
    /// Refused when the session no longer follows the topic or no longer holds
    /// the capability behind it. Both questions are asked of
    /// [`Session::may_emit`] rather than answered here, so there is exactly one
    /// definition of who may receive what, and a revoked grant stops a
    /// subscription that was established while it was still held.
    pub fn emit(
        &mut self,
        topic: T,
        payload: Vec<u8>,
        session: &Session<T>,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) -> Emission {
        if !session.may_emit(topic) {
            return Emission::Ignored;
        }
        let Some(channel) = self.routes.get(&topic) else {
            return Emission::Ignored;
        };
        outbox.emit(channels, *channel, Some(topic), payload)
    }
}

/// One piece of a byte stream.
///
/// The end is a variant rather than a zero-length chunk. A reader that saw only
/// bytes could not tell "nothing more has been produced yet" from "there will
/// never be more", and a log that legitimately writes an empty chunk would be
/// read as finished.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Parcel {
    /// Bytes to append to what the reader has already received.
    Chunk(Vec<u8>),
    /// No further bytes will follow on this stream.
    End,
}

impl Parcel {
    /// The byte that distinguishes a chunk from the end of the stream.
    const CHUNK: u8 = 0;
    const END: u8 = 1;

    /// Encodes the parcel as a frame payload, tagged so that an empty chunk and
    /// the end marker do not encode identically.
    #[must_use]
    pub fn payload(&self) -> Vec<u8> {
        match self {
            Self::Chunk(bytes) => [&[Self::CHUNK], bytes.as_slice()].concat(),
            Self::End => vec![Self::END],
        }
    }

    /// Reads a parcel back from a frame payload.
    ///
    /// # Errors
    /// Returns `Coding::Malformed` when the payload is empty or carries a tag
    /// this host does not define.
    pub fn read(payload: &[u8]) -> Result<Self, Coding> {
        let (tag, bytes) = payload
            .split_first()
            .ok_or_else(|| Coding::Malformed("a stream payload carries no marker".into()))?;
        match *tag {
            Self::CHUNK => Ok(Self::Chunk(bytes.to_vec())),
            Self::END => Ok(Self::End),
            other => Err(Coding::Malformed(format!("unknown stream marker {other}"))),
        }
    }

    /// Whether this parcel ends the stream.
    #[must_use]
    pub const fn ends(&self) -> bool {
        matches!(self, Self::End)
    }
}

/// The bulk byte streams one session has open.
///
/// Container logs and file contents ride these. Nothing here ever drops a
/// chunk: bytes cannot coalesce, so a stream with no credit stops its producer
/// and resumes where it left off.
#[derive(Debug, Default)]
pub struct Streams {
    open: BTreeSet<ChannelId>,
}

impl Streams {
    /// Streams one session may hold open at once.
    ///
    /// Bounded well below the channel limit so that bulk reads cannot consume
    /// the whole budget and leave a session unable to open a subscription or
    /// answer a call.
    pub const LIMIT: usize = 8;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.open.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Whether a stream is still open on this channel.
    #[must_use]
    pub fn contains(&self, channel: ChannelId) -> bool {
        self.open.contains(&channel)
    }

    /// Opens a stream channel.
    ///
    /// # Errors
    /// Returns `Refusal::Exhausted` when the session already holds
    /// [`Streams::LIMIT`] streams, or when the channel budget is spent. A
    /// refusal allocates nothing, so the streams already open are untouched.
    pub fn open(&mut self, channels: &mut Channels) -> Result<ChannelId, Refusal> {
        if self.open.len() >= Self::LIMIT {
            return Err(Refusal::Exhausted);
        }
        let channel = channels.open(Purpose::Stream)?;
        self.open.insert(channel);
        Ok(channel)
    }

    /// Queues one chunk of bytes.
    ///
    /// Returns `Emission::Blocked` when the reader has returned no credit. The
    /// producer must then stop and offer the same chunk again, because dropping
    /// it would corrupt the result rather than merely age it.
    pub fn write<T: Topic>(
        &mut self,
        channel: ChannelId,
        chunk: Vec<u8>,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) -> Emission {
        self.send(channel, &Parcel::Chunk(chunk), channels, outbox)
    }

    /// Queues the end-of-stream marker.
    ///
    /// Queued like any other parcel, so it arrives after the bytes before it
    /// rather than overtaking them. A blocked marker must be offered again.
    pub fn finish<T: Topic>(
        &mut self,
        channel: ChannelId,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) -> Emission {
        self.send(channel, &Parcel::End, channels, outbox)
    }

    /// Releases a stream channel, discarding whatever it still held.
    ///
    /// # Errors
    /// Returns `Refusal::Unknown` when no stream is open on that channel.
    pub fn close<T: Topic>(
        &mut self,
        channel: ChannelId,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) -> Result<(), Refusal> {
        if !self.open.remove(&channel) {
            return Err(Refusal::Unknown(channel));
        }
        outbox.discard(channel);
        channels.close(channel)
    }

    /// A stream carries no topic: it is one continuous body of bytes, and a
    /// topic is what the outbox would coalesce on.
    fn send<T: Topic>(
        &self,
        channel: ChannelId,
        parcel: &Parcel,
        channels: &mut Channels,
        outbox: &mut Outbox<T>,
    ) -> Emission {
        if !self.open.contains(&channel) {
            return Emission::Ignored;
        }
        outbox.emit(channels, channel, None, parcel.payload())
    }
}

#[cfg(test)]
mod tests {
    use super::{Parcel, Streams, Subscriptions};
    use crate::capability::{Capability, CapabilityKey};
    use crate::channel::Channels;
    use crate::frame::ChannelId;
    use crate::outbox::{Emission, Outbox};
    use crate::session::Topic;

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Reach {
        Read,
    }

    impl Capability for Reach {
        const DOMAIN: &'static str = "sample";
        const ALL: &'static [Self] = &[Self::Read];

        fn name(&self) -> &'static str {
            "read"
        }
    }

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Subject {
        Containers,
        Images,
    }

    impl Topic for Subject {
        fn requirement(&self) -> CapabilityKey {
            Reach::Read.key()
        }
    }

    #[test]
    fn an_empty_chunk_is_not_the_end_of_the_stream() {
        let empty = Parcel::Chunk(Vec::new());
        assert_ne!(empty.payload(), Parcel::End.payload());
        assert_eq!(Parcel::read(&empty.payload()).expect("read"), empty);
        assert!(Parcel::read(&Parcel::End.payload()).expect("read").ends());
        assert!(!empty.ends());
    }

    #[test]
    fn a_stream_payload_without_a_marker_is_refused() {
        assert!(Parcel::read(&[]).is_err());
        assert!(Parcel::read(&[9]).is_err());
    }

    #[test]
    fn subscribing_twice_allocates_one_channel() {
        let mut channels = Channels::new();
        let mut subscriptions = Subscriptions::default();

        let first = subscriptions.open(Subject::Containers, &mut channels).expect("opened");
        let second = subscriptions.open(Subject::Containers, &mut channels).expect("opened");

        assert_eq!(first, second);
        assert_eq!(channels.len(), 1);
        assert_eq!(subscriptions.len(), 1);
    }

    #[test]
    fn closing_an_unrouted_topic_releases_nothing() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let mut subscriptions = Subscriptions::default();

        assert_eq!(subscriptions.close(Subject::Images, &mut channels, &mut outbox), None);
    }

    #[test]
    fn a_stream_that_was_never_opened_receives_nothing() {
        let mut channels = Channels::new();
        let mut outbox: Outbox<Subject> = Outbox::new();
        let mut streams = Streams::new();
        let ghost = ChannelId::new(80);

        assert_eq!(
            streams.write(ghost, b"x".to_vec(), &mut channels, &mut outbox),
            Emission::Ignored
        );
        assert!(outbox.is_empty());
    }
}
