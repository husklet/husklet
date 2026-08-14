use super::Server;
use crate::composition::{CheckpointSink, CheckpointSource, CompositionError};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, mpsc},
    time::Duration,
};

struct Store;

impl CheckpointSink for Store {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

impl CheckpointSource for Store {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[test]
fn stop_interrupts_silent_checkpoint_peer() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (channel, _silent) = UnixStream::pair().unwrap();
    let worker = Arc::clone(&server);
    let (done, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        worker.serve(channel, 1);
        let _ = done.send(());
    });
    while server.channels.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    server.stop();
    completed.recv_timeout(Duration::from_millis(250)).unwrap();
}

#[test]
fn peer_accepted_after_stop_cannot_escape_shutdown() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (channel, _silent) = UnixStream::pair().unwrap();
    server.stop();
    let worker = Arc::clone(&server);
    let (done, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        worker.serve(channel, 1);
        let _ = done.send(());
    });
    completed.recv_timeout(Duration::from_millis(250)).unwrap();
}
