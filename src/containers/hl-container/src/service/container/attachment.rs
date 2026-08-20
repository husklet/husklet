use super::{Arc, Container, ContainerState, Error, ExitStatus, Io, JournalId, Result, Running, Service};

fn remove_exact_io(
    values: &mut std::collections::HashMap<JournalId, Arc<Io>>,
    id: &JournalId,
    generation: &Arc<Io>,
) -> Option<Arc<Io>> {
    values
        .get(id)
        .is_some_and(|current| Arc::ptr_eq(current, generation))
        .then(|| values.remove(id).expect("matched I/O generation"))
}

fn remove_exact_owner(
    values: &mut std::collections::HashMap<JournalId, Arc<super::OutputOwner>>,
    id: &JournalId,
    owner: &Arc<super::OutputOwner>,
) -> Option<Arc<super::OutputOwner>> {
    values
        .get(id)
        .is_some_and(|current| Arc::ptr_eq(current, owner))
        .then(|| values.remove(id).expect("matched output owner"))
}

impl Service {
    pub(crate) async fn logs(&self, reference: &str) -> Result<crate::Logs> {
        let container = self.resolve(reference).await?;
        self.logs.read(&JournalId::container(container.id)).await
    }

    pub(crate) async fn attach(self: &Arc<Self>, reference: &str) -> Result<crate::Session> {
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        if matches!(container.state, ContainerState::Exited { .. }) && container.checkpoint.is_none() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "created, running, paused, or checkpointed",
            });
        }
        let journal = JournalId::container(container.id.clone());
        let cursor = self.logs.cursor(&journal).await?;
        let io = self.attach_io(&container, cursor).await?;
        Ok(crate::Session::new(Arc::clone(self), io, journal, cursor, cursor))
    }

    pub(crate) async fn follow(self: &Arc<Self>, reference: &str) -> Result<crate::Session> {
        let _guard = self.operations.lock().await;
        let container = self.resolve(reference).await?;
        let journal = JournalId::container(container.id.clone());
        let live_at = self.logs.cursor(&journal).await?;
        let io = if container.state.is_active() {
            self.attach_io(&container, live_at).await?
        } else {
            Arc::new(Io::new(
                container.spec.process.console.stdin,
                container.generation,
                live_at,
            ))
        };
        if !container.state.is_active() {
            io.finish().await;
        }
        Ok(crate::Session::new(Arc::clone(self), io, journal, 0, live_at))
    }

    pub(super) async fn own(
        self: Arc<Self>,
        process: Arc<dyn Running>,
        journal: JournalId,
        io: Arc<Io>,
        complete: tokio::sync::watch::Sender<bool>,
    ) -> Result<ExitStatus> {
        let logs = process.take_logs();
        let (finished, receiver) = tokio::sync::watch::channel(false);
        let waiting = async {
            let result = process.wait().await;
            tokio::task::yield_now().await;
            let _ = finished.send(true);
            result
        };
        let (result, ()) = tokio::join!(waiting, self.drain(&journal, &io, logs, receiver));
        io.finish().await;
        let _ = complete.send(true);
        result
    }

    async fn drain(
        &self,
        id: &JournalId,
        io: &Io,
        logs: Option<crate::service::LogReceiver>,
        mut finished: tokio::sync::watch::Receiver<bool>,
    ) {
        let Some(mut logs) = logs else { return };
        loop {
            let chunk = tokio::select! {
                biased;
                changed = finished.changed() => {
                    let _ = changed;
                    while let Ok(chunk) = logs.try_recv() {
                        if !self.append(id, io, chunk).await {
                            return;
                        }
                    }
                    return;
                }
                chunk = logs.recv() => chunk,
            };
            let Some(chunk) = chunk else { return };
            if !self.append(id, io, chunk).await {
                return;
            }
        }
    }

    async fn append(&self, id: &JournalId, io: &Io, chunk: crate::LogChunk) -> bool {
        match self.logs.append(id, chunk.stream, &chunk.bytes).await {
            Ok(entry) => {
                io.publish(entry);
                true
            }
            Err(error) => {
                self.failures.lock().await.insert(id.clone(), error.to_string());
                false
            }
        }
    }

    pub(crate) async fn output(&self, id: &JournalId, cursor: u64, io: &Io) -> Result<Option<crate::Entry>> {
        loop {
            if io.is_past_terminal(cursor) {
                return Ok(None);
            }
            let notified = io.notify.notified();
            tokio::pin!(notified);
            if let Some(entry) = io.after(cursor) {
                return Ok(Some(entry));
            }
            if let Some(entry) = self.logs.after(id, cursor, 1).await?.into_iter().next() {
                return Ok(Some(entry));
            }
            if io.is_done() {
                return Ok(None);
            }
            notified.await;
        }
    }

    pub(crate) async fn history(&self, id: &JournalId, cursor: u64, limit: usize) -> Result<Vec<crate::Entry>> {
        self.logs.after(id, cursor, limit).await
    }

    /// Waits for one journal's output owner to close its generation, no later than `deadline`.
    ///
    /// The bound is an absolute instant rather than a per-journal budget on purpose. A capture
    /// waits on the container and then on every sealed domain member, and a per-journal duration
    /// makes the total scale with the member count: three wedged members at 30s each run 90s, past
    /// the window the caller that reports the failure is itself willing to wait. One deadline
    /// shared by every wait in a capture keeps the capture's own budget the thing that gives up
    /// first, so the failure it raises is the attributed one.
    pub(super) async fn await_output_completion(
        &self,
        id: &JournalId,
        mut completion: tokio::sync::watch::Receiver<bool>,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        if *completion.borrow() {
            return Ok(());
        }
        if tokio::time::timeout_at(deadline, completion.changed()).await.is_err() {
            if let Some(owner) = self.output_owners.lock().await.remove(id) {
                owner.abort.abort();
            }
            let _ = completion.changed().await;
            return Err(Error::Runtime(format!(
                "timed out waiting for {id} process output ownership to close"
            )));
        }
        if completion.has_changed().is_err() && !*completion.borrow() {
            return Err(Error::Runtime(format!(
                "{id} process output owner exited without closing its generation"
            )));
        }
        if *completion.borrow() {
            Ok(())
        } else {
            Err(Error::Runtime(format!(
                "{id} process output owner signalled completion without closing its generation"
            )))
        }
    }

    pub(super) async fn attach_io(&self, container: &Container, start_cursor: u64) -> Result<Arc<Io>> {
        let generation = if container.state.is_active() {
            container.generation
        } else {
            container
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::Runtime("container generation space is exhausted".into()))?
        };
        Ok(self.io_for_generation(container, generation, start_cursor).await)
    }

    pub(super) async fn io_for_generation(&self, container: &Container, generation: u64, start_cursor: u64) -> Arc<Io> {
        let mut values = self.io.lock().await;
        let id = JournalId::container(container.id.clone());
        if let Some(io) = values.get(&id)
            && io.generation() == generation
        {
            return Arc::clone(io);
        }
        let io = Arc::new(Io::new(container.spec.process.console.stdin, generation, start_cursor));
        let previous = values.insert(id, Arc::clone(&io));
        drop(values);
        if let Some(previous) = previous {
            previous.finish().await;
        }
        io
    }

    pub(super) async fn retire_io_generation(&self, id: &JournalId, generation: &Arc<Io>) {
        let removed = {
            let mut values = self.io.lock().await;
            remove_exact_io(&mut values, id, generation)
        };
        if let Some(io) = removed {
            io.finish().await;
        } else {
            generation.finish().await;
        }
    }

    pub(super) async fn retire_output_owner(&self, id: &JournalId, owner: &Arc<super::OutputOwner>) {
        let mut owners = self.output_owners.lock().await;
        remove_exact_owner(&mut owners, id, owner);
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Io, JournalId, remove_exact_io, remove_exact_owner};
    use std::collections::HashMap;

    #[tokio::test]
    async fn stale_cleanup_cannot_remove_or_finish_replacement_in_the_same_slot() {
        let id = JournalId::container(crate::ContainerId::new());
        let stale = Arc::new(Io::new(true, 1, 0));
        let replacement = Arc::new(Io::new(true, 2, 0));
        let mut values = HashMap::from([(id.clone(), Arc::clone(&replacement))]);

        assert!(remove_exact_io(&mut values, &id, &stale).is_none());
        stale.finish().await;
        assert!(Arc::ptr_eq(values.get(&id).unwrap(), &replacement));
        assert!(!replacement.is_done());
        assert!(replacement.take_input().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn stale_output_owner_cleanup_cannot_remove_replacement() {
        let id = JournalId::container(crate::ContainerId::new());
        let stale_task = tokio::spawn(std::future::pending::<()>());
        let replacement_task = tokio::spawn(std::future::pending::<()>());
        let stale = Arc::new(super::super::OutputOwner {
            abort: stale_task.abort_handle(),
        });
        let replacement = Arc::new(super::super::OutputOwner {
            abort: replacement_task.abort_handle(),
        });
        let mut values = HashMap::from([(id.clone(), Arc::clone(&replacement))]);

        assert!(remove_exact_owner(&mut values, &id, &stale).is_none());
        assert!(Arc::ptr_eq(values.get(&id).unwrap(), &replacement));
        stale_task.abort();
        replacement_task.abort();
    }
}
