use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::ProcessId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TraceLinkId {
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracePermission {
    Granted,
    Denied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceStop {
    Group(u32),
    Signal(u32),
    SyscallEntry,
    SyscallExit,
    Exec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceResume {
    Continue(Option<u32>),
    Syscall(Option<u32>),
    Detach(Option<u32>),
    Kill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceEvent {
    pub link: TraceLinkId,
    pub tracer: ProcessId,
    pub tracee: ProcessId,
    pub stop: TraceStop,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceWait {
    Event(TraceEvent),
    WouldBlock,
}

/// Names which end of the trace relationship failed to resolve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceSubject {
    Tracer(ProcessId),
    Tracee(ProcessId),
    Number(u32),
}

/// Distinguishes the lookup behind an unusable link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkFault {
    Untraced(ProcessId),
    Stale(TraceLinkId),
    Unqueued(TraceLinkId),
}

/// Names why an attach was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceDenial {
    Permission,
    SelfTrace,
    NoParent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceError {
    Capacity,
    Denied(TraceDenial),
    InvalidLink(LinkFault),
    InvalidProcess(TraceSubject),
    AlreadyTraced(ProcessId),
    NotStopped(TraceLinkId),
    AlreadyStopped(TraceLinkId),
    WrongTracer { expected: ProcessId, actual: ProcessId },
    InvalidSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceSnapshot {
    pub link: TraceLinkId,
    pub tracer: ProcessId,
    pub tracee: ProcessId,
    pub stopped: Option<TraceStop>,
    pub reported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceImage {
    pub version: u32,
    pub links: Vec<TraceSnapshot>,
    pub sequence: u64,
}

#[derive(Clone, Copy)]
struct Link {
    tracer: ProcessId,
    tracee: ProcessId,
    stopped: Option<TraceStop>,
    reported: bool,
    syscall_mode: bool,
    pending_attach: bool,
}

struct Slot {
    generation: u32,
    link: Option<Link>,
}

struct State {
    slots: Vec<Slot>,
    tracees: BTreeMap<ProcessId, TraceLinkId>,
    events: VecDeque<TraceEvent>,
    sequence: u64,
    commands: BTreeMap<TraceLinkId, TraceResume>,
}

pub(crate) struct Registry {
    state: Mutex<State>,
    ready: Condvar,
}

impl Registry {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                slots: (0..capacity)
                    .map(|_| Slot {
                        generation: 0,
                        link: None,
                    })
                    .collect(),
                tracees: BTreeMap::new(),
                events: VecDeque::new(),
                sequence: 1,
                commands: BTreeMap::new(),
            }),
            ready: Condvar::new(),
        }
    }

    pub(crate) fn attach(
        &self,
        tracer: ProcessId,
        tracee: ProcessId,
        permission: TracePermission,
        pending_attach: bool,
    ) -> Result<TraceLinkId, TraceError> {
        if permission == TracePermission::Denied {
            return Err(TraceError::Denied(TraceDenial::Permission));
        }
        if tracer == tracee {
            return Err(TraceError::Denied(TraceDenial::SelfTrace));
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.tracees.contains_key(&tracee) {
            return Err(TraceError::AlreadyTraced(tracee));
        }
        let (index, slot) = state
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.link.is_none() && slot.generation != u32::MAX)
            .ok_or(TraceError::Capacity)?;
        slot.generation += 1;
        let id = TraceLinkId {
            slot: index as u32,
            generation: slot.generation,
        };
        slot.link = Some(Link {
            tracer,
            tracee,
            stopped: None,
            reported: false,
            syscall_mode: false,
            pending_attach,
        });
        state.tracees.insert(tracee, id);
        Ok(id)
    }

    fn link_mut(state: &mut State, id: TraceLinkId) -> Result<&mut Link, TraceError> {
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(TraceError::InvalidLink(LinkFault::Stale(id)))?;
        if slot.generation != id.generation {
            return Err(TraceError::InvalidLink(LinkFault::Stale(id)));
        }
        slot.link.as_mut().ok_or(TraceError::InvalidLink(LinkFault::Stale(id)))
    }

    pub(crate) fn stop(&self, tracee: ProcessId, stop: TraceStop) -> Result<TraceEvent, TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = *state
            .tracees
            .get(&tracee)
            .ok_or(TraceError::InvalidLink(LinkFault::Untraced(tracee)))?;
        let tracer = {
            let link = Self::link_mut(&mut state, id)?;
            if link.stopped.is_some() {
                return Err(TraceError::AlreadyStopped(id));
            }
            link.stopped = Some(stop);
            link.reported = false;
            link.tracer
        };
        let sequence = state.sequence;
        state.sequence = state.sequence.wrapping_add(1).max(1);
        let event = TraceEvent {
            link: id,
            tracer,
            tracee,
            stop,
            sequence,
        };
        state.events.push_back(event);
        self.ready.notify_all();
        Ok(event)
    }

    pub(crate) fn wait(&self, tracer: ProcessId, tracee: Option<ProcessId>) -> Result<TraceWait, TraceError> {
        let event = self.peek(tracer, tracee)?;
        let TraceWait::Event(event) = event else {
            return Ok(event);
        };
        self.commit_wait(tracer, event)?;
        Ok(TraceWait::Event(event))
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn peek(&self, tracer: ProcessId, tracee: Option<ProcessId>) -> Result<TraceWait, TraceError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state
            .events
            .iter()
            .copied()
            .find(|event| event.tracer == tracer && tracee.is_none_or(|process| event.tracee == process))
            .map_or(TraceWait::WouldBlock, TraceWait::Event))
    }

    pub(crate) fn commit_wait(&self, tracer: ProcessId, selected: TraceEvent) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let position = state.events.iter().position(|event| {
            event.tracer == tracer && event.link == selected.link && event.sequence == selected.sequence
        });
        let Some(position) = position else {
            return Err(TraceError::InvalidLink(LinkFault::Unqueued(selected.link)));
        };
        let queued = state
            .events
            .remove(position)
            .ok_or(TraceError::InvalidLink(LinkFault::Unqueued(selected.link)))?;
        if queued != selected {
            return Err(TraceError::InvalidLink(LinkFault::Unqueued(selected.link)));
        }
        Self::link_mut(&mut state, selected.link)?.reported = true;
        Ok(())
    }

    pub(crate) fn link(
        &self,
        tracer: ProcessId,
        tracee: ProcessId,
        require_stop: bool,
    ) -> Result<TraceLinkId, TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = *state
            .tracees
            .get(&tracee)
            .ok_or(TraceError::InvalidLink(LinkFault::Untraced(tracee)))?;
        let link = Self::link_mut(&mut state, id)?;
        if link.tracer != tracer {
            return Err(TraceError::WrongTracer {
                expected: link.tracer,
                actual: tracer,
            });
        }
        if require_stop && link.stopped.is_none() {
            return Err(TraceError::NotStopped(id));
        }
        Ok(id)
    }

    pub(crate) fn resume(&self, tracer: ProcessId, id: TraceLinkId, command: TraceResume) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tracee = {
            let link = Self::link_mut(&mut state, id)?;
            if link.tracer != tracer {
                return Err(TraceError::WrongTracer {
                    expected: link.tracer,
                    actual: tracer,
                });
            }
            if link.stopped.is_none() || !link.reported {
                return Err(TraceError::NotStopped(id));
            }
            link.stopped = None;
            link.reported = false;
            link.syscall_mode = matches!(command, TraceResume::Syscall(_));
            link.tracee
        };
        state.events.retain(|event| event.link != id);
        state.commands.insert(id, command);
        if matches!(command, TraceResume::Detach(_)) {
            Self::remove(&mut state, id, tracee)?;
        }
        self.ready.notify_all();
        Ok(())
    }

    pub(crate) fn syscall_stop(&self, tracee: ProcessId, exit: bool) -> Result<Option<TraceEvent>, TraceError> {
        let stop = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = *state
                .tracees
                .get(&tracee)
                .ok_or(TraceError::InvalidLink(LinkFault::Untraced(tracee)))?;
            let link = Self::link_mut(&mut state, id)?;
            if link.pending_attach {
                link.pending_attach = false;
                Some(TraceStop::Group(19))
            } else if link.syscall_mode {
                Some(if exit {
                    TraceStop::SyscallExit
                } else {
                    TraceStop::SyscallEntry
                })
            } else {
                None
            }
        };
        stop.map(|stop| self.stop(tracee, stop)).transpose()
    }

    pub(crate) fn await_resume(&self, tracee: ProcessId, id: TraceLinkId) -> Result<TraceResume, TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(command) = state.commands.remove(&id) {
                return Ok(command);
            }
            if state.tracees.get(&tracee) != Some(&id) {
                return Err(TraceError::InvalidLink(LinkFault::Untraced(tracee)));
            }
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
        }
    }

    pub(crate) fn take_resume(&self, tracee: ProcessId, id: TraceLinkId) -> Result<Option<TraceResume>, TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(command) = state.commands.remove(&id) {
            return Ok(Some(command));
        }
        if state.tracees.get(&tracee) != Some(&id) {
            return Err(TraceError::InvalidLink(LinkFault::Untraced(tracee)));
        }
        Ok(None)
    }

    fn remove(state: &mut State, id: TraceLinkId, tracee: ProcessId) -> Result<(), TraceError> {
        state.tracees.remove(&tracee);
        state.events.retain(|event| event.link != id);
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(TraceError::InvalidLink(LinkFault::Stale(id)))?;
        if slot.generation != id.generation {
            return Err(TraceError::InvalidLink(LinkFault::Stale(id)));
        }
        slot.link = None;
        Ok(())
    }

    pub(crate) fn exit(&self, process: ProcessId) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed: Vec<_> = state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let link = slot.link?;
                (link.tracer == process || link.tracee == process).then_some((
                    TraceLinkId {
                        slot: index as u32,
                        generation: slot.generation,
                    },
                    link.tracee,
                ))
            })
            .collect();
        for (id, tracee) in removed {
            if Self::remove(&mut state, id, tracee).is_err() {
                break;
            }
        }
        self.ready.notify_all();
    }

    pub(crate) fn image(&self) -> TraceImage {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let links = state
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let link = slot.link?;
                Some(TraceSnapshot {
                    link: TraceLinkId {
                        slot: index as u32,
                        generation: slot.generation,
                    },
                    tracer: link.tracer,
                    tracee: link.tracee,
                    stopped: link.stopped,
                    reported: link.reported,
                })
            })
            .collect();
        TraceImage {
            version: 1,
            links,
            sequence: state.sequence,
        }
    }

    pub(crate) fn restore(&self, image: &TraceImage) -> Result<(), TraceError> {
        if image.version != 1 || image.sequence == 0 {
            return Err(TraceError::InvalidSnapshot);
        }
        let capacity = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slots
            .len();
        let mut restored = State {
            slots: Self::slots(capacity),
            tracees: BTreeMap::new(),
            events: VecDeque::new(),
            sequence: image.sequence,
            commands: BTreeMap::new(),
        };
        for saved in &image.links {
            let slot = restored
                .slots
                .get_mut(saved.link.slot as usize)
                .ok_or(TraceError::InvalidSnapshot)?;
            if saved.link.generation == 0 || slot.link.is_some() || restored.tracees.contains_key(&saved.tracee) {
                return Err(TraceError::InvalidSnapshot);
            }
            slot.generation = saved.link.generation;
            slot.link = Some(Link {
                tracer: saved.tracer,
                tracee: saved.tracee,
                stopped: saved.stopped,
                reported: saved.reported,
                syscall_mode: false,
                pending_attach: false,
            });
            restored.tracees.insert(saved.tracee, saved.link);
            if !saved.reported {
                let Some(stop) = saved.stopped else { continue };
                restored.events.push_back(TraceEvent {
                    link: saved.link,
                    tracer: saved.tracer,
                    tracee: saved.tracee,
                    stop,
                    sequence: restored.sequence,
                });
                restored.sequence = restored.sequence.wrapping_add(1).max(1);
            }
        }
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = restored;
        self.ready.notify_all();
        Ok(())
    }

    fn slots(capacity: usize) -> Vec<Slot> {
        (0..capacity)
            .map(|_| Slot {
                generation: 0,
                link: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use crate::{
        ExitStatus, LinkFault, ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry, TraceDenial,
        TraceError, TracePermission, TraceResume, TraceStop, TraceWait,
    };

    fn family() -> (TaskRegistry, crate::ProcessId, crate::ProcessId, crate::ProcessId) {
        let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (parent, thread) = registry
            .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
            .unwrap();
        let (first, _) = registry
            .commit_fork_process(registry.begin_fork_process(thread).unwrap())
            .unwrap();
        let (second, _) = registry
            .commit_fork_process(registry.begin_fork_process(thread).unwrap())
            .unwrap();
        (registry, parent, first, second)
    }

    #[test]
    fn stop_resume_detach() {
        let (registry, tracer, tracee, _) = family();
        let link = registry.trace_attach(tracer, tracee, TracePermission::Granted).unwrap();
        let event = registry.trace_stop(tracee, TraceStop::SyscallEntry).unwrap();
        assert_eq!(registry.trace_wait(tracer, Some(tracee)), Ok(TraceWait::Event(event)));
        assert_eq!(registry.trace_resume(tracer, link, TraceResume::Syscall(None)), Ok(()));
        assert_eq!(
            registry.trace_await_resume(tracee, link),
            Ok(TraceResume::Syscall(None))
        );
        let event = registry.trace_stop(tracee, TraceStop::Signal(10)).unwrap();
        assert_eq!(registry.trace_wait(tracer, None), Ok(TraceWait::Event(event)));
        registry
            .trace_resume(tracer, link, TraceResume::Detach(Some(12)))
            .unwrap();
        assert_eq!(
            registry.trace_await_resume(tracee, link),
            Ok(TraceResume::Detach(Some(12)))
        );
        assert_eq!(
            registry.trace_stop(tracee, TraceStop::Group(19)),
            Err(TraceError::InvalidLink(LinkFault::Untraced(tracee)))
        );
    }

    #[test]
    fn permissions_ownership() {
        let (registry, tracer, tracee, other) = family();
        assert_eq!(
            registry.trace_attach(tracer, tracee, TracePermission::Denied),
            Err(TraceError::Denied(TraceDenial::Permission)),
        );
        let link = registry.trace_attach(tracer, tracee, TracePermission::Granted).unwrap();
        assert_eq!(registry.trace_link(tracer, tracee, false), Ok(link));
        assert_eq!(
            registry.trace_link(tracer, tracee, true),
            Err(TraceError::NotStopped(link))
        );
        assert_eq!(
            registry.trace_attach(other, tracee, TracePermission::Granted),
            Err(TraceError::AlreadyTraced(tracee)),
        );
        registry.trace_stop(tracee, TraceStop::Signal(5)).unwrap();
        assert_eq!(registry.trace_link(tracer, tracee, true), Ok(link));
        assert_eq!(
            registry.trace_link(other, tracee, true),
            Err(TraceError::WrongTracer {
                expected: tracer,
                actual: other
            })
        );
        assert_eq!(registry.trace_wait(other, None), Ok(TraceWait::WouldBlock));
        assert!(matches!(registry.trace_wait(tracer, None), Ok(TraceWait::Event(_))));
        assert_eq!(
            registry.trace_resume(other, link, TraceResume::Continue(None)),
            Err(TraceError::WrongTracer {
                expected: tracer,
                actual: other
            }),
        );
    }

    #[test]
    fn exec_exit_cleanup() {
        let (registry, tracer, tracee, _) = family();
        let link = registry.trace_me(tracee).unwrap();
        registry.mark_exec(tracee).unwrap();
        let event = registry.trace_wait(tracer, Some(tracee)).unwrap();
        assert!(matches!(event, TraceWait::Event(value) if value.stop == TraceStop::Exec));
        registry
            .trace_resume(tracer, link, TraceResume::Continue(None))
            .unwrap();
        registry.exit_process(tracee, ExitStatus::Code(0)).unwrap();
        assert_eq!(
            registry.trace_stop(tracee, TraceStop::Signal(5)),
            Err(TraceError::InvalidLink(LinkFault::Untraced(tracee)))
        );
        assert!(registry.trace_image().links.is_empty());
    }

    #[test]
    fn exec_publishes_stop() {
        let (registry, tracer, tracee, _) = family();
        let registry = Arc::new(registry);
        registry.trace_me(tracee).unwrap();
        let thread = registry
            .snapshot()
            .processes
            .iter()
            .find(|process| process.id == tracee)
            .unwrap()
            .leader;
        let mut exec = registry.prepare_exec(tracee, thread).unwrap();

        exec.publish().unwrap();
        assert_eq!(registry.trace_wait(tracer, Some(tracee)), Ok(TraceWait::WouldBlock));
        exec.finish();

        assert!(matches!(
            registry.trace_wait(tracer, Some(tracee)),
            Ok(TraceWait::Event(event)) if event.stop == TraceStop::Exec,
        ));
    }

    #[test]
    fn checkpoint_snapshot() {
        let (registry, tracer, tracee, _) = family();
        let link = registry.trace_attach(tracer, tracee, TracePermission::Granted).unwrap();
        registry.trace_stop(tracee, TraceStop::Group(19)).unwrap();
        let image = registry.trace_image();
        assert_eq!(image.version, 1);
        assert_eq!(image.links.len(), 1);
        assert_eq!(image.links[0].link, link);
        assert_eq!(image.links[0].stopped, Some(TraceStop::Group(19)));
        let restored = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (restored_parent, restored_thread) = restored
            .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
            .unwrap();
        let (restored_child, _) = restored
            .commit_fork_process(restored.begin_fork_process(restored_thread).unwrap())
            .unwrap();
        assert_eq!((restored_parent, restored_child), (tracer, tracee));
        restored.restore_trace_image(&image).unwrap();
        assert!(matches!(restored.trace_wait(tracer, None), Ok(TraceWait::Event(_))));
    }

    #[test]
    fn prelink_wait() {
        let (registry, tracer, tracee, _) = family();
        let registry = Arc::new(registry);
        let barrier = Arc::new(Barrier::new(2));
        let waiter_registry = Arc::clone(&registry);
        let waiter_barrier = Arc::clone(&barrier);
        let waiter = std::thread::spawn(move || {
            let observed = waiter_registry.wait_observation();
            waiter_barrier.wait();
            waiter_registry.wait_change(observed);
            waiter_registry.trace_peek(tracer, Some(tracee))
        });
        barrier.wait();
        registry.trace_me(tracee).unwrap();
        let event = registry.trace_stop(tracee, TraceStop::Group(19)).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(TraceWait::Event(event)));
    }
}
