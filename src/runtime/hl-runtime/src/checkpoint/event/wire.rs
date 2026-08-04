use hl_descriptor::{DescriptionIdentity, Readiness};
use hl_event::{
    EVENT_CHECKPOINT_OBJECT_MAXIMUM, EpollInterest, EpollSnapshot, EpollTargetCheckpoint, EpollWatchKey,
    EpollWatchSnapshot, EventCheckpointImage, EventObjectCheckpoint, EventObjectId, EventObjectState, EventResourceKey,
    InotifyEventSnapshot, InotifyLimits, InotifyMask, InotifySnapshot, InotifyWatchCheckpoint, InotifyWatchSnapshot,
    SignalFdSnapshot, SignalMask, TimerFdClock, TimerFdSnapshot, WatchBinding, WatchNodeIdentity, WatchPathIdentity,
};

use super::CheckpointCodec;

const WIRE_VERSION: u32 = 2;
const WIRE_BYTES_MAXIMUM: usize = 4 * 1024 * 1024;

/// Durable, bounded little-endian codec for the event-domain checkpoint image.
#[derive(Clone, Copy, Debug, Default)]
pub struct WireCodec;

impl CheckpointCodec for WireCodec {
    fn encode(&self, image: &EventCheckpointImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        let mut output = Output::default();
        output.u32(WIRE_VERSION)?;
        output.u32(image.version)?;
        output.count(image.generations.len())?;
        output.count(image.objects.len())?;
        for generation in &image.generations {
            output.u32(*generation)?;
        }
        for object in &image.objects {
            output.object(object)?;
        }
        Ok(output.bytes)
    }

    fn decode(&self, bytes: &[u8]) -> Result<EventCheckpointImage, ()> {
        if bytes.len() > WIRE_BYTES_MAXIMUM {
            return Err(());
        }
        let mut input = Input { bytes, offset: 0 };
        if input.u32()? != WIRE_VERSION {
            return Err(());
        }
        let version = input.u32()?;
        let generation_count = input.count()?;
        let object_count = input.count()?;
        let mut generations = Vec::with_capacity(generation_count);
        for _ in 0..generation_count {
            generations.push(input.u32()?);
        }
        let mut objects = Vec::with_capacity(object_count);
        for _ in 0..object_count {
            objects.push(input.object()?);
        }
        if input.offset != bytes.len() {
            return Err(());
        }
        let image = EventCheckpointImage {
            version,
            generations,
            objects,
        };
        image.validate().map_err(|_| ())?;
        Ok(image)
    }
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
}

impl Output {
    fn extend(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let length = self.bytes.len().checked_add(bytes.len()).ok_or(())?;
        if length > WIRE_BYTES_MAXIMUM {
            return Err(());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), ()> {
        self.extend(&[value])
    }
    fn bool(&mut self, value: bool) -> Result<(), ()> {
        self.u8(u8::from(value))
    }
    fn u32(&mut self, value: u32) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    fn i32(&mut self, value: i32) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    fn u64(&mut self, value: u64) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }

    fn count(&mut self, value: usize) -> Result<(), ()> {
        if value > EVENT_CHECKPOINT_OBJECT_MAXIMUM {
            return Err(());
        }
        self.u32(u32::try_from(value).map_err(|_| ())?)
    }

    fn option_u64(&mut self, value: Option<u64>) -> Result<(), ()> {
        self.bool(value.is_some())?;
        if let Some(value) = value {
            self.u64(value)?;
        }
        Ok(())
    }

    fn resource(&mut self, key: EventResourceKey) -> Result<(), ()> {
        self.u64(key.value())
    }

    fn object_id(&mut self, id: EventObjectId) -> Result<(), ()> {
        self.u32(id.slot)?;
        self.u32(id.generation)
    }

    fn object(&mut self, object: &EventObjectCheckpoint) -> Result<(), ()> {
        self.object_id(object.id)?;
        match &object.state {
            EventObjectState::EventFd(snapshot) => {
                self.u8(1)?;
                self.u64(snapshot.counter)?;
                self.bool(snapshot.semaphore)?;
                self.bool(snapshot.nonblocking)
            }
            EventObjectState::TimerFd { snapshot, clock } => {
                self.u8(2)?;
                self.timer(snapshot)?;
                self.resource(*clock)
            }
            EventObjectState::SignalFd { snapshot, task_queue } => {
                self.u8(3)?;
                self.u64(snapshot.mask.bits())?;
                self.bool(snapshot.nonblocking)?;
                self.resource(*task_queue)
            }
            EventObjectState::Epoll { snapshot, targets } => {
                self.u8(4)?;
                self.epoll(snapshot)?;
                self.count(targets.len())?;
                for target in targets {
                    self.epoll_target(target)?;
                }
                Ok(())
            }
            EventObjectState::Inotify {
                snapshot,
                source,
                watches,
            } => {
                self.u8(5)?;
                self.inotify(snapshot)?;
                self.resource(*source)?;
                self.count(watches.len())?;
                for watch in watches {
                    self.count(watch.watch)?;
                    self.resource(watch.source)?;
                }
                Ok(())
            }
        }
    }

    fn timer(&mut self, timer: &TimerFdSnapshot) -> Result<(), ()> {
        self.i32(timer.clock as i32)?;
        self.option_u64(timer.deadline_nanoseconds)?;
        self.u64(timer.interval_nanoseconds)?;
        self.u64(timer.pending_expirations)?;
        self.bool(timer.nonblocking)?;
        self.bool(timer.absolute_realtime)?;
        self.option_u64(timer.cancel_generation)?;
        self.bool(timer.canceled)
    }

    fn watch_key(&mut self, key: EpollWatchKey) -> Result<(), ()> {
        self.i32(key.descriptor_number)?;
        self.u32(key.descriptor_generation)?;
        self.u64(key.description.identity)?;
        self.u32(key.description.generation)
    }

    fn epoll(&mut self, epoll: &EpollSnapshot) -> Result<(), ()> {
        self.count(epoll.watch_limit)?;
        self.u64(epoll.next_token)?;
        self.u64(epoll.epoch)?;
        self.count(epoll.watches.len())?;
        for watch in &epoll.watches {
            self.watch_key(watch.key)?;
            self.u32(watch.interests.bits())?;
            self.u64(watch.data)?;
            self.u32(watch.previous.bits())?;
            self.bool(watch.disabled)?;
        }
        self.count(epoll.ready.len())?;
        for key in &epoll.ready {
            self.watch_key(*key)?;
        }
        Ok(())
    }

    fn epoll_target(&mut self, target: &EpollTargetCheckpoint) -> Result<(), ()> {
        self.count(target.watch)?;
        self.resource(target.descriptor)?;
        self.bool(target.nested.is_some())?;
        if let Some(id) = target.nested {
            self.object_id(id)?;
        }
        Ok(())
    }

    fn inotify(&mut self, value: &InotifySnapshot) -> Result<(), ()> {
        self.count(value.limits.watches)?;
        self.count(value.limits.queued_events)?;
        self.count(value.limits.queued_bytes)?;
        self.count(value.limits.name_bytes)?;
        self.bool(value.nonblocking)?;
        self.u32(value.next_cookie)?;
        self.bool(value.overflow_queued)?;
        self.count(value.watch_generations.len())?;
        for generation in &value.watch_generations {
            self.u32(*generation)?;
        }
        self.count(value.watches.len())?;
        for watch in &value.watches {
            self.inotify_watch(watch)?;
        }
        self.count(value.queue.len())?;
        for event in &value.queue {
            self.i32(event.watch_descriptor)?;
            self.u32(event.mask.bits())?;
            self.u32(event.cookie)?;
            self.count(event.name.len())?;
            self.extend(&event.name)?;
        }
        Ok(())
    }

    fn inotify_watch(&mut self, watch: &InotifyWatchSnapshot) -> Result<(), ()> {
        self.i32(watch.watch_descriptor)?;
        self.u32(watch.generation)?;
        self.u64(watch.binding.node.device)?;
        self.u64(watch.binding.node.object)?;
        self.u64(watch.binding.path.0)?;
        self.bool(watch.binding.is_directory)?;
        self.u32(watch.mask.bits())
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Input<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], ()> {
        let end = self.offset.checked_add(length).ok_or(())?;
        let bytes = self.bytes.get(self.offset..end).ok_or(())?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, ()> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(()),
        }
    }
    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn i32(&mut self) -> Result<i32, ()> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }

    fn count(&mut self) -> Result<usize, ()> {
        let count = usize::try_from(self.u32()?).map_err(|_| ())?;
        if count > EVENT_CHECKPOINT_OBJECT_MAXIMUM {
            return Err(());
        }
        Ok(count)
    }

    fn option_u64(&mut self) -> Result<Option<u64>, ()> {
        if self.bool()? { Ok(Some(self.u64()?)) } else { Ok(None) }
    }

    fn resource(&mut self) -> Result<EventResourceKey, ()> {
        EventResourceKey::new(self.u64()?).ok_or(())
    }

    fn object_id(&mut self) -> Result<EventObjectId, ()> {
        Ok(EventObjectId {
            slot: self.u32()?,
            generation: self.u32()?,
        })
    }

    fn object(&mut self) -> Result<EventObjectCheckpoint, ()> {
        let id = self.object_id()?;
        let state = match self.u8()? {
            1 => EventObjectState::EventFd(hl_event::EventFdSnapshot {
                counter: self.u64()?,
                semaphore: self.bool()?,
                nonblocking: self.bool()?,
            }),
            2 => EventObjectState::TimerFd {
                snapshot: self.timer()?,
                clock: self.resource()?,
            },
            3 => EventObjectState::SignalFd {
                snapshot: SignalFdSnapshot {
                    mask: SignalMask::from_bits(self.u64()?),
                    nonblocking: self.bool()?,
                },
                task_queue: self.resource()?,
            },
            4 => {
                let snapshot = self.epoll()?;
                let count = self.count()?;
                let mut targets = Vec::with_capacity(count);
                for _ in 0..count {
                    targets.push(self.epoll_target()?);
                }
                EventObjectState::Epoll { snapshot, targets }
            }
            5 => {
                let snapshot = self.inotify()?;
                let source = self.resource()?;
                let count = self.count()?;
                let mut watches = Vec::with_capacity(count);
                for _ in 0..count {
                    watches.push(InotifyWatchCheckpoint {
                        watch: self.count()?,
                        source: self.resource()?,
                    });
                }
                EventObjectState::Inotify {
                    snapshot,
                    source,
                    watches,
                }
            }
            _ => return Err(()),
        };
        Ok(EventObjectCheckpoint { id, state })
    }

    fn clock(&mut self) -> Result<TimerFdClock, ()> {
        TimerFdClock::from_linux_id(self.i32()?).ok_or(())
    }

    fn timer(&mut self) -> Result<TimerFdSnapshot, ()> {
        Ok(TimerFdSnapshot {
            clock: self.clock()?,
            deadline_nanoseconds: self.option_u64()?,
            interval_nanoseconds: self.u64()?,
            pending_expirations: self.u64()?,
            nonblocking: self.bool()?,
            absolute_realtime: self.bool()?,
            cancel_generation: self.option_u64()?,
            canceled: self.bool()?,
        })
    }

    fn watch_key(&mut self) -> Result<EpollWatchKey, ()> {
        Ok(EpollWatchKey {
            descriptor_number: self.i32()?,
            descriptor_generation: self.u32()?,
            description: DescriptionIdentity {
                identity: self.u64()?,
                generation: self.u32()?,
            },
        })
    }

    fn epoll(&mut self) -> Result<EpollSnapshot, ()> {
        let watch_limit = self.count()?;
        let next_token = self.u64()?;
        let epoch = self.u64()?;
        let count = self.count()?;
        let mut watches = Vec::with_capacity(count);
        for _ in 0..count {
            watches.push(EpollWatchSnapshot {
                key: self.watch_key()?,
                interests: EpollInterest::from_bits(self.u32()?),
                data: self.u64()?,
                previous: Readiness::from_bits(self.u32()?),
                disabled: self.bool()?,
            });
        }
        let count = self.count()?;
        let mut ready = Vec::with_capacity(count);
        for _ in 0..count {
            ready.push(self.watch_key()?);
        }
        Ok(EpollSnapshot {
            watch_limit,
            next_token,
            epoch,
            watches,
            ready,
        })
    }

    fn epoll_target(&mut self) -> Result<EpollTargetCheckpoint, ()> {
        Ok(EpollTargetCheckpoint {
            watch: self.count()?,
            descriptor: self.resource()?,
            nested: if self.bool()? { Some(self.object_id()?) } else { None },
        })
    }

    fn inotify(&mut self) -> Result<InotifySnapshot, ()> {
        let limits = InotifyLimits {
            watches: self.count()?,
            queued_events: self.count()?,
            queued_bytes: self.count()?,
            name_bytes: self.count()?,
        };
        let nonblocking = self.bool()?;
        let next_cookie = self.u32()?;
        let overflow_queued = self.bool()?;
        let count = self.count()?;
        let mut watch_generations = Vec::with_capacity(count);
        for _ in 0..count {
            watch_generations.push(self.u32()?);
        }
        let count = self.count()?;
        let mut watches = Vec::with_capacity(count);
        for _ in 0..count {
            watches.push(self.inotify_watch()?);
        }
        let count = self.count()?;
        let mut queue = Vec::with_capacity(count);
        for _ in 0..count {
            let watch_descriptor = self.i32()?;
            let mask = InotifyMask::from_bits(self.u32()?);
            let cookie = self.u32()?;
            let name_length = self.count()?;
            let name = self.take(name_length)?.to_vec();
            queue.push(InotifyEventSnapshot {
                watch_descriptor,
                mask,
                cookie,
                name,
            });
        }
        Ok(InotifySnapshot {
            limits,
            nonblocking,
            next_cookie,
            overflow_queued,
            watch_generations,
            watches,
            queue,
        })
    }

    fn inotify_watch(&mut self) -> Result<InotifyWatchSnapshot, ()> {
        Ok(InotifyWatchSnapshot {
            watch_descriptor: self.i32()?,
            generation: self.u32()?,
            binding: WatchBinding {
                node: WatchNodeIdentity {
                    device: self.u64()?,
                    object: self.u64()?,
                },
                path: WatchPathIdentity(self.u64()?),
                is_directory: self.bool()?,
            },
            mask: InotifyMask::from_bits(self.u32()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use hl_event::{
        EVENT_CHECKPOINT_VERSION, EpollInterest, EpollSnapshot, EpollTargetCheckpoint, EpollWatchKey,
        EpollWatchSnapshot, EventCheckpointImage, EventFdSnapshot, EventObjectCheckpoint, EventObjectId,
        EventObjectState, EventResourceKey,
    };

    use super::{CheckpointCodec, WireCodec};

    fn image() -> EventCheckpointImage {
        let key = EpollWatchKey {
            descriptor_number: 7,
            descriptor_generation: 3,
            description: hl_descriptor::DescriptionIdentity {
                identity: 41,
                generation: 2,
            },
        };
        EventCheckpointImage {
            version: EVENT_CHECKPOINT_VERSION,
            generations: vec![1, 1],
            objects: vec![
                EventObjectCheckpoint {
                    id: EventObjectId { slot: 0, generation: 1 },
                    state: EventObjectState::EventFd(EventFdSnapshot {
                        counter: 9,
                        semaphore: true,
                        nonblocking: true,
                    }),
                },
                EventObjectCheckpoint {
                    id: EventObjectId { slot: 1, generation: 1 },
                    state: EventObjectState::Epoll {
                        snapshot: EpollSnapshot {
                            watch_limit: 4,
                            next_token: 2,
                            epoch: 11,
                            watches: vec![EpollWatchSnapshot {
                                key,
                                interests: EpollInterest::from_bits(EpollInterest::READ | EpollInterest::ONESHOT),
                                data: 0xfeed,
                                previous: hl_descriptor::Readiness::from_bits(hl_descriptor::Readiness::READ),
                                disabled: true,
                            }],
                            ready: vec![key],
                        },
                        targets: vec![EpollTargetCheckpoint {
                            watch: 0,
                            descriptor: EventResourceKey::new(17).unwrap(),
                            nested: None,
                        }],
                    },
                },
            ],
        }
    }

    #[test]
    fn epoll_round_trip() {
        let codec = WireCodec;
        let image = image();
        let bytes = codec.encode(&image).unwrap();
        assert_eq!(codec.decode(&bytes), Ok(image));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let codec = WireCodec;
        let mut bytes = codec.encode(&image()).unwrap();
        bytes.push(0);
        assert!(codec.decode(&bytes).is_err());
    }

    #[test]
    fn rejects_image_v1() {
        let codec = WireCodec;
        let mut bytes = codec.encode(&image()).unwrap();
        bytes[4..8].copy_from_slice(&1_u32.to_le_bytes());
        assert!(codec.decode(&bytes).is_err());
    }

    #[test]
    fn rejects_wire_v1() {
        let codec = WireCodec;
        let mut bytes = codec.encode(&image()).unwrap();
        bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
        assert!(codec.decode(&bytes).is_err());
    }
}
