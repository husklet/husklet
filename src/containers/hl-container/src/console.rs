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
    owns_attachment: bool,
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
            owns_attachment: false,
        }
    }

    /// Claims the single interactive attachment owned by this process.
    ///
    /// # Errors
    /// Returns an error while another interactive client owns stdin and
    /// disconnect authority for the same process.
    pub fn claim_attachment(mut self) -> Result<Self> {
        self.io.claim_attachment()?;
        self.io.acknowledge(self.cursor);
        self.owns_attachment = true;
        Ok(self)
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

    /// Records output successfully delivered to this session's external owner.
    pub fn acknowledge(&self, sequence: u64) {
        if self.owns_attachment && sequence <= self.cursor {
            self.io.acknowledge(sequence);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.owns_attachment {
            self.io.release_attachment();
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
    generation: u64,
    start_cursor: u64,
    pub(crate) notify: tokio::sync::Notify,
    live: std::sync::Mutex<std::collections::VecDeque<Entry>>,
    done: std::sync::atomic::AtomicBool,
    terminal_at: std::sync::atomic::AtomicU64,
    stdin: bool,
    input: tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    receiver: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>>,
    attachment: std::sync::atomic::AtomicBool,
    delivered: std::sync::atomic::AtomicU64,
}

impl Io {
    const LIVE_CAPACITY: usize = 1_024;

    pub(crate) fn new(stdin: bool, generation: u64, start_cursor: u64) -> Self {
        let (input, receiver) = tokio::sync::mpsc::channel(64);
        Self {
            generation,
            start_cursor,
            notify: tokio::sync::Notify::new(),
            live: std::sync::Mutex::new(std::collections::VecDeque::new()),
            done: std::sync::atomic::AtomicBool::new(false),
            terminal_at: std::sync::atomic::AtomicU64::new(u64::MAX),
            stdin,
            input: tokio::sync::Mutex::new(stdin.then_some(input)),
            receiver: tokio::sync::Mutex::new(stdin.then_some(receiver)),
            attachment: std::sync::atomic::AtomicBool::new(false),
            delivered: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn claim_attachment(&self) -> Result<()> {
        self.attachment
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| Error::Runtime("process already has an interactive attachment".into()))
    }

    fn release_attachment(&self) {
        self.attachment.store(false, std::sync::atomic::Ordering::Release);
    }

    fn acknowledge(&self, sequence: u64) {
        self.delivered.fetch_max(sequence, std::sync::atomic::Ordering::AcqRel);
    }

    pub(crate) fn delivered_cursor(&self) -> u64 {
        self.delivered.load(std::sync::atomic::Ordering::Acquire)
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

    pub(crate) async fn finish(&self) {
        self.input.lock().await.take();
        let live = self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let terminal_at = live
            .back()
            .map_or(self.start_cursor, |entry| entry.sequence)
            .max(self.start_cursor);
        let _ = self.terminal_at.compare_exchange(
            u64::MAX,
            terminal_at,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
        self.done.store(true, std::sync::atomic::Ordering::Release);
        drop(live);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn is_past_terminal(&self, cursor: u64) -> bool {
        cursor >= self.terminal_at.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn publish(&self, entry: Entry) {
        let mut live = self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.done.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
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
    use super::{Input, Io};
    use crate::{Entry, Stream};
    use std::sync::Arc;

    #[test]
    fn live_entries_are_available_without_reopening_the_journal() {
        let io = Io::new(false, 7, 0);
        io.publish(Entry {
            sequence: 8,
            timestamp_ms: 1,
            stream: Stream::Stdout,
            bytes: b"x".to_vec(),
        });

        assert_eq!(io.after(7).unwrap().bytes, b"x");
        assert!(io.after(8).is_none());
    }

    #[tokio::test]
    async fn completed_generation_cannot_write_into_its_replacement() {
        let first = Arc::new(Io::new(true, 11, 0));
        let stale = Input { io: Arc::clone(&first) };
        let mut first_receiver = first.take_input().await.unwrap().unwrap();
        stale.write(b"first".to_vec()).await.unwrap();
        assert_eq!(first_receiver.recv().await.unwrap(), b"first");

        first.finish().await;
        let second = Arc::new(Io::new(true, 12, 0));
        let current = Input {
            io: Arc::clone(&second),
        };
        let mut second_receiver = second.take_input().await.unwrap().unwrap();

        assert!(stale.write(b"stale".to_vec()).await.is_err());
        current.write(b"current".to_vec()).await.unwrap();
        assert_eq!(second_receiver.recv().await.unwrap(), b"current");
        assert!(second_receiver.try_recv().is_err(), "stale input crossed generations");
    }

    #[tokio::test]
    async fn attachment_authority_is_scoped_to_one_generation() {
        let first = Arc::new(Io::new(true, 21, 0));
        first.claim_attachment().unwrap();
        first.finish().await;

        let second = Arc::new(Io::new(true, 22, 0));
        second
            .claim_attachment()
            .expect("a stale generation must not own the replacement attachment");
        assert!(first.claim_attachment().is_err());
    }

    #[tokio::test]
    async fn finishing_linearizes_against_final_output_publication() {
        let io = Arc::new(Io::new(false, 31, 4));
        io.publish(Entry {
            sequence: 5,
            timestamp_ms: 1,
            stream: Stream::Stdout,
            bytes: b"final".to_vec(),
        });
        io.finish().await;
        io.publish(Entry {
            sequence: 6,
            timestamp_ms: 2,
            stream: Stream::Stdout,
            bytes: b"next-generation".to_vec(),
        });

        assert_eq!(io.after(4).unwrap().bytes, b"final");
        assert!(io.after(5).is_none());
        assert!(io.is_past_terminal(5));
    }
}
