use hl_container::{Containers, Input, Signal, Size};
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

pub(super) struct Hostname;

impl Hostname {
    pub(super) fn sanitize(name: &str) -> String {
        let value: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        match value.trim_matches('-') {
            "" => "workspace".to_owned(),
            value => value.to_owned(),
        }
    }
}

pub(super) struct ContainerPty {
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) containers: Containers,
    pub(super) name: String,
    pub(super) input: Input,
    pub(super) output: std::sync::mpsc::Receiver<Vec<u8>>,
    pub(super) pending: VecDeque<u8>,
    pub(super) exited: Arc<Mutex<Option<i32>>>,
    pub(super) _gpu_service: Option<crate::runtime::gpu::Service>,
    pub(super) _compositor_service: Option<crate::runtime::compositor::Service>,
}

impl PtyBackend for ContainerPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.runtime
            .block_on(self.input.write(bytes.to_vec()))
            .map_err(io::Error::other)
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0;
        while read < buffer.len() {
            if let Some(byte) = self.pending.pop_front() {
                buffer[read] = byte;
                read += 1;
                continue;
            }
            match self.output.try_recv() {
                Ok(bytes) => self.pending.extend(bytes),
                Err(std::sync::mpsc::TryRecvError::Empty)
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        Ok(read)
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        if let Ok(size) = Size::new(rows.max(1), columns.max(1)) {
            let _ = self
                .runtime
                .block_on(self.containers.resize(&self.name, size));
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        None
    }

    fn try_wait(&mut self) -> Option<i32> {
        *self.exited.lock().expect("container exit status")
    }
}

impl Drop for ContainerPty {
    fn drop(&mut self) {
        if self.try_wait().is_none() {
            let _ = self
                .runtime
                .block_on(self.containers.signal(&self.name, Signal::Hangup));
        }
    }
}
