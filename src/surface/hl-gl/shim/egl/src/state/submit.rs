use super::global::state_ptr;
use super::*;

impl GlobalState {
    pub(super) fn group(token: usize) -> Option<Arc<group::GroupSlot>> {
        // SAFETY: see [`Self::access`].
        let mutex: &Mutex<State> = unsafe { &*state_ptr() };
        let state = mutex.lock().unwrap_or_else(|error| error.into_inner());
        (token != 0).then(|| state.contexts.group(token)).flatten()
    }

    fn transport() -> Result<Ready, hl_gpu::GpuError> {
        let transport = {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(&state.transport)
        };
        let request = FeatureRequest {
            wire_version: WIRE_VERSION,
            ..FeatureRequest::default()
        };
        let ready = transport
            .ensure(&request)
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
        {
            // SAFETY: see [`Self::access`].
            let mutex: &Mutex<State> = unsafe { &*state_ptr() };
            let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
            state.native_present = ready
                .capabilities
                .present_kinds
                .contains(&PresentKind::IoSurface);
            state.max_buffer_bytes = ready.capabilities.max_buffer_bytes;
            state.ready = Some(ready.clone());
        }
        Ok(ready)
    }

    pub(crate) fn flush_surface_retirements() -> hl_gpu::Result<()> {
        let commands = Self::access(|state| core::mem::take(&mut state.surface_retirements));
        Self::submit_surface_retirements(commands)
    }

    pub(super) fn submit_surface_retirements(commands: Vec<hl_gpu::Cmd>) -> hl_gpu::Result<()> {
        if commands.is_empty() {
            return Ok(());
        }
        let ready = Self::transport()?;
        let ticket = ready
            .sequencer
            .submit(Plan::new(move |sink| sink.submit(&commands)))
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
        let result = match ticket.wait_for(Duration::from_secs(5)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                ready.sequencer.fail();
                Err(hl_gpu::GpuError::Decode(
                    "surface retirement timed out".into(),
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(hl_gpu::GpuError::Decode(
                "GPU transport actor disconnected".into(),
            )),
        };
        if let Err(error) = &result {
            Self::lose_all_groups(format!("display transport failed: {error}"));
        }
        result
    }

    fn lose_all_groups(reason: impl Into<Arc<str>>) {
        let reason = reason.into();
        let groups = Self::access(|state| state.contexts.groups());
        for group in groups {
            group.lose(Arc::clone(&reason));
        }
    }

    /// Prepare submit-only work under the share-group lease, release it before transport I/O, then verify
    /// the same generation after acknowledgement. State advances optimistically during preparation; a
    /// rejected or ambiguous request terminates the group, so no rollback can overwrite GL calls recorded
    /// by sibling contexts while the actor was waiting.
    pub(crate) fn gpu_submit<T: Send + 'static>(
        operation: impl FnOnce(&mut group::GroupData, &mut dyn CommandSink) -> hl_gpu::Result<T>
            + Send
            + 'static,
    ) -> hl_gpu::Result<T> {
        Self::gpu_submit_for(current::context(), operation)
    }

    pub(crate) fn gpu_submit_for<T: Send + 'static>(
        token: usize,
        operation: impl FnOnce(&mut group::GroupData, &mut dyn CommandSink) -> hl_gpu::Result<T>
            + Send
            + 'static,
    ) -> hl_gpu::Result<T> {
        let group = Self::group(token).ok_or(hl_gpu::GpuError::Invalid("EGL context"))?;
        let (generation, prepared) = {
            let mut lease = group
                .acquire()
                .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
            if !lease.data_mut().activate(token) {
                return Err(hl_gpu::GpuError::Invalid("current EGL context"));
            }
            let generation = lease.generation();
            let prepared = SubmitPlan::prepare(|sink| operation(lease.data_mut(), sink))?;
            (generation, prepared)
        };
        let ready = Self::transport()?;
        let failed_group = Arc::clone(&group);
        let in_flight = InFlightGroup::new(Arc::clone(&group));
        let ticket = ready
            .sequencer
            .submit(Plan::new(move |sink| {
                let mut in_flight = in_flight;
                match prepared.execute(sink) {
                    Ok(value) => {
                        in_flight.complete();
                        Ok(value)
                    }
                    Err(error) => {
                        in_flight.complete();
                        failed_group.lose(format!("GPU transport failed: {error}"));
                        Err(error)
                    }
                }
            }))
            .map_err(|error| {
                group.lose(format!("GPU transport submission failed: {error}"));
                hl_gpu::GpuError::Decode(error.to_string())
            })?;
        let value = match ticket.wait_for(Duration::from_secs(31)) {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                group.lose("GPU transport operation timed out");
                ready.sequencer.fail();
                return Err(hl_gpu::GpuError::Decode(
                    "GPU transport operation timed out".into(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                group.lose("GPU transport actor disconnected");
                return Err(hl_gpu::GpuError::Decode(
                    "GPU transport actor disconnected".into(),
                ));
            }
        };
        if group
            .generation()
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?
            != generation
        {
            return Err(hl_gpu::GpuError::Invalid("share group generation changed"));
        }
        let mut lease = group
            .acquire()
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
        if lease.generation() != generation || !lease.data_mut().activate(token) {
            lease.lose("share group changed before GPU completion");
            return Err(hl_gpu::GpuError::Invalid("share group generation changed"));
        }
        lease.data_mut().collect_deleted_objects();
        Ok(value)
    }

    pub(crate) fn gpu_io<T: Send + 'static>(
        deadline: Duration,
        operation: impl FnOnce(&mut group::GroupData, &mut dyn CommandSink) -> hl_gpu::Result<T>
            + Send
            + 'static,
    ) -> hl_gpu::Result<IoResult<T>> {
        Self::gpu_io_for(current::context(), deadline, operation)
    }

    pub(crate) fn gpu_io_for<T: Send + 'static>(
        token: usize,
        deadline: Duration,
        operation: impl FnOnce(&mut group::GroupData, &mut dyn CommandSink) -> hl_gpu::Result<T>
            + Send
            + 'static,
    ) -> hl_gpu::Result<IoResult<T>> {
        let group = Self::group(token).ok_or(hl_gpu::GpuError::Invalid("EGL context"))?;
        let (generation, prepared) = {
            let mut lease = group
                .acquire()
                .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
            if !lease.data_mut().activate(token) {
                return Err(hl_gpu::GpuError::Invalid("current EGL context"));
            }
            let generation = lease.generation();
            let prepared = IoPlan::prepare(|sink| operation(lease.data_mut(), sink))?;
            (generation, prepared)
        };
        let ready = Self::transport()?;
        let failed_group = Arc::clone(&group);
        let in_flight = InFlightGroup::new(Arc::clone(&group));
        let ticket = ready
            .sequencer
            .submit(Plan::new(move |sink| {
                let mut in_flight = in_flight;
                match prepared.execute(sink) {
                    Ok(result) => {
                        in_flight.complete();
                        Ok(result)
                    }
                    Err(error) => {
                        in_flight.complete();
                        failed_group.lose(format!("GPU transport failed: {error}"));
                        Err(error)
                    }
                }
            }))
            .map_err(|error| {
                group.lose(format!("GPU transport submission failed: {error}"));
                hl_gpu::GpuError::Decode(error.to_string())
            })?;
        let result = match ticket.wait_for(deadline) {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => return Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                group.lose("GPU transport operation timed out");
                ready.sequencer.fail();
                return Err(hl_gpu::GpuError::Decode(
                    "GPU transport operation timed out".into(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                group.lose("GPU transport actor disconnected");
                return Err(hl_gpu::GpuError::Decode(
                    "GPU transport actor disconnected".into(),
                ));
            }
        };
        let mut lease = group
            .acquire()
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
        if lease.generation() != generation || !lease.data_mut().activate(token) {
            lease.lose("share group changed before GPU I/O completion");
            return Err(hl_gpu::GpuError::Invalid("share group generation changed"));
        }
        lease.data_mut().collect_deleted_objects();
        Ok(result)
    }
}
