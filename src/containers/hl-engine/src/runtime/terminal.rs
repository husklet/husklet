#![allow(unsafe_code)]

use crate::composition::{
    CompositionError, StandardStream, StandardStreamPort, Terminal, TerminalAttachment, TerminalPort,
};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub(super) struct NativeOutputBridge {
    input: OwnedFd,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    port: Arc<dyn StandardStreamPort>,
    workers: Vec<JoinHandle<()>>,
}

impl NativeOutputBridge {
    pub(super) fn attach(port: Arc<dyn StandardStreamPort>) -> Result<Self, CompositionError> {
        let input = open_null()?;
        let (stdout_reader, stdout) = open_pipe()?;
        let (stderr_reader, stderr) = open_pipe()?;
        let stop = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(Mutex::new(0));
        let stdout_worker = spawn_stream_reader(
            Arc::clone(&port),
            Arc::clone(&stop),
            Arc::clone(&in_flight),
            stdout_reader,
            StandardStream::Stdout,
        )?;
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
                drop(stdout);
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
            workers: vec![stdout_worker, stderr_worker],
        })
    }

    pub(super) fn standard_fds(&self) -> [i32; 3] {
        [
            self.input.as_raw_fd(),
            self.stdout.as_ref().expect("live stdout bridge").as_raw_fd(),
            self.stderr.as_ref().expect("live stderr bridge").as_raw_fd(),
        ]
    }

    pub(super) fn flush(&self) {
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

fn open_null() -> Result<OwnedFd, CompositionError> {
    let path = c"/dev/null";
    // SAFETY: path is a static NUL-terminated string and the returned descriptor is uniquely owned.
    let descriptor = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if descriptor < 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    // SAFETY: successful open returned a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn open_pipe() -> Result<(File, OwnedFd), CompositionError> {
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
    // SAFETY: successful pipe2 returned two fresh descriptors with distinct ownership.
    let pair = unsafe { (File::from_raw_fd(descriptors[0]), OwnedFd::from_raw_fd(descriptors[1])) };
    set_file_nonblocking(&pair.0)?;
    Ok(pair)
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

struct NativeTerminalControl {
    master: OwnedFd,
}

impl TerminalAttachment for NativeTerminalControl {
    fn resize(&self, rows: u16, columns: u16) -> Result<(), CompositionError> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the control owns a live PTY master and `size` is readable for this ioctl call.
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
        (result == 0).then_some(()).ok_or(CompositionError::RuntimeConstruction)
    }
}

pub(super) struct NativeTerminalBridge {
    slave: OwnedFd,
    monitor: File,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    terminal: Arc<Terminal>,
    port: Arc<dyn TerminalPort>,
    workers: Vec<JoinHandle<()>>,
}

impl NativeTerminalBridge {
    pub(super) fn attach(terminal: Arc<Terminal>) -> Result<Self, CompositionError> {
        let (master, slave) = open_pair(terminal.initial())?;
        set_nonblocking(&master)?;
        let input_master = duplicate(&master)?;
        let output_master = duplicate(&master)?;
        let monitor = duplicate(&master)?;
        let control = Arc::new(NativeTerminalControl { master });
        terminal.attach(control)?;
        let stop = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(Mutex::new(0));
        let port = terminal.port();
        let input = spawn_input(Arc::clone(&port), Arc::clone(&stop), input_master)?;
        let output = match spawn_output(
            Arc::clone(&port),
            Arc::clone(&stop),
            Arc::clone(&in_flight),
            output_master,
        ) {
            Ok(worker) => worker,
            Err(error) => {
                stop.store(true, Ordering::Release);
                port.close();
                let _ = input.join();
                return Err(error);
            }
        };
        Ok(Self {
            slave,
            monitor,
            stop,
            in_flight,
            terminal,
            port,
            workers: vec![input, output],
        })
    }

    pub(super) fn standard_fds(&self) -> [i32; 3] {
        [self.slave.as_raw_fd(); 3]
    }

    pub(super) fn flush(&self) {
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
        let mut pending = 0;
        // SAFETY: FIONREAD writes one integer and borrows a live duplicate of the PTY master.
        if unsafe { libc::ioctl(self.monitor.as_raw_fd(), libc::FIONREAD, &raw mut pending) } == 0 {
            pending
        } else {
            1
        }
    }
}

impl Drop for NativeTerminalBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.terminal.detach();
        self.port.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn set_nonblocking(descriptor: &OwnedFd) -> Result<(), CompositionError> {
    // SAFETY: both operations borrow a live descriptor and do not transfer ownership.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    Ok(())
}

fn open_pair(initial: (u16, u16)) -> Result<(OwnedFd, OwnedFd), CompositionError> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: initial.0,
        ws_col: initial.1,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: writable descriptor outputs and a readable winsize are supplied; no termios override is requested.
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    if result != 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    // SAFETY: successful `openpty` returned two new uniquely owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) })
}

fn duplicate(descriptor: &OwnedFd) -> Result<File, CompositionError> {
    // SAFETY: `dup` borrows a live descriptor and returns a fresh descriptor on success.
    let copy = unsafe { libc::dup(descriptor.as_raw_fd()) };
    if copy < 0 {
        return Err(CompositionError::RuntimeConstruction);
    }
    // SAFETY: `copy` is a fresh uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(copy) })
}

fn spawn_input(
    port: Arc<dyn TerminalPort>,
    stop: Arc<AtomicBool>,
    mut master: File,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name("hl-terminal-input".to_owned())
        .spawn(move || {
            let mut bytes = [0_u8; 8192];
            while !stop.load(Ordering::Acquire) {
                let count = match port.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if write_master(&mut master, &bytes[..count], &stop).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| CompositionError::RuntimeConstruction)
}

fn write_master(master: &mut File, bytes: &[u8], stop: &AtomicBool) -> std::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() && !stop.load(Ordering::Acquire) {
        match master.write(&bytes[written..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut poll = libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: `poll` references one initialized record for the duration of the call.
                if unsafe { libc::poll(&raw mut poll, 1, 50) } < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn spawn_output(
    port: Arc<dyn TerminalPort>,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    mut master: File,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name("hl-terminal-output".to_owned())
        .spawn(move || {
            let mut bytes = [0_u8; 8192];
            while !stop.load(Ordering::Acquire) {
                let mut poll = libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: `poll` references one initialized poll record for the duration of the call.
                let ready = unsafe { libc::poll(&raw mut poll, 1, 50) };
                if ready < 0 {
                    break;
                }
                if ready == 0 {
                    continue;
                }
                let mut active = in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let count = match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        *active += 1;
                        count
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        drop(active);
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        drop(active);
                        continue;
                    }
                    Err(_) => break,
                };
                drop(active);
                let written = write_output(port.as_ref(), &bytes[..count]);
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

fn write_output(port: &dyn TerminalPort, bytes: &[u8]) -> bool {
    let mut written = 0;
    while written < bytes.len() {
        match port.write(&bytes[written..]) {
            Ok(0) | Err(_) => return false,
            Ok(count) => written += count,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{NativeOutputBridge, NativeTerminalBridge};
    use crate::composition::{StandardStream, StandardStreamPort, Terminal, TerminalPort};
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Port {
        state: Mutex<(VecDeque<u8>, Vec<u8>, bool)>,
        changed: Condvar,
    }

    impl TerminalPort for Port {
        fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            while state.0.is_empty() && !state.2 {
                state = self.changed.wait(state).unwrap();
            }
            if state.2 {
                return Ok(0);
            }
            let count = output.len().min(state.0.len());
            for byte in &mut output[..count] {
                *byte = state.0.pop_front().unwrap();
            }
            Ok(count)
        }

        fn write(&self, input: &[u8]) -> std::io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            state.1.extend_from_slice(input);
            self.changed.notify_all();
            Ok(input.len())
        }

        fn close(&self) {
            let mut state = self.state.lock().unwrap();
            state.2 = true;
            self.changed.notify_all();
        }
    }

    struct OutputPort(AtomicBool);

    impl StandardStreamPort for OutputPort {
        fn write(&self, _stream: StandardStream, input: &[u8]) -> std::io::Result<usize> {
            Ok(input.len())
        }

        fn close(&self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn idle_output_bridge_drop_reaps_readers() {
        let port = Arc::new(OutputPort(AtomicBool::new(false)));
        let bridge = NativeOutputBridge::attach(port.clone()).unwrap();
        let (finished, completed) = mpsc::channel();
        std::thread::spawn(move || {
            drop(bridge);
            finished.send(()).unwrap();
        });
        completed
            .recv_timeout(Duration::from_secs(1))
            .expect("idle output bridge did not reap its readers");
        assert!(port.0.load(Ordering::Acquire));
    }

    #[derive(Default)]
    struct BlockingOutputPort {
        state: Mutex<(bool, bool, Vec<u8>)>,
        changed: Condvar,
    }

    impl StandardStreamPort for BlockingOutputPort {
        fn write(&self, _stream: StandardStream, input: &[u8]) -> std::io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
            state.2.extend_from_slice(input);
            Ok(input.len())
        }

        fn close(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    #[test]
    fn output_flush_waits_for_bytes_removed_from_the_pipe() {
        let port = Arc::new(BlockingOutputPort::default());
        let bridge = NativeOutputBridge::attach(port.clone()).unwrap();
        let descriptor = bridge.standard_fds()[2];
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(descriptor) };
        assert!(copy >= 0);
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut writer = unsafe { std::fs::File::from_raw_fd(copy) };
        writer.write_all(b"profile-record").unwrap();

        let mut state = port.state.lock().unwrap();
        while !state.0 {
            state = port.changed.wait(state).unwrap();
        }
        drop(state);

        let (finished, completed) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                bridge.flush();
                finished.send(()).unwrap();
            });
            assert!(completed.recv_timeout(Duration::from_millis(50)).is_err());
            let mut state = port.state.lock().unwrap();
            state.1 = true;
            port.changed.notify_all();
            drop(state);
            completed
                .recv_timeout(Duration::from_secs(1))
                .expect("output flush returned before the port accepted its bytes");
        });
        assert_eq!(port.state.lock().unwrap().2, b"profile-record");
    }

    #[test]
    fn owned_pty_binds_stdio_pumps_and_resize() {
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal.clone()).unwrap();
        let descriptors = bridge.standard_fds();
        assert_eq!(descriptors, [descriptors[0]; 3]);
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(descriptors[0]) };
        assert!(copy >= 0);
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut slave = unsafe { std::fs::File::from_raw_fd(copy) };
        slave.write_all(b"guest-output\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut state = port.state.lock().unwrap();
        while !state.1.windows(12).any(|bytes| bytes == b"guest-output") && Instant::now() < deadline {
            state = port.changed.wait_timeout(state, Duration::from_millis(20)).unwrap().0;
        }
        assert!(state.1.windows(12).any(|bytes| bytes == b"guest-output"));
        state.0.extend(b"host-input\n");
        port.changed.notify_all();
        drop(state);
        let descriptor = slave.into_raw_fd();
        // SAFETY: the descriptor is live and F_SETFL updates its status flags.
        unsafe { libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) };
        // SAFETY: ownership is immediately restored after changing flags.
        let mut slave = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let mut input = [0_u8; 64];
        let deadline = Instant::now() + Duration::from_secs(2);
        let count = loop {
            match slave.read(&mut input) {
                Ok(count) => break count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read PTY input: {error}"),
            }
        };
        assert_eq!(&input[..count], b"host-input\n");
        terminal.resize(41, 109).unwrap();
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: descriptor is a live PTY slave and size is writable.
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) },
            0
        );
        assert_eq!((size.ws_row, size.ws_col), (41, 109));
        drop(bridge);
        assert!(port.state.lock().unwrap().2);
        assert!(terminal.resize(42, 110).is_err());
    }

    struct SaturatingPort {
        closed: AtomicBool,
        enabled: AtomicBool,
        reads: AtomicUsize,
    }

    impl TerminalPort for SaturatingPort {
        fn read(&self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.closed.load(Ordering::Acquire) {
                return Ok(0);
            }
            while !self.enabled.load(Ordering::Acquire) && !self.closed.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            if self.closed.load(Ordering::Acquire) {
                return Ok(0);
            }
            self.reads.fetch_add(1, Ordering::Release);
            output.fill(b'x');
            Ok(output.len())
        }

        fn write(&self, input: &[u8]) -> std::io::Result<usize> {
            Ok(input.len())
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Release);
        }
    }

    #[test]
    fn drop_cancels_input_blocked_by_pty_backpressure() {
        let port = Arc::new(SaturatingPort {
            closed: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        });
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal).unwrap();
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: the bridge owns a live PTY slave and `attributes` is writable.
        assert_eq!(
            unsafe { libc::tcgetattr(bridge.standard_fds()[0], attributes.as_mut_ptr()) },
            0
        );
        // SAFETY: tcgetattr initialized the termios and the following calls borrow it synchronously.
        let mut attributes = unsafe { attributes.assume_init() };
        // SAFETY: `attributes` is a valid initialized termios structure.
        unsafe { libc::cfmakeraw(&raw mut attributes) };
        // SAFETY: the descriptor and termios remain live for this synchronous call.
        assert_eq!(
            unsafe { libc::tcsetattr(bridge.standard_fds()[0], libc::TCSANOW, &raw const attributes) },
            0
        );
        port.enabled.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while port.reads.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let stalled = port.reads.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            port.reads.load(Ordering::Acquire),
            stalled,
            "PTY input did not reach backpressure"
        );

        let (finished, completed) = mpsc::channel();
        std::thread::spawn(move || {
            drop(bridge);
            finished.send(()).unwrap();
        });
        completed
            .recv_timeout(Duration::from_secs(1))
            .expect("PTY bridge drop remained blocked behind a full master buffer");
    }
}
