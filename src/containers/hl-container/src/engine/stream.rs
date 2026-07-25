use super::Engine;
use crate::{LogChunk, Stream};
use std::io::Read;
use std::sync::{Arc, Mutex};

impl Engine {
    pub(super) fn terminal_writer(
        terminal: Arc<Mutex<hl_engine::Terminal>>,
        mut input: tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        std::thread::spawn(move || {
            use std::io::Write as _;
            while let Some(bytes) = input.blocking_recv() {
                let Ok(mut terminal) = terminal.lock() else {
                    break;
                };
                if terminal.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
    }

    pub(super) fn writer(mut file: std::fs::File, mut input: tokio::sync::mpsc::Receiver<Vec<u8>>) {
        std::thread::spawn(move || {
            use std::io::Write as _;
            while let Some(bytes) = input.blocking_recv() {
                if file.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
    }

    pub(super) fn reader(
        mut file: impl Read + Send + 'static,
        stream: Stream,
        sender: tokio::sync::mpsc::UnboundedSender<LogChunk>,
    ) {
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(length) => {
                        if sender
                            .send(LogChunk {
                                stream,
                                bytes: buffer[..length].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }
}
