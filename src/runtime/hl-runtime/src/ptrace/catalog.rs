use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::cpu::{StoppedRegisterImage, TraceRegisterError, TraceSafepointPort};
use hl_linux::{GuestMemory, PtraceOptions};
use hl_task::{LinkFault, ProcessId, TraceError, TraceEvent, TraceLinkId, TracePermission, TraceStop, TraceSubject};

use super::PtracePort;
use super::{RuntimeSafepoint, TraceWake};

pub struct TraceExchange {
    image: Mutex<Option<StoppedRegisterImage>>,
    memory: Arc<dyn GuestMemory + Send + Sync>,
}

impl TraceExchange {
    #[must_use]
    pub fn new(memory: Arc<dyn GuestMemory + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            image: Mutex::new(None),
            memory,
        })
    }

    fn current(&self, link: TraceLinkId) -> Result<StoppedRegisterImage, TraceError> {
        self.image
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(TraceError::NotStopped(link))
    }

    fn replace(&self, image: StoppedRegisterImage) {
        *self.image.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(image);
    }
}

impl TraceSafepointPort for TraceExchange {
    fn publish(&self, image: StoppedRegisterImage) -> Result<(), TraceRegisterError> {
        self.replace(image);
        Ok(())
    }

    fn restore(&self) -> Result<StoppedRegisterImage, TraceRegisterError> {
        self.image
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(TraceRegisterError::Architecture)
    }
}

struct Link {
    tracee: ProcessId,
    options: PtraceOptions,
    event_message: u64,
}

#[derive(Default)]
struct State {
    processes: BTreeMap<ProcessId, Arc<TraceExchange>>,
    links: BTreeMap<TraceLinkId, Link>,
    wakes: BTreeMap<ProcessId, Arc<dyn TraceWake>>,
}

#[derive(Default)]
pub struct Catalog {
    state: Mutex<State>,
}

impl Catalog {
    pub fn register(&self, process: ProcessId, exchange: Arc<TraceExchange>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .processes
            .insert(process, exchange);
    }

    pub fn safepoint(&self, tasks: Arc<hl_task::TaskRegistry>, process: ProcessId) -> Option<Arc<RuntimeSafepoint>> {
        let exchange = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .processes
            .get(&process)
            .cloned()?;
        Some(Arc::new(RuntimeSafepoint::new(tasks, process, exchange)))
    }

    pub fn register_wake(&self, process: ProcessId, wake: Arc<dyn TraceWake>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wakes
            .insert(process, wake);
    }

    /// Retires every process-scoped ptrace projection after terminal task
    /// cleanup. In particular, the exchange owns the tracee's guest memory;
    /// retaining it would keep the complete address space alive after reap.
    pub fn unregister(&self, process: ProcessId) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.processes.remove(&process);
        state.wakes.remove(&process);
        state.links.retain(|_, link| link.tracee != process);
    }

    fn exchange(&self, link: TraceLinkId) -> Result<Arc<TraceExchange>, TraceError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let process = state
            .links
            .get(&link)
            .ok_or(TraceError::InvalidLink(LinkFault::Stale(link)))?
            .tracee;
        state
            .processes
            .get(&process)
            .cloned()
            .ok_or(TraceError::InvalidProcess(TraceSubject::Tracee(process)))
    }
}

impl PtracePort for Catalog {
    fn attached(&self, link: TraceLinkId, tracee: ProcessId) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.links.insert(
            link,
            Link {
                tracee,
                options: PtraceOptions::from_bits(0),
                event_message: 0,
            },
        );
        Ok(())
    }

    fn permission(&self, _: ProcessId, tracee: ProcessId) -> TracePermission {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.processes.contains_key(&tracee) {
            TracePermission::Granted
        } else {
            TracePermission::Denied
        }
    }

    fn registers(&self, link: TraceLinkId) -> Result<StoppedRegisterImage, TraceError> {
        self.exchange(link)?.current(link)
    }

    fn set_registers(&self, link: TraceLinkId, image: StoppedRegisterImage) -> Result<(), TraceError> {
        self.exchange(link)?.replace(image);
        Ok(())
    }

    fn options(&self, link: TraceLinkId, options: PtraceOptions) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .links
            .get_mut(&link)
            .ok_or(TraceError::InvalidLink(LinkFault::Stale(link)))?
            .options = options;
        Ok(())
    }

    fn event_message(&self, link: TraceLinkId) -> Result<u64, TraceError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state
            .links
            .get(&link)
            .ok_or(TraceError::InvalidLink(LinkFault::Stale(link)))?
            .event_message)
    }

    fn read(&self, link: TraceLinkId, address: u64, bytes: &mut [u8]) -> Result<(), TraceError> {
        let exchange = self.exchange(link)?;
        match exchange.memory.read(address, bytes) {
            Ok(count) if count == bytes.len() => Ok(()),
            _ => Err(TraceError::InvalidSnapshot),
        }
    }

    fn write(&self, link: TraceLinkId, address: u64, bytes: &[u8]) -> Result<(), TraceError> {
        let exchange = self.exchange(link)?;
        match exchange.memory.write(address, bytes) {
            Ok(count) if count == bytes.len() => Ok(()),
            _ => Err(TraceError::InvalidSnapshot),
        }
    }

    fn wait_status(&self, event: TraceEvent) -> u32 {
        let options = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .links
            .get(&event.link)
            .map_or_else(|| PtraceOptions::from_bits(0), |link| link.options);
        match event.stop {
            TraceStop::Group(signal) | TraceStop::Signal(signal) => (signal << 8) | 0x7f,
            TraceStop::SyscallEntry | TraceStop::SyscallExit => {
                let signal = if options.traces_syscalls() { 5 | 0x80 } else { 5 };
                (signal << 8) | 0x7f
            }
            TraceStop::Exec if options.traces_exec() => (((4 << 8) | 5) << 8) | 0x7f,
            TraceStop::Exec => (5 << 8) | 0x7f,
        }
    }

    fn resumed(&self, link: TraceLinkId) {
        let wake = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .links
                .get(&link)
                .and_then(|link| state.wakes.get(&link.tracee))
                .cloned()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}
