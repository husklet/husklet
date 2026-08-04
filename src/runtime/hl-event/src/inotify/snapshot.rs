use std::collections::VecDeque;
use std::sync::Arc;

use super::{INOTIFY_HEADER_SIZE, Inotify, InotifyError, InotifySnapshot, QueuedEvent, Watch, WatchSlot, WatchSource};

impl Inotify {
    pub fn from_snapshot(snapshot: &InotifySnapshot, source: Arc<dyn WatchSource>) -> Result<Self, InotifyError> {
        if snapshot.limits.watches == 0
            || snapshot.limits.queued_events < 2
            || snapshot.limits.queued_bytes < INOTIFY_HEADER_SIZE * 2
            || snapshot.limits.name_bytes == 0
            || snapshot.watch_generations.len() > snapshot.limits.watches
            || snapshot.watches.len() > snapshot.limits.watches
            || snapshot.queue.len() > snapshot.limits.queued_events
        {
            return Err(InotifyError::InvalidArgument);
        }
        let object = Self::new(snapshot.nonblocking, snapshot.limits, source)?;
        let mut installed = Vec::new();
        for saved in &snapshot.watches {
            let index = saved
                .watch_descriptor
                .checked_sub(1)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(InotifyError::InvalidArgument)?;
            if index >= snapshot.watch_generations.len()
                || snapshot.watch_generations[index] != saved.generation
                || saved.generation == 0
            {
                object.retire_inner();
                return Err(InotifyError::InvalidArgument);
            }
            let token = (u64::from(saved.generation) << 32)
                | u64::try_from(index + 1).map_err(|_| InotifyError::ResourceLimit)?;
            if let Err(error) = object.inner.source.add(saved.binding, token, saved.mask) {
                object.retire_inner();
                return Err(error.into());
            }
            installed.push((index, token, saved));
        }
        let mut queue_bytes = 0_usize;
        let queue = snapshot
            .queue
            .iter()
            .map(|saved| {
                if saved.name.len() > snapshot.limits.name_bytes {
                    return Err(InotifyError::InvalidArgument);
                }
                let event = QueuedEvent {
                    watch_descriptor: saved.watch_descriptor,
                    mask: saved.mask,
                    cookie: saved.cookie,
                    name: saved.name.clone(),
                };
                queue_bytes = queue_bytes
                    .checked_add(event.encoded_len())
                    .ok_or(InotifyError::ResourceLimit)?;
                Ok(event)
            })
            .collect::<Result<VecDeque<_>, _>>();
        let queue = match queue {
            Ok(queue) => queue,
            Err(error) => {
                object.retire_inner();
                return Err(error);
            }
        };
        if queue_bytes > snapshot.limits.queued_bytes {
            object.retire_inner();
            return Err(InotifyError::ResourceLimit);
        }
        let mut slots = snapshot
            .watch_generations
            .iter()
            .copied()
            .map(|generation| WatchSlot {
                generation,
                watch: None,
            })
            .collect::<Vec<_>>();
        for (index, token, saved) in installed {
            if slots[index].watch.is_some() {
                object.retire_inner();
                return Err(InotifyError::InvalidArgument);
            }
            slots[index].watch = Some(Watch {
                binding: saved.binding,
                mask: saved.mask,
                token,
            });
        }
        let mut state = object.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        state.slots = slots;
        state.queue = queue;
        state.queue_bytes = queue_bytes;
        state.overflow_queued = snapshot.overflow_queued;
        state.next_cookie = snapshot.next_cookie.max(1);
        drop(state);
        Ok(object)
    }
}
