use crate::{
    ProcessId, ProcessLifecycle, TraceError, TraceEvent, TraceImage, TraceLinkId, TracePermission, TraceResume,
    TraceStop, TraceWait,
};

use super::TaskRegistry;

impl TaskRegistry {
    pub fn trace_attach(
        &self,
        tracer: ProcessId,
        tracee: ProcessId,
        permission: TracePermission,
    ) -> Result<TraceLinkId, TraceError> {
        let state = self.lock();
        let tracer_state = Self::process(&state, tracer).map_err(|_| TraceError::InvalidProcess)?;
        let tracee_state = Self::process(&state, tracee).map_err(|_| TraceError::InvalidProcess)?;
        if tracer_state.lifecycle != ProcessLifecycle::Running || tracee_state.lifecycle != ProcessLifecycle::Running {
            return Err(TraceError::InvalidProcess);
        }
        drop(state);
        self.traces.attach(tracer, tracee, permission, true)
    }

    pub fn trace_me(&self, tracee: ProcessId) -> Result<TraceLinkId, TraceError> {
        let state = self.lock();
        let tracer = Self::process(&state, tracee)
            .map_err(|_| TraceError::InvalidProcess)?
            .parent
            .ok_or(TraceError::Denied)?;
        drop(state);
        self.traces.attach(tracer, tracee, TracePermission::Granted, false)
    }

    pub fn trace_seize(
        &self,
        tracer: ProcessId,
        tracee: ProcessId,
        permission: TracePermission,
    ) -> Result<TraceLinkId, TraceError> {
        let state = self.lock();
        Self::process(&state, tracer).map_err(|_| TraceError::InvalidProcess)?;
        Self::process(&state, tracee).map_err(|_| TraceError::InvalidProcess)?;
        drop(state);
        self.traces.attach(tracer, tracee, permission, false)
    }

    pub fn trace_syscall_stop(&self, tracee: ProcessId, exit: bool) -> Result<Option<TraceEvent>, TraceError> {
        let event = self.traces.syscall_stop(tracee, exit)?;
        if event.is_some() {
            let mut state = self.lock();
            state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
            drop(state);
            self.child_ready.notify_all();
        }
        Ok(event)
    }

    pub fn trace_stop(&self, tracee: ProcessId, stop: TraceStop) -> Result<TraceEvent, TraceError> {
        let state = self.lock();
        Self::process(&state, tracee).map_err(|_| TraceError::InvalidProcess)?;
        drop(state);
        let event = self.traces.stop(tracee, stop)?;
        let mut state = self.lock();
        state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
        drop(state);
        self.child_ready.notify_all();
        Ok(event)
    }

    pub fn trace_wait(&self, tracer: ProcessId, tracee: Option<ProcessId>) -> Result<TraceWait, TraceError> {
        let state = self.lock();
        Self::process(&state, tracer).map_err(|_| TraceError::InvalidProcess)?;
        drop(state);
        self.traces.wait(tracer, tracee)
    }

    pub fn trace_peek(&self, tracer: ProcessId, tracee: Option<ProcessId>) -> Result<TraceWait, TraceError> {
        self.traces.peek(tracer, tracee)
    }

    pub fn trace_commit_wait(&self, tracer: ProcessId, event: TraceEvent) -> Result<(), TraceError> {
        self.traces.commit_wait(tracer, event)
    }

    pub fn trace_resume(&self, tracer: ProcessId, link: TraceLinkId, command: TraceResume) -> Result<(), TraceError> {
        self.traces.resume(tracer, link, command)
    }

    pub fn trace_await_resume(&self, tracee: ProcessId, link: TraceLinkId) -> Result<TraceResume, TraceError> {
        self.traces.await_resume(tracee, link)
    }

    pub fn trace_take_resume(&self, tracee: ProcessId, link: TraceLinkId) -> Result<Option<TraceResume>, TraceError> {
        self.traces.take_resume(tracee, link)
    }

    pub fn trace_link(
        &self,
        tracer: ProcessId,
        tracee: ProcessId,
        require_stop: bool,
    ) -> Result<TraceLinkId, TraceError> {
        self.traces.link(tracer, tracee, require_stop)
    }

    pub fn process_number(&self, number: u32) -> Result<ProcessId, TraceError> {
        let state = self.lock();
        state
            .processes
            .iter()
            .enumerate()
            .find_map(|(slot, entry)| {
                entry.value.as_ref()?;
                let id = ProcessId::new(slot as u32, entry.generation);
                (id.number() == number).then_some(id)
            })
            .ok_or(TraceError::InvalidProcess)
    }

    #[must_use]
    pub fn trace_image(&self) -> TraceImage {
        self.traces.image()
    }

    pub fn restore_trace_image(&self, image: &TraceImage) -> Result<(), TraceError> {
        let state = self.lock();
        for link in &image.links {
            Self::process(&state, link.tracer).map_err(|_| TraceError::InvalidSnapshot)?;
            Self::process(&state, link.tracee).map_err(|_| TraceError::InvalidSnapshot)?;
            if link.tracer == link.tracee {
                return Err(TraceError::InvalidSnapshot);
            }
        }
        self.traces.restore(image)
    }
}
