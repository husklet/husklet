//! Queued emission with bounded memory.
//!
//! A subscriber that stops consuming must not be able to grow the host. State
//! subscriptions therefore coalesce — the newest snapshot is the whole truth,
//! so superseded ones are dropped and counted — while byte streams, where
//! dropping would corrupt the result, block the producer instead.

use std::collections::{BTreeMap, VecDeque};

use crate::channel::{Channels, Permission, Purpose};
use crate::frame::{ChannelId, Frame};

/// What happened to an emission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Emission {
    /// Queued for sending.
    Queued,
    /// Replaced a superseded value on a coalescing channel.
    Superseded,
    /// The channel is full and cannot drop values; the producer must stop.
    Blocked,
    /// The subscriber is not entitled to this topic, or is not following it.
    Ignored,
}

/// One queued message.
///
/// The topic is whatever the domain coalesces on, and this crate only ever
/// compares it: a queued value is replaced by a newer value of the same topic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message<T> {
    pub channel: ChannelId,
    pub topic: Option<T>,
    pub payload: Vec<u8>,
    /// Values this message superseded, so a receiver is told it is seeing the
    /// newest state rather than every state.
    pub superseded: u64,
}

/// Pending output for one session.
#[derive(Debug)]
pub struct Outbox<T> {
    queues: BTreeMap<ChannelId, VecDeque<Message<T>>>,
    bytes: usize,
    dropped: u64,
}

impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Self {
            queues: BTreeMap::new(),
            bytes: 0,
            dropped: 0,
        }
    }
}

impl<T: Copy + PartialEq> Outbox<T> {
    /// Messages held per channel before backpressure applies.
    pub const DEPTH: usize = 32;
    /// Messages held across a whole session, including queues whose owner
    /// failed to pair channel closure with explicit outbox disposal.
    pub const LIMIT: usize = Self::DEPTH * Channels::LIMIT;
    /// Payload bytes retained across a whole session. Two maximum-size frames
    /// can wait concurrently; further producers receive backpressure instead
    /// of multiplying that allocation by every channel and credit slot.
    pub const BYTE_LIMIT: usize = Frame::PAYLOAD_LIMIT * 2;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Messages waiting on a channel.
    #[must_use]
    pub fn depth(&self, channel: ChannelId) -> usize {
        self.queues.get(&channel).map_or(0, VecDeque::len)
    }

    /// Messages waiting across every channel. This is the number that must
    /// stay bounded however long a subscriber ignores its stream.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queues.values().map(VecDeque::len).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Payload bytes retained across every channel.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Values dropped by coalescing since the session opened.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Queues a message, applying credit and the channel's drop policy.
    pub fn emit(
        &mut self,
        channels: &mut Channels,
        channel: ChannelId,
        topic: Option<T>,
        payload: Vec<u8>,
    ) -> Emission {
        let Some(purpose) = channels.purpose(channel) else {
            return Emission::Ignored;
        };
        let message = Message {
            channel,
            topic,
            payload,
            superseded: 0,
        };
        match channels.reserve(channel) {
            Ok(Permission::Send) => {
                let emission = self.enqueue(channel, message, purpose);
                if emission == Emission::Blocked {
                    channels.refund(channel);
                }
                emission
            }
            Ok(Permission::Await) => self.withhold(channel, message, purpose),
            Err(_) => Emission::Ignored,
        }
    }

    /// Takes what may now be sent on a channel.
    pub fn drain(&mut self, channel: ChannelId) -> Vec<Message<T>> {
        let drained = self.queues.remove(&channel).unwrap_or_default();
        self.bytes = self
            .bytes
            .saturating_sub(drained.iter().map(|message| message.payload.len()).sum());
        drained.into_iter().collect()
    }

    /// Discards everything queued for a channel that is closing.
    pub fn discard(&mut self, channel: ChannelId) {
        if let Some(discarded) = self.queues.remove(&channel) {
            self.bytes = self
                .bytes
                .saturating_sub(discarded.iter().map(|message| message.payload.len()).sum());
        }
    }

    fn enqueue(&mut self, channel: ChannelId, message: Message<T>, purpose: Purpose) -> Emission {
        let full = self.len() >= Self::LIMIT;
        let byte_full = self
            .bytes
            .checked_add(message.payload.len())
            .is_none_or(|total| total > Self::BYTE_LIMIT);
        let queue = self.queues.entry(channel).or_default();
        if !full && !byte_full && queue.len() < Self::DEPTH {
            self.bytes += message.payload.len();
            queue.push_back(message);
            return Emission::Queued;
        }
        Self::supersede(queue, message, purpose, &mut self.bytes).map_or(Emission::Blocked, |dropped| {
            self.dropped = self.dropped.saturating_add(dropped);
            Emission::Superseded
        })
    }

    /// Handles an emission with no credit. A coalescing channel still records
    /// the newest value; anything else must stop the producer.
    fn withhold(&mut self, channel: ChannelId, message: Message<T>, purpose: Purpose) -> Emission {
        if !purpose.coalesces() {
            return Emission::Blocked;
        }
        let queue = self.queues.entry(channel).or_default();
        Self::supersede(queue, message, purpose, &mut self.bytes).map_or(Emission::Blocked, |dropped| {
            self.dropped = self.dropped.saturating_add(dropped);
            Emission::Superseded
        })
    }

    /// Replaces the newest queued value of the same topic, returning how many
    /// values were dropped. Returns `None` when the channel may not drop.
    fn supersede(
        queue: &mut VecDeque<Message<T>>,
        message: Message<T>,
        purpose: Purpose,
        bytes: &mut usize,
    ) -> Option<u64> {
        if !purpose.coalesces() {
            return None;
        }
        let existing = queue.iter().rposition(|held| held.topic == message.topic);
        let Some(index) = existing else {
            if queue.is_empty() {
                return None;
            }
            // A different topic on a full coalescing channel: drop the oldest
            // rather than refuse, since the queue is already bounded.
            let removed = queue.front()?.payload.len();
            let retained = bytes.saturating_sub(removed);
            if retained
                .checked_add(message.payload.len())
                .is_none_or(|total| total > Self::BYTE_LIMIT)
            {
                return None;
            }
            let dropped = queue.pop_front().map_or(0, |_| 1);
            *bytes = retained + message.payload.len();
            queue.push_back(message);
            return Some(dropped);
        };
        let removed = queue[index].payload.len();
        let retained = bytes.saturating_sub(removed);
        if retained
            .checked_add(message.payload.len())
            .is_none_or(|total| total > Self::BYTE_LIMIT)
        {
            return None;
        }
        let superseded = queue[index].superseded.saturating_add(1);
        *bytes = retained + message.payload.len();
        queue[index] = Message { superseded, ..message };
        Some(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{Emission, Outbox};
    use crate::channel::{Channels, Purpose};

    /// Two topics is all this needs: the subject is coalescing, not routing.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Topic {
        Containers,
        Images,
    }

    fn payload(value: u8) -> Vec<u8> {
        vec![value]
    }

    #[test]
    fn an_emission_on_an_unopened_channel_is_ignored() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let ghost = crate::frame::ChannelId::new(77);

        assert_eq!(
            outbox.emit(&mut channels, ghost, Some(Topic::Containers), payload(1)),
            Emission::Ignored
        );
        assert!(outbox.is_empty());
    }

    #[test]
    fn a_subscriber_that_never_consumes_cannot_grow_the_host() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("opened");

        for value in 0..10_000_u32 {
            outbox.emit(&mut channels, channel, Some(Topic::Containers), payload(value as u8));
        }

        assert!(
            outbox.len() <= Outbox::<Topic>::DEPTH,
            "queued {} messages for a subscriber that never read one",
            outbox.len()
        );
        assert!(outbox.dropped() > 0, "the drops must be counted, not silent");
    }

    #[test]
    fn the_newest_state_survives_and_reports_what_it_replaced() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("opened");

        for value in 0..(Outbox::<Topic>::DEPTH + 4) {
            outbox.emit(
                &mut channels,
                channel,
                Some(Topic::Containers),
                payload(u8::try_from(value % 256).expect("in range")),
            );
        }

        let held = outbox.drain(channel);
        let newest = held.last().expect("something is held");
        assert_eq!(
            newest.payload,
            payload(u8::try_from((Outbox::<Topic>::DEPTH + 3) % 256).expect("in range")),
            "a snapshot subscriber must end up with the newest state"
        );
        assert!(newest.superseded > 0, "and be told it is not seeing every state");
    }

    #[test]
    fn a_byte_stream_blocks_rather_than_dropping() {
        let mut channels = Channels::new();
        let mut outbox: Outbox<Topic> = Outbox::new();
        let channel = channels.open(Purpose::Stream).expect("opened");

        let mut blocked = false;
        for value in 0..1_000_u32 {
            if outbox.emit(&mut channels, channel, None, payload(value as u8)) == Emission::Blocked {
                blocked = true;
                break;
            }
        }

        assert!(
            blocked,
            "dropping bytes would corrupt the stream, so the producer stops"
        );
        assert!(outbox.len() <= Outbox::<Topic>::DEPTH);
        assert_eq!(outbox.dropped(), 0, "a stream never drops silently");
    }

    #[test]
    fn draining_frees_the_queue_for_more() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("opened");

        assert_eq!(
            outbox.emit(&mut channels, channel, Some(Topic::Containers), payload(1)),
            Emission::Queued
        );
        assert_eq!(outbox.depth(channel), 1);

        let drained = outbox.drain(channel);

        assert_eq!(drained.len(), 1);
        assert_eq!(outbox.depth(channel), 0);
    }

    #[test]
    fn separate_topics_do_not_supersede_each_other() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("opened");

        outbox.emit(&mut channels, channel, Some(Topic::Containers), payload(1));
        outbox.emit(&mut channels, channel, Some(Topic::Images), payload(2));

        let held = outbox.drain(channel);
        assert_eq!(held.len(), 2, "an image update must not replace a container update");
    }

    #[test]
    fn a_closing_channel_discards_what_it_held() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("opened");
        outbox.emit(&mut channels, channel, Some(Topic::Containers), payload(1));

        outbox.discard(channel);

        assert!(outbox.is_empty(), "a dead session must not retain its queue");
    }

    #[test]
    fn retired_channel_queues_cannot_grow_a_session_without_bound() {
        let mut channels = Channels::new();
        let mut outbox: Outbox<Topic> = Outbox::new();
        let mut blocked = 0;

        for value in 0..(Channels::LIMIT + 8) {
            let channel = channels.open(Purpose::Stream).expect("one live channel");
            for _ in 0..Outbox::<Topic>::DEPTH {
                if outbox.emit(&mut channels, channel, None, payload(value as u8)) == Emission::Blocked {
                    blocked += 1;
                }
            }
            // Deliberately model a faulty owner that omits `outbox.discard`.
            channels.close(channel).expect("closed");
        }

        assert_eq!(outbox.len(), Outbox::<Topic>::LIMIT);
        assert!(blocked > 0, "retired queues eventually apply global backpressure");
    }

    #[test]
    fn a_refused_enqueue_refunds_the_credit_it_could_not_use() {
        let mut channels = Channels::new();
        let mut outbox: Outbox<Topic> = Outbox::new();
        let channel = channels.open(Purpose::Stream).expect("opened");
        for value in 0..Outbox::<Topic>::DEPTH {
            assert_eq!(
                outbox.emit(&mut channels, channel, None, payload(value as u8)),
                Emission::Queued
            );
        }
        channels.replenish(channel, u32::MAX).expect("forged credit is clamped");

        assert_eq!(
            outbox.emit(&mut channels, channel, None, payload(99)),
            Emission::Blocked
        );
        assert_eq!(
            channels.credit(channel),
            Some(Channels::CREDIT),
            "a frame that was neither retained nor sent spends no credit"
        );
    }

    #[test]
    fn maximum_frames_across_channel_churn_obey_the_total_byte_budget() {
        let mut channels = Channels::new();
        let mut outbox: Outbox<Topic> = Outbox::new();
        let first = channels.open(Purpose::Stream).expect("first stream");
        let maximum = vec![b'x'; crate::Frame::PAYLOAD_LIMIT];

        assert_eq!(
            outbox.emit(&mut channels, first, None, maximum.clone()),
            Emission::Queued
        );
        assert_eq!(
            outbox.emit(&mut channels, first, None, maximum.clone()),
            Emission::Queued
        );
        assert_eq!(outbox.bytes(), Outbox::<Topic>::BYTE_LIMIT);
        channels.close(first).expect("retired without discarding its queue");

        let next = channels.open(Purpose::Stream).expect("replacement stream");
        assert_eq!(
            outbox.emit(&mut channels, next, None, maximum.clone()),
            Emission::Blocked,
            "retiring a channel alone cannot evade the session byte budget"
        );
        assert_eq!(channels.credit(next), Some(Channels::CREDIT));
        assert_eq!(outbox.bytes(), Outbox::<Topic>::BYTE_LIMIT);

        outbox.discard(first);
        assert_eq!(outbox.bytes(), 0, "discard releases the complete byte charge");
        assert_eq!(outbox.emit(&mut channels, next, None, maximum), Emission::Queued);
        assert_eq!(outbox.bytes(), crate::Frame::PAYLOAD_LIMIT);
    }

    #[test]
    fn coalescing_replaces_bytes_without_exceeding_the_session_budget() {
        let mut channels = Channels::new();
        let mut outbox = Outbox::new();
        let channel = channels.open(Purpose::Subscription).expect("subscription");
        let maximum = vec![b'x'; crate::Frame::PAYLOAD_LIMIT];

        assert_eq!(
            outbox.emit(&mut channels, channel, Some(Topic::Containers), maximum.clone()),
            Emission::Queued
        );
        assert_eq!(
            outbox.emit(&mut channels, channel, Some(Topic::Containers), maximum.clone()),
            Emission::Queued
        );
        for _ in 2..Channels::CREDIT {
            assert_eq!(
                outbox.emit(&mut channels, channel, Some(Topic::Containers), maximum.clone()),
                Emission::Superseded
            );
            assert_eq!(outbox.bytes(), Outbox::<Topic>::BYTE_LIMIT);
        }
        assert_eq!(channels.credit(channel), Some(0));

        assert_eq!(
            outbox.emit(&mut channels, channel, Some(Topic::Containers), payload(7)),
            Emission::Superseded
        );
        assert_eq!(outbox.bytes(), crate::Frame::PAYLOAD_LIMIT + 1);
        let held = outbox.drain(channel);
        assert_eq!(held.len(), 2);
        assert_eq!(held[1].payload, payload(7));
        assert_eq!(outbox.bytes(), 0);
    }
}
