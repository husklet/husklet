#![allow(unsafe_code)]

use crate::composition::{CompositionError, Terminal, TerminalAttachment, TerminalPort};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

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
    stop: Arc<AtomicBool>,
    port: Arc<dyn TerminalPort>,
    workers: Vec<JoinHandle<()>>,
}

impl NativeTerminalBridge {
    pub(super) fn attach(terminal: Arc<Terminal>) -> Result<Self, CompositionError> {
        let (master, slave) = open_pair(terminal.initial())?;
        let input_master = duplicate(&master)?;
        let output_master = duplicate(&master)?;
        let control = Arc::new(NativeTerminalControl { master });
        terminal.attach(control)?;
        let stop = Arc::new(AtomicBool::new(false));
        let port = terminal.port();
        let input = spawn_input(Arc::clone(&port), Arc::clone(&stop), input_master)?;
        let output = match spawn_output(Arc::clone(&port), Arc::clone(&stop), output_master) {
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
            stop,
            port,
            workers: vec![input, output],
        })
    }

    pub(super) fn standard_fds(&self) -> [i32; 3] {
        [self.slave.as_raw_fd(); 3]
    }
}

impl Drop for NativeTerminalBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.port.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn open_pair(initial: (u16, u16)) -> Result<(OwnedFd, OwnedFd), CompositionError> {
    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
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
            std::ptr::null(),
            &raw const size,
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
                if master.write_all(&bytes[..count]).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| CompositionError::RuntimeConstruction)
}

fn spawn_output(
    port: Arc<dyn TerminalPort>,
    stop: Arc<AtomicBool>,
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
                let count = match master.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if !write_output(port.as_ref(), &bytes[..count]) {
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
    use super::NativeTerminalBridge;
    use crate::composition::{Terminal, TerminalPort};
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::sync::{Arc, Condvar, Mutex};
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
    }
}
