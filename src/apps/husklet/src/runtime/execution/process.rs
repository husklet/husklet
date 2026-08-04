use hl_client::api::{Size, TerminalInput};
use hl_client::Client;
use hl_ws_term::PtyBackend;
use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex};

pub(super) struct Shell;

impl Shell {
    pub(super) fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) struct ExecPty {
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) client: Client,
    pub(super) execution: String,
    pub(super) input: TerminalInput,
    pub(super) output: Output,
    pub(super) exited: Arc<Mutex<Option<i32>>>,
}

pub(super) struct Output {
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
    closed: bool,
}

impl Output {
    pub(super) fn new(receiver: std::sync::mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            pending: VecDeque::new(),
            closed: false,
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> usize {
        let mut read = 0;
        while read < buffer.len() {
            if let Some(byte) = self.pending.pop_front() {
                buffer[read] = byte;
                read += 1;
                continue;
            }
            match self.receiver.try_recv() {
                Ok(bytes) => self.pending.extend(bytes),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        read
    }

    fn finished(&self) -> bool {
        self.closed && self.pending.is_empty()
    }
}

impl PtyBackend for ExecPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.runtime.block_on(self.input.write(bytes)).map_err(io::Error::other)
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        Ok(self.output.read(buffer))
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        if let Ok(size) = Size::new(rows.max(1), columns.max(1)) {
            if let Err(error) = self
                .runtime
                .block_on(self.client.executions().resize(&self.execution, size))
            {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "workspace terminal resize failed execution={} columns={} rows={} error={error}",
                    self.execution,
                    columns,
                    rows
                );
            }
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        None
    }

    fn try_wait(&mut self) -> Option<i32> {
        self.output
            .finished()
            .then(|| *self.exited.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .flatten()
    }
}

impl Drop for ExecPty {
    fn drop(&mut self) {
        if self.try_wait().is_none() {
            if let Err(error) = self
                .runtime
                .block_on(self.client.executions().signal(&self.execution, "KILL"))
            {
                hl_log::hl_error!(
                    hl_log::tag::RUNTIME,
                    "workspace execution cleanup signal failed execution={} error={error}",
                    self.execution
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Output;

    #[test]
    fn output_finishes_only_after_every_chunk_is_drained() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(b"last ".to_vec()).unwrap();
        sender.send(b"line\n".to_vec()).unwrap();
        drop(sender);
        let mut output = Output::new(receiver);
        let mut bytes = [0; 5];

        let count = output.read(&mut bytes);

        assert_eq!(&bytes[..count], b"last ");
        assert!(!output.finished());
        let count = output.read(&mut bytes);
        assert_eq!(&bytes[..count], b"line\n");
        assert!(!output.finished());
        assert_eq!(output.read(&mut bytes), 0);
        assert!(output.finished());
    }
}
