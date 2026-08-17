use super::{Arc, Container, ContainerState, Error, ExitStatus, Io, JournalId, Result, Running, Service};

impl Service {
    pub(crate) async fn logs(&self, reference: &str) -> Result<crate::Logs> {
        let container = self.resolve(reference).await?;
        self.logs.read(&JournalId::container(container.id)).await
    }

    pub(crate) async fn attach(self: &Arc<Self>, reference: &str) -> Result<crate::Session> {
        let container = self.resolve(reference).await?;
        if matches!(container.state, ContainerState::Exited { .. }) {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "created, running, or paused",
            });
        }
        let journal = JournalId::container(container.id.clone());
        let cursor = self.logs.cursor(&journal).await?;
        let io = self.io(&container).await;
        Ok(crate::Session::new(Arc::clone(self), io, journal, cursor, cursor))
    }

    pub(crate) async fn follow(self: &Arc<Self>, reference: &str) -> Result<crate::Session> {
        let container = self.resolve(reference).await?;
        let journal = JournalId::container(container.id.clone());
        let live_at = self.logs.cursor(&journal).await?;
        let io = self.io(&container).await;
        if matches!(container.state, ContainerState::Exited { .. }) {
            io.finish().await;
        }
        Ok(crate::Session::new(Arc::clone(self), io, journal, 0, live_at))
    }

    pub(super) async fn own(self: Arc<Self>, process: Arc<dyn Running>, journal: JournalId) -> Result<ExitStatus> {
        let logs = process.take_logs();
        let (finished, receiver) = tokio::sync::watch::channel(false);
        let service = Arc::clone(&self);
        let draining = tokio::spawn(async move {
            service.drain(&journal, logs, receiver).await;
        });
        let result = process.wait().await;
        tokio::task::yield_now().await;
        let _ = finished.send(true);
        let _ = draining.await;
        result
    }

    async fn drain(
        &self,
        id: &JournalId,
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
                        if !self.append(id, chunk).await {
                            return;
                        }
                    }
                    return;
                }
                chunk = logs.recv() => chunk,
            };
            let Some(chunk) = chunk else { return };
            if !self.append(id, chunk).await {
                return;
            }
        }
    }

    async fn append(&self, id: &JournalId, chunk: crate::LogChunk) -> bool {
        match self.logs.append(id, chunk.stream, &chunk.bytes).await {
            Ok(entry) => {
                self.publish_output(id, entry).await;
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

    pub(super) async fn io(&self, container: &Container) -> Arc<Io> {
        let mut values = self.io.lock().await;
        let id = JournalId::container(container.id.clone());
        if let Some(io) = values.get(&id) {
            return Arc::clone(io);
        }
        let io = Arc::new(Io::new(container.spec.process.console.stdin));
        values.insert(id, Arc::clone(&io));
        io
    }

    async fn publish_output(&self, id: &JournalId, entry: crate::Entry) {
        if let Some(io) = self.io.lock().await.get(id).cloned() {
            io.publish(entry);
        }
    }
}
