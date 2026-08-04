use std::sync::Arc;

use super::model::{INOTIFY_HEADER_SIZE, InotifyMask, WatchSourceEvent};
use super::{Inotify, InotifyInner, InotifyState, QueuedEvent};

impl Inotify {
    pub(crate) fn accept_source_event(inner: &Arc<InotifyInner>, event: WatchSourceEvent) {
        let (notify, remove) = {
            let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.retired {
                return;
            }
            Self::process_source_event(inner, &mut state, event)
        };
        if let Some(token) = remove {
            let _ = inner.source.remove(token);
        }
        if notify {
            inner.changed.notify_all();
            inner.readiness.notify();
        }
    }

    fn process_source_event(
        inner: &InotifyInner,
        state: &mut InotifyState,
        event: WatchSourceEvent,
    ) -> (bool, Option<u64>) {
        if event.mask.contains(InotifyMask::QUEUE_OVERFLOW) {
            return (Self::queue_overflow(inner, state), None);
        }
        let Some(index) = state.slots.iter().position(|slot| {
            slot.watch
                .as_ref()
                .is_some_and(|watch| watch.token == event.source_token)
        }) else {
            return (false, None);
        };
        let watch = state.slots[index]
            .watch
            .as_ref()
            .expect("position selected an active watch");
        if event.name.len() > inner.limits.name_bytes
            || event.unlinked_child && watch.mask.contains(InotifyMask::EXCLUDE_UNLINKED)
        {
            return (false, None);
        }
        let event_bits = event.mask.bits() & InotifyMask::EVENT_BITS;
        if event_bits != 0 && event_bits & watch.mask.bits() & InotifyMask::EVENT_BITS == 0 {
            return (false, None);
        }
        Self::queue_matching_event(inner, state, index, event)
    }

    fn queue_matching_event(
        inner: &InotifyInner,
        state: &mut InotifyState,
        index: usize,
        event: WatchSourceEvent,
    ) -> (bool, Option<u64>) {
        let watch = state.slots[index].watch.as_ref().expect("matched watch remains active");
        let descriptor = i32::try_from(index + 1).unwrap();
        let terminal = watch.mask.contains(InotifyMask::ONESHOT)
            || event.mask.contains(InotifyMask::IGNORED)
            || event.mask.contains(InotifyMask::DELETE_SELF)
            || event.mask.contains(InotifyMask::UNMOUNT);
        let token = watch.token;
        let mut notify = Self::queue_event(
            inner,
            state,
            QueuedEvent {
                watch_descriptor: descriptor,
                mask: event.mask,
                cookie: event.cookie,
                name: event.name,
            },
        );
        if !terminal {
            return (notify, None);
        }
        state.slots[index].watch = None;
        if event.mask.contains(InotifyMask::IGNORED) {
            return (notify, None);
        }
        notify |= Self::queue_event(
            inner,
            state,
            QueuedEvent {
                watch_descriptor: descriptor,
                mask: InotifyMask::from_bits(InotifyMask::IGNORED),
                cookie: 0,
                name: Vec::new(),
            },
        );
        (notify, Some(token))
    }

    pub(crate) fn queue_event(inner: &InotifyInner, state: &mut InotifyState, event: QueuedEvent) -> bool {
        let encoded = event.encoded_len();
        let regular_event_limit = inner.limits.queued_events - 1;
        let regular_byte_limit = inner.limits.queued_bytes - INOTIFY_HEADER_SIZE;
        if state.queue.len() >= regular_event_limit || encoded > regular_byte_limit.saturating_sub(state.queue_bytes) {
            return Self::queue_overflow(inner, state);
        }
        let was_empty = state.queue.is_empty();
        state.queue_bytes += encoded;
        state.queue.push_back(event);
        was_empty
    }

    fn queue_overflow(inner: &InotifyInner, state: &mut InotifyState) -> bool {
        if state.overflow_queued
            || state.queue.len() == inner.limits.queued_events
            || state.queue_bytes + INOTIFY_HEADER_SIZE > inner.limits.queued_bytes
        {
            return false;
        }
        let was_empty = state.queue.is_empty();
        state.queue.push_back(QueuedEvent {
            watch_descriptor: -1,
            mask: InotifyMask::from_bits(InotifyMask::QUEUE_OVERFLOW),
            cookie: 0,
            name: Vec::new(),
        });
        state.queue_bytes += INOTIFY_HEADER_SIZE;
        state.overflow_queued = true;
        was_empty
    }
}
