use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_event::{
    PreparedSignalSelection, SignalInfo as EventSignalInfo, SignalMask as EventSignalMask, SignalObserver, SignalQueue,
    SignalQueueError, SignalSubscription,
};
use hl_task::{
    PendingTarget, SignalActivitySubscription, SignalActivityWake, SignalInfo, SignalMask, TaskRegistry, ThreadId,
};

struct ObserverState {
    active: bool,
    callbacks: usize,
}

struct ObserverEntry {
    observer: Arc<dyn SignalObserver>,
    state: Mutex<ObserverState>,
    quiescent: Condvar,
}

struct ActivityWake(Arc<ObserverEntry>);

impl SignalActivityWake for ActivityWake {
    fn signal_activity_changed(&self) {
        self.0.notify();
    }
}

impl ObserverEntry {
    fn notify(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.active {
                return;
            }
            state.callbacks += 1;
        }
        self.observer.signal_available();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.callbacks -= 1;
        if state.callbacks == 0 {
            self.quiescent.notify_all();
        }
    }

    fn quiesce(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = false;
        while state.callbacks != 0 {
            state = self
                .quiescent
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

struct ObserverRegistry {
    next: u64,
    closed: bool,
    entries: BTreeMap<u64, Arc<ObserverEntry>>,
}

struct QueueInner {
    tasks: Arc<TaskRegistry>,
    thread: ThreadId,
    observers: Mutex<ObserverRegistry>,
}

/// Cross-domain projection of one task thread's pending signals to signalfd.
pub struct TaskSignalQueue {
    inner: Arc<QueueInner>,
}

impl TaskSignalQueue {
    #[must_use]
    pub fn new(tasks: Arc<TaskRegistry>, thread: ThreadId) -> Self {
        Self {
            inner: Arc::new(QueueInner {
                tasks,
                thread,
                observers: Mutex::new(ObserverRegistry {
                    next: 1,
                    closed: false,
                    entries: BTreeMap::new(),
                }),
            }),
        }
    }

    pub fn enqueue(&self, target: PendingTarget, info: SignalInfo) -> Result<bool, hl_task::TaskError> {
        self.inner.tasks.enqueue_signal(target, info)
    }

    pub fn quiesce(&self) {
        let entries = {
            let mut registry = self
                .inner
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.closed = true;
            std::mem::take(&mut registry.entries)
        };
        for entry in entries.values() {
            entry.quiesce();
        }
    }

    fn task_mask(mask: EventSignalMask) -> SignalMask {
        SignalMask::from_bits(mask.bits())
    }

    fn event_info(info: SignalInfo) -> EventSignalInfo {
        EventSignalInfo {
            signal: u32::from(info.signal.get()),
            error: info.error,
            code: info.code,
            process_id: info.sender_process,
            user_id: info.sender_user,
            integer: info.value as i32,
            pointer: info.value,
            address: info.address,
            ..EventSignalInfo::default()
        }
    }

    fn actor_thread(&self, actor: hl_descriptor::OperationActor) -> Result<ThreadId, SignalQueueError> {
        let thread = ThreadId::from_wire(actor.thread, actor.thread_generation).ok_or(SignalQueueError::Failed)?;
        let process =
            hl_task::ProcessId::from_wire(actor.process, actor.process_generation).ok_or(SignalQueueError::Failed)?;
        let snapshot = self.inner.tasks.snapshot();
        let entry = snapshot
            .threads
            .iter()
            .find(|entry| entry.id == thread)
            .ok_or(SignalQueueError::Failed)?;
        if entry.process != process {
            return Err(SignalQueueError::Failed);
        }
        Ok(thread)
    }
}

impl std::fmt::Debug for TaskSignalQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TaskSignalQueue")
            .field("thread", &self.inner.thread)
            .finish_non_exhaustive()
    }
}

impl SignalQueue for TaskSignalQueue {
    fn dequeue(&self, mask: EventSignalMask) -> Result<Option<EventSignalInfo>, SignalQueueError> {
        self.inner
            .tasks
            .consume_signal_wait(self.inner.thread, Self::task_mask(mask))
            .map(|info| info.map(Self::event_info))
            .map_err(|_| SignalQueueError::Failed)
    }

    fn has_pending(&self, mask: EventSignalMask) -> bool {
        self.inner
            .tasks
            .has_signal_wait(self.inner.thread, Self::task_mask(mask))
            .unwrap_or(false)
    }

    fn subscribe(&self, observer: Arc<dyn SignalObserver>) -> Result<Box<dyn SignalSubscription>, SignalQueueError> {
        let entry = Arc::new(ObserverEntry {
            observer,
            state: Mutex::new(ObserverState {
                active: true,
                callbacks: 0,
            }),
            quiescent: Condvar::new(),
        });
        let token = {
            let mut registry = self
                .inner
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if registry.closed {
                return Err(SignalQueueError::Canceled);
            }
            let token = registry.next;
            registry.next = registry.next.wrapping_add(1).max(1);
            registry.entries.insert(token, Arc::clone(&entry));
            token
        };
        let wake = Arc::new(ActivityWake(Arc::clone(&entry)));
        let activity = self.inner.tasks.subscribe_signal_activity(wake);
        Ok(Box::new(TaskSignalSubscription {
            queue: Arc::downgrade(&self.inner),
            token,
            entry,
            _activity: activity,
        }))
    }

    fn prepare(&self, mask: EventSignalMask) -> Result<Option<Box<dyn PreparedSignalSelection>>, SignalQueueError> {
        self.inner
            .tasks
            .prepare_signal_wait(self.inner.thread, Self::task_mask(mask))
            .map(|value| {
                value.map(|prepared| {
                    Box::new(TaskPreparedSignal {
                        tasks: self.inner.tasks.clone(),
                        prepared,
                    }) as Box<dyn PreparedSignalSelection>
                })
            })
            .map_err(|_| SignalQueueError::Failed)
    }

    fn prepare_context(
        &self,
        mask: EventSignalMask,
        actor: hl_descriptor::OperationActor,
    ) -> Result<Option<Box<dyn PreparedSignalSelection>>, SignalQueueError> {
        let thread = self.actor_thread(actor)?;
        self.inner
            .tasks
            .prepare_signal_wait(thread, Self::task_mask(mask))
            .map(|value| {
                value.map(|prepared| {
                    Box::new(TaskPreparedSignal {
                        tasks: self.inner.tasks.clone(),
                        prepared,
                    }) as Box<dyn PreparedSignalSelection>
                })
            })
            .map_err(|_| SignalQueueError::Failed)
    }
}

struct TaskPreparedSignal {
    tasks: Arc<TaskRegistry>,
    prepared: hl_task::PreparedSignalWait,
}

impl PreparedSignalSelection for TaskPreparedSignal {
    fn info(&self) -> EventSignalInfo {
        TaskSignalQueue::event_info(self.prepared.info())
    }
    fn commit(self: Box<Self>) -> Result<bool, SignalQueueError> {
        self.tasks
            .commit_signal_wait(self.prepared)
            .map_err(|_| SignalQueueError::Failed)
    }
}

struct TaskSignalSubscription {
    queue: Weak<QueueInner>,
    token: u64,
    entry: Arc<ObserverEntry>,
    _activity: SignalActivitySubscription,
}

impl SignalSubscription for TaskSignalSubscription {
    fn quiesce(&self) {
        self.entry.quiesce();
        if let Some(queue) = self.queue.upgrade() {
            queue
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries
                .remove(&self.token);
        }
    }
}

impl Drop for TaskSignalSubscription {
    fn drop(&mut self) {
        self.quiesce();
    }
}
