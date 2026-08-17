use crate::{Entry, Error, JournalId, Result, service::Service};
use std::sync::Arc;

/// Cursor-based live output session for a container process.
///
/// Records are read from the durable journal; notifications are only wakeups. Slow or concurrent
/// consumers therefore cannot lose output.
pub struct Session {
    service: Arc<Service>,
    io: Arc<Io>,
    id: JournalId,
    cursor: u64,
    live_at: u64,
}

/// Cloneable writer for one process stdin.
#[derive(Clone)]
pub struct Input {
    io: Arc<Io>,
}

impl Session {
    pub(crate) fn new(service: Arc<Service>, io: Arc<Io>, id: JournalId, cursor: u64, live_at: u64) -> Self {
        Self {
            service,
            io,
            id,
            cursor,
            live_at,
        }
    }

    /// Reads output that was durable before this session was created.
    ///
    /// The cursor advances atomically into live following; calling this again returns no duplicates.
    ///
    /// # Errors
    /// Returns journal persistence or corruption failures.
    pub async fn history(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        while self.cursor < self.live_at {
            let remaining = usize::try_from(self.live_at - self.cursor).unwrap_or(usize::MAX);
            let mut batch = self.service.history(&self.id, self.cursor, remaining.min(256)).await?;
            let Some(last) = batch.last() else {
                return Err(Error::Corrupt(
                    "output journal ended before the session boundary".into(),
                ));
            };
            self.cursor = last.sequence;
            entries.append(&mut batch);
        }
        Ok(entries)
    }

    /// Receives the next ordered output record, or `None` after all process output is durable.
    ///
    /// # Errors
    /// Returns persistence or lifecycle failures.
    pub async fn next(&mut self) -> Result<Option<Entry>> {
        let entry = self.service.output(&self.id, self.cursor, &self.io).await?;
        if let Some(entry) = &entry {
            self.cursor = entry.sequence;
        }
        Ok(entry)
    }

    /// Writes bytes to the process stdin with bounded backpressure.
    ///
    /// # Errors
    /// Returns an error if stdin was not opened or has already been closed.
    pub async fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.io.write(bytes.into()).await
    }

    /// Explicitly closes process stdin for every attachment, causing the guest to observe EOF.
    ///
    /// Dropping a session does not close stdin. This operation is idempotent.
    pub async fn close(&self) {
        self.io.close().await;
    }

    #[must_use]
    pub fn input(&self) -> Input {
        Input {
            io: Arc::clone(&self.io),
        }
    }
}

impl Input {
    /// Writes bytes with bounded backpressure.
    ///
    /// # Errors
    /// Returns an error if stdin was not opened or has already been closed.
    pub async fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.io.write(bytes.into()).await
    }

    /// Explicitly closes process stdin. Idempotent.
    pub async fn close(&self) {
        self.io.close().await;
    }
}

pub(crate) struct Io {
    pub(crate) notify: tokio::sync::Notify,
    live: std::sync::Mutex<std::collections::VecDeque<Entry>>,
    done: std::sync::atomic::AtomicBool,
    stdin: bool,
    input: tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    receiver: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
}

impl Io {
    const LIVE_CAPACITY: usize = 1_024;

    pub(crate) fn new(stdin: bool) -> Self {
        let (input, receiver) = tokio::sync::mpsc::channel(64);
        Self {
            notify: tokio::sync::Notify::new(),
            live: std::sync::Mutex::new(std::collections::VecDeque::new()),
            done: std::sync::atomic::AtomicBool::new(false),
            stdin,
            input: tokio::sync::Mutex::new(stdin.then_some(input)),
            receiver: tokio::sync::Mutex::new(stdin.then_some(receiver)),
        }
    }

    pub(crate) async fn take_input(&self) -> Result<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>> {
        if !self.stdin {
            return Ok(None);
        }
        self.receiver
            .lock()
            .await
            .take()
            .map(Some)
            .ok_or_else(|| Error::Runtime("container stdin is already owned".into()))
    }

    pub(crate) async fn rearm_input(&self) {
        if !self.stdin {
            return;
        }
        let (input, receiver) = tokio::sync::mpsc::channel(64);
        *self.input.lock().await = Some(input);
        *self.receiver.lock().await = Some(receiver);
        self.done.store(false, std::sync::atomic::Ordering::Release);
    }

    async fn write(&self, bytes: Vec<u8>) -> Result<()> {
        let input = self
            .input
            .lock()
            .await
            .clone()
            .ok_or_else(|| Error::Runtime("container stdin is closed".into()))?;
        input
            .send(bytes)
            .await
            .map_err(|_| Error::Runtime("container stdin is closed".into()))
    }

    async fn close(&self) {
        self.input.lock().await.take();
    }

    pub(crate) fn finish(&self) {
        self.done.store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn publish(&self, entry: Entry) {
        let mut live = self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        live.push_back(entry);
        if live.len() > Self::LIVE_CAPACITY {
            live.pop_front();
        }
        drop(live);
        self.notify.notify_waiters();
    }

    pub(crate) fn after(&self, cursor: u64) -> Option<Entry> {
        let next = cursor.checked_add(1)?;
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| entry.sequence == next)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::Io;
    use crate::{Entry, Stream};

    #[test]
    fn live_entries_are_available_without_reopening_the_journal() {
        let io = Io::new(false);
        io.publish(Entry {
            sequence: 8,
            timestamp_ms: 1,
            stream: Stream::Stdout,
            bytes: b"x".to_vec(),
        });

        assert_eq!(io.after(7).unwrap().bytes, b"x");
        assert!(io.after(8).is_none());
    }
}
