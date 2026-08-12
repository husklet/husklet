//! Shared bounded subprocess-output capture.

use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(10);

pub(super) struct DrainOutput {
    pub(super) bytes: Vec<u8>,
    pub(super) exceeded: bool,
}

pub(super) struct Drain {
    count: Arc<AtomicU64>,
    limit: u64,
    stopping: Arc<AtomicBool>,
    thread: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drain {
    pub(super) fn spawn(source: File, limit: u64) -> Self {
        let count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&count);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = thread::spawn(move || Self::read(source, limit, &observed, &stop));
        Self {
            count,
            limit,
            stopping,
            thread,
        }
    }

    fn read(mut source: File, limit: u64, observed: &AtomicU64, stop: &AtomicBool) -> std::io::Result<Vec<u8>> {
        let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
        let mut retained = Vec::with_capacity(capacity);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let size = match source.read(&mut buffer) {
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && stop.load(Ordering::Acquire) => {
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if size == 0 {
                break;
            }
            observed.fetch_add(size as u64, Ordering::Release);
            let available = usize::try_from(limit.saturating_sub(retained.len() as u64)).unwrap_or(usize::MAX);
            retained.extend_from_slice(&buffer[..size.min(available)]);
        }
        Ok(retained)
    }

    pub(super) fn exceeded(&self) -> bool {
        self.count.load(Ordering::Acquire) > self.limit
    }

    pub(super) fn finish(self) -> std::io::Result<DrainOutput> {
        self.stopping.store(true, Ordering::Release);
        let bytes = self
            .thread
            .join()
            .map_err(|_| std::io::Error::other("subprocess capture thread panicked"))??;
        let exceeded = self.count.load(Ordering::Acquire) > self.limit;
        Ok(DrainOutput { bytes, exceeded })
    }
}
