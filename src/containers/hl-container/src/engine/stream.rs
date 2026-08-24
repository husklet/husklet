//! Container-owned adapters for the engine's standard-stream and terminal ports.
//!
//! A guest writes to a log sender and reads from a Tokio queue, but the engine's ports are
//! blocking and expect bounded cancellation. Both channels here bridge exactly that: a byte queue
//! under its own lock, a condition variable that bounds every wait independently of the sender's
//! lifetime, and a waker that turns an arriving keystroke into an immediate wakeup rather than a
//! poll interval.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex as StdMutex},
    time::Duration,
};

struct TerminalState {
    pending: VecDeque<u8>,
    closed: bool,
}

/// The client's input queue, held outside the state lock so a reader parked on
/// it never delays `close` or a concurrent drain of already-received bytes.
type InputQueue = StdMutex<Option<tokio::sync::mpsc::Receiver<Vec<u8>>>>;

/// Unparks the reader that installed it, so an arriving keystroke wakes the
/// guest's read immediately instead of at the next cancellation tick.
struct ReaderWaker(std::thread::Thread);

impl std::task::Wake for ReaderWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Blocks until the sender enqueues bytes, the sender is dropped, or `deadline`
/// passes.
///
/// `Poll::Pending` means the deadline expired with the queue still open, which
/// is the caller's opportunity to observe cancellation. Waking on the sender's
/// own notification rather than on a poll interval is what keeps keystroke
/// latency at the cost of a thread wakeup instead of half a poll period.
fn receive_until(
    receiver: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    deadline: std::time::Instant,
) -> std::task::Poll<Option<Vec<u8>>> {
    let waker = std::task::Waker::from(Arc::new(ReaderWaker(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    loop {
        if let std::task::Poll::Ready(received) = receiver.poll_recv(&mut context) {
            return std::task::Poll::Ready(received);
        }
        let now = std::time::Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return std::task::Poll::Pending;
        };
        std::thread::park_timeout(remaining);
    }
}

/// Container-owned adapter for the engine's host-terminal port.
///
/// The condition variable provides bounded cancellation independently of the
/// client input sender's lifetime. Tokio's bounded queues provide backpressure;
/// timed waits avoid holding a lock across a channel operation or busy-spinning.
pub(super) struct TerminalChannel {
    state: StdMutex<TerminalState>,
    input: InputQueue,
    changed: Condvar,
    output: crate::service::LogSender,
}

pub(super) struct OutputChannel {
    state: StdMutex<TerminalState>,
    input: InputQueue,
    changed: Condvar,
    output: crate::service::LogSender,
}

impl OutputChannel {
    const CANCELLATION_POLL: Duration = TerminalChannel::CANCELLATION_POLL;

    pub(super) fn new(
        receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        output: crate::service::LogSender,
    ) -> Self {
        Self {
            state: StdMutex::new(TerminalState {
                pending: VecDeque::new(),
                closed: false,
            }),
            input: StdMutex::new(receiver),
            changed: Condvar::new(),
            output,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TerminalState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits one cancellation period for client input, appending whatever
    /// arrives to `pending`. Returns false once the client's sender is gone.
    fn receive(&self) -> bool {
        let mut input = self.input.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(receiver) = input.as_mut() else {
            return false;
        };
        match receive_until(receiver, std::time::Instant::now() + Self::CANCELLATION_POLL) {
            std::task::Poll::Ready(Some(bytes)) => {
                drop(input);
                self.lock().pending.extend(bytes);
            }
            std::task::Poll::Ready(None) => *input = None,
            std::task::Poll::Pending => {}
        }
        true
    }
}

impl hl_engine::composition::StandardStreamPort for OutputChannel {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut state = self.lock();
                if state.closed {
                    return Ok(0);
                }
                if !state.pending.is_empty() {
                    let length = output.len().min(state.pending.len());
                    for destination in &mut output[..length] {
                        *destination = state.pending.pop_front().expect("bounded by pending length");
                    }
                    return Ok(length);
                }
            }
            if !self.receive() {
                return Ok(0);
            }
        }
    }

    fn write(&self, stream: hl_engine::composition::StandardStream, input: &[u8]) -> std::io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let length = input.len().min(crate::service::LOG_CHUNK_BYTES);
        let stream = match stream {
            hl_engine::composition::StandardStream::Stdout => crate::Stream::Stdout,
            hl_engine::composition::StandardStream::Stderr => crate::Stream::Stderr,
        };
        let mut chunk = crate::LogChunk {
            stream,
            bytes: input[..length].to_vec(),
        };
        loop {
            if self.lock().closed {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            match self.output.try_send(chunk) {
                Ok(()) => return Ok(length),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    chunk = returned;
                    std::thread::sleep(Self::CANCELLATION_POLL);
                }
            }
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

impl TerminalChannel {
    const CANCELLATION_POLL: Duration = Duration::from_millis(10);

    pub(super) fn new(
        receiver: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        output: crate::service::LogSender,
    ) -> Self {
        Self {
            state: StdMutex::new(TerminalState {
                pending: VecDeque::new(),
                closed: false,
            }),
            input: StdMutex::new(receiver),
            changed: Condvar::new(),
            output,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TerminalState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits one cancellation period for client input, appending whatever
    /// arrives to `pending`. Returns false once the client's sender is gone.
    fn receive(&self) -> bool {
        let mut input = self.input.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(receiver) = input.as_mut() else {
            return false;
        };
        match receive_until(receiver, std::time::Instant::now() + Self::CANCELLATION_POLL) {
            std::task::Poll::Ready(Some(bytes)) => {
                drop(input);
                self.lock().pending.extend(bytes);
            }
            std::task::Poll::Ready(None) => *input = None,
            std::task::Poll::Pending => {}
        }
        true
    }
}

impl hl_engine::composition::TerminalPort for TerminalChannel {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut state = self.lock();
                if state.closed {
                    return Ok(0);
                }
                if !state.pending.is_empty() {
                    let length = output.len().min(state.pending.len());
                    for destination in &mut output[..length] {
                        *destination = state.pending.pop_front().expect("bounded by pending length");
                    }
                    return Ok(length);
                }
            }
            if !self.receive() {
                return Ok(0);
            }
        }
    }

    fn write(&self, input: &[u8]) -> std::io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let length = input.len().min(crate::service::LOG_CHUNK_BYTES);
        let mut chunk = crate::LogChunk {
            stream: crate::Stream::Stdout,
            bytes: input[..length].to_vec(),
        };
        loop {
            if self.lock().closed {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "terminal transport closed",
                ));
            }
            match self.output.try_send(chunk) {
                Ok(()) => return Ok(length),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "container log receiver closed",
                    ));
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    chunk = returned;
                    let state = self.lock();
                    if state.closed {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "terminal transport closed",
                        ));
                    }
                    drop(
                        self.changed
                            .wait_timeout(state, Self::CANCELLATION_POLL)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
            }
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalChannel;
    use hl_engine::composition::TerminalPort as _;
    use std::sync::{Arc, Mutex};

    #[test]
    fn terminal_channel_preserves_partial_input_and_eof() {
        let (input, receiver) = tokio::sync::mpsc::channel(1);
        input.blocking_send(b"abcdef".to_vec()).unwrap();
        drop(input);
        let (output, _logs) = crate::service::log_channel();
        let terminal = TerminalChannel::new(Some(receiver), output);
        let mut bytes = [0_u8; 4];

        assert_eq!(terminal.read(&mut bytes).unwrap(), 4);
        assert_eq!(&bytes, b"abcd");
        assert_eq!(terminal.read(&mut bytes).unwrap(), 2);
        assert_eq!(&bytes[..2], b"ef");
        assert_eq!(terminal.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn terminal_channel_merges_and_bounds_output() {
        let (output, mut logs) = crate::service::log_channel();
        let terminal = TerminalChannel::new(None, output);
        let bytes = vec![b'x'; crate::service::LOG_CHUNK_BYTES + 7];

        assert_eq!(terminal.write(&bytes).unwrap(), crate::service::LOG_CHUNK_BYTES);
        assert_eq!(
            logs.blocking_recv().unwrap(),
            crate::LogChunk {
                stream: crate::Stream::Stdout,
                bytes: bytes[..crate::service::LOG_CHUNK_BYTES].to_vec(),
            }
        );
    }

    #[test]
    fn terminal_close_cancels_blocked_read() {
        let (_input, receiver) = tokio::sync::mpsc::channel(1);
        let (output, _logs) = crate::service::log_channel();
        let terminal = Arc::new(TerminalChannel::new(Some(receiver), output));
        let reader = Arc::clone(&terminal);
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(reader.read(&mut [0_u8; 1])).unwrap();
        });

        terminal.close();

        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_millis(100))
                .unwrap()
                .unwrap(),
            0
        );
        worker.join().unwrap();
    }

    #[test]
    fn terminal_close_cancels_backpressured_write() {
        let (output, _logs) = crate::service::log_channel();
        for _ in 0..crate::service::LOG_QUEUE_DEPTH {
            output
                .blocking_send(crate::LogChunk {
                    stream: crate::Stream::Stdout,
                    bytes: vec![b'x'],
                })
                .unwrap();
        }
        let terminal = Arc::new(TerminalChannel::new(None, output));
        let writer = Arc::clone(&terminal);
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(writer.write(b"blocked")).unwrap();
        });

        terminal.close();

        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_millis(100))
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
        worker.join().unwrap();
    }

    #[test]
    fn terminal_read_wakes_on_arriving_input_rather_than_the_poll_interval() {
        let (input, receiver) = tokio::sync::mpsc::channel(8);
        let (output, _logs) = crate::service::log_channel();
        let terminal = Arc::new(TerminalChannel::new(Some(receiver), output));
        let reader = Arc::clone(&terminal);
        let sent: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let sent_writer = Arc::clone(&sent);
        let samples = 21;
        let worker = std::thread::spawn(move || {
            let mut latencies = Vec::with_capacity(samples);
            for _ in 0..samples {
                let mut byte = [0_u8; 8];
                assert_eq!(reader.read(&mut byte).unwrap(), 1);
                let arrived = std::time::Instant::now();
                let at = sent_writer
                    .lock()
                    .unwrap()
                    .take()
                    .expect("send is timestamped before it is sent");
                latencies.push(arrived.duration_since(at));
            }
            latencies
        });

        for _ in 0..samples {
            // Longer than the cancellation poll, so every keystroke lands on an
            // already-waiting reader rather than on a queue it has yet to drain.
            std::thread::sleep(TerminalChannel::CANCELLATION_POLL * 2);
            *sent.lock().unwrap() = Some(std::time::Instant::now());
            input.blocking_send(vec![b'x']).unwrap();
        }
        let mut latencies = worker.join().unwrap();
        latencies.sort_unstable();

        // A reader woken only by the poll timer averages half the period; one
        // woken by the sender is bounded by a thread wakeup instead.
        let median = latencies[latencies.len() / 2];
        assert!(
            median * 4 < TerminalChannel::CANCELLATION_POLL,
            "median keystroke delivery {median:?} is within a poll period of {:?}",
            TerminalChannel::CANCELLATION_POLL
        );
    }

    /// Profiles the owned-message-to-byte-buffer path used by a real terminal paste.
    ///
    /// Run alone with `--ignored --nocapture`. The second round reuses the queue's high-water
    /// allocation; hashes and exact byte equality prevent a faster partial drain from looking like
    /// an improvement. This intentionally measures the current `Vec` -> `VecDeque` -> read-buffer
    /// path before changing it.
    #[test]
    #[ignore = "a large-paste profile, not an assertion"]
    fn terminal_channel_large_paste_cost() {
        const MESSAGE_BYTES: usize = 16 * 1024;
        for length in [1024_usize, 8 * 1024, 64 * 1024, 1024 * 1024] {
            let messages = length.div_ceil(MESSAGE_BYTES);
            let (input, receiver) = tokio::sync::mpsc::channel(messages.max(1));
            let (output, _logs) = crate::service::log_channel();
            let terminal = TerminalChannel::new(Some(receiver), output);
            for round in 0..2 {
                let end_to_end_started = std::time::Instant::now();
                let expected = (0..length).map(|index| (index % 251) as u8).collect::<Vec<_>>();
                for chunk in expected.chunks(MESSAGE_BYTES) {
                    input.blocking_send(chunk.to_vec()).unwrap();
                }
                let receiver_started = std::time::Instant::now();
                let mut seen = Vec::with_capacity(length);
                let mut read_calls = 0;
                let mut buffer = [0_u8; 8192];
                while seen.len() < length {
                    let count = terminal.read(&mut buffer).unwrap();
                    assert_ne!(count, 0, "terminal input ended before the declared paste length");
                    read_calls += 1;
                    seen.extend_from_slice(&buffer[..count]);
                }
                assert_eq!(seen, expected, "terminal channel changed paste bytes");
                let hash = seen.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
                });
                println!(
                    "channel-paste round={round} bytes={length} receiver_ns={} end_to_end_ns={} messages={messages} read_calls={read_calls} fnv64={hash:016x}",
                    receiver_started.elapsed().as_nanos(),
                    end_to_end_started.elapsed().as_nanos()
                );
            }
            drop(input);
            assert_eq!(terminal.read(&mut [0_u8; 1]).unwrap(), 0);
        }
    }
}
