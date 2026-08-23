#![allow(unsafe_code)]

use super::{
    Arc, AsRawFd, AtomicBool, CompositionError, File, FromRawFd, JoinHandle, Mutex, Ordering, OwnedFd, Read,
    StandardStream, StandardStreamPort, write_master,
};

pub(crate) struct NativeOutputBridge {
    input: OwnedFd,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    port: Arc<dyn StandardStreamPort>,
    workers: Vec<JoinHandle<()>>,
}

impl NativeOutputBridge {
    pub(crate) fn attach(port: Arc<dyn StandardStreamPort>) -> Result<Self, CompositionError> {
        let (input, input_writer) = open_input_pipe()?;
        let (stdout_reader, stdout) = open_pipe()?;
        let (stderr_reader, stderr) = open_pipe()?;
        let stop = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(Mutex::new(0));
        let input_worker = spawn_standard_input(Arc::clone(&port), Arc::clone(&stop), input_writer)?;
        let stdout_worker = match spawn_stream_reader(
            Arc::clone(&port),
            Arc::clone(&stop),
            Arc::clone(&in_flight),
            stdout_reader,
            StandardStream::Stdout,
        ) {
            Ok(worker) => worker,
            Err(error) => {
                stop.store(true, Ordering::Release);
                port.close();
                let _ = input_worker.join();
                return Err(error);
            }
        };
        let stderr_worker = match spawn_stream_reader(
            Arc::clone(&port),
            Arc::clone(&stop),
            Arc::clone(&in_flight),
            stderr_reader,
            StandardStream::Stderr,
        ) {
            Ok(worker) => worker,
            Err(error) => {
                stop.store(true, Ordering::Release);
                port.close();
                drop(stdout);
                let _ = input_worker.join();
                let _ = stdout_worker.join();
                return Err(error);
            }
        };
        Ok(Self {
            input,
            stdout: Some(stdout),
            stderr: Some(stderr),
            stop,
            in_flight,
            port,
            workers: vec![input_worker, stdout_worker, stderr_worker],
        })
    }

    pub(crate) fn standard_fds(&self) -> [i32; 3] {
        [
            self.input.as_raw_fd(),
            self.stdout.as_ref().expect("live stdout bridge").as_raw_fd(),
            self.stderr.as_ref().expect("live stderr bridge").as_raw_fd(),
        ]
    }

    pub(crate) fn flush(&self) {
        for _ in 0..200 {
            let in_flight = self.in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if *in_flight == 0 && self.pending_bytes() == 0 {
                return;
            }
            drop(in_flight);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn pending_bytes(&self) -> i32 {
        [&self.stdout, &self.stderr]
            .into_iter()
            .filter_map(Option::as_ref)
            .map(|descriptor| {
                let mut pending = 0;
                // SAFETY: FIONREAD writes one integer and borrows a live pipe descriptor.
                if unsafe { libc::ioctl(descriptor.as_raw_fd(), libc::FIONREAD, &raw mut pending) } == 0 {
                    pending
                } else {
                    1
                }
            })
            .sum()
    }
}

impl Drop for NativeOutputBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.stdout.take();
        self.stderr.take();
        self.port.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn open_input_pipe() -> Result<(OwnedFd, File), CompositionError> {
    let (reader, writer) = open_pipe_descriptors()?;
    set_file_nonblocking(&writer)?;
    Ok((reader, writer))
}

fn open_pipe() -> Result<(File, OwnedFd), CompositionError> {
    let (reader, writer) = open_pipe_descriptors()?;
    let reader = File::from(reader);
    set_file_nonblocking(&reader)?;
    Ok((reader, writer.into()))
}

fn open_pipe_descriptors() -> Result<(OwnedFd, File), CompositionError> {
    let mut descriptors = [-1; 2];
    // SAFETY: descriptors points to two writable integers.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    for descriptor in descriptors {
        // SAFETY: pipe returned live descriptors; F_SETFD does not transfer ownership.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            // SAFETY: both descriptors are still uniquely owned here.
            unsafe {
                libc::close(descriptors[0]);
                libc::close(descriptors[1]);
            }
            return Err(CompositionError::RuntimeConstruction);
        }
    }
    // SAFETY: successful pipe returned two fresh descriptors with distinct ownership.
    Ok(unsafe { (OwnedFd::from_raw_fd(descriptors[0]), File::from_raw_fd(descriptors[1])) })
}

fn set_file_nonblocking(descriptor: &File) -> Result<(), CompositionError> {
    // SAFETY: both operations borrow a live descriptor and do not transfer ownership.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    Ok(())
}

fn spawn_stream_reader(
    port: Arc<dyn StandardStreamPort>,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    mut reader: File,
    stream: StandardStream,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name(match stream {
            StandardStream::Stdout => "hl-stdout".to_owned(),
            StandardStream::Stderr => "hl-stderr".to_owned(),
        })
        .spawn(move || {
            let mut bytes = [0_u8; 8192];
            while !stop.load(Ordering::Acquire) {
                let Some(count) = read_stream(&mut reader, &mut bytes, &stop, &in_flight) else {
                    break;
                };
                let written = write_stream(port.as_ref(), stream, &bytes[..count]);
                let mut active = in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                *active -= 1;
                drop(active);
                if !written {
                    return;
                }
            }
        })
        .map_err(|_| CompositionError::RuntimeConstruction)
}

fn spawn_standard_input(
    port: Arc<dyn StandardStreamPort>,
    stop: Arc<AtomicBool>,
    mut writer: File,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name("hl-stdin".to_owned())
        .spawn(move || {
            let mut bytes = [0_u8; 8192];
            while !stop.load(Ordering::Acquire) {
                let count = match port.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if write_master(&mut writer, &bytes[..count], &stop).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| CompositionError::RuntimeConstruction)
}

fn read_stream(reader: &mut File, bytes: &mut [u8], stop: &AtomicBool, in_flight: &Mutex<usize>) -> Option<usize> {
    while !stop.load(Ordering::Acquire) {
        let mut active = in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match reader.read(bytes) {
            Ok(0) => return None,
            Ok(count) => {
                *active += 1;
                return Some(count);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                drop(active);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => drop(active),
            Err(_) => return None,
        }
    }
    None
}

fn write_stream(port: &dyn StandardStreamPort, stream: StandardStream, bytes: &[u8]) -> bool {
    let mut written = 0;
    while written < bytes.len() {
        match port.write(stream, &bytes[written..]) {
            Ok(0) | Err(_) => return false,
            Ok(count) => written += count,
        }
    }
    true
}
