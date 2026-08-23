#![allow(unsafe_code)]

use super::{
    Arc, AsRawFd, AtomicBool, CompositionError, File, FromRawFd, GuestDiscipline, JoinHandle, Mutex, Ordering, OwnedFd,
    Read, Terminal, TerminalAttachment, TerminalPort, Write, line_discipline, write_output,
};

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

pub(crate) struct NativeTerminalBridge {
    /// `None` once the slave has been taken for a restored member: that member's engine process receives
    /// the descriptor over `SCM_RIGHTS` and owns it from then on, and the host keeping a second copy would
    /// hold the master open past the member's exit and turn an end-of-file into a hang.
    slave: Option<OwnedFd>,
    monitor: File,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    terminal: Arc<Terminal>,
    port: Arc<dyn TerminalPort>,
    guest: Option<Arc<GuestDiscipline>>,
    workers: Vec<JoinHandle<()>>,
}

/// Which line discipline stands between the host's keystrokes and the guest's `read`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InputDiscipline {
    /// The host kernel's. What every terminal here used to get, and still the only choice for a pty
    /// whose slave this process does not keep: the discipline needs that descriptor to read the
    /// guest's termios and to raise end-of-file.
    ///
    /// On a macOS host this is the BSD discipline, whose `MAX_CANON` is 1024 and which **flushes the
    /// whole input queue** on overflow, so a pasted canonical line of 1025 bytes or more is silently
    /// destroyed while every write reports success.
    Host,
    /// The Linux `N_TTY` discipline, run in this process over a raw host slave.
    ///
    /// A raw BSD slave applies backpressure instead of flushing -- measured: the write goes short and
    /// every accepted byte is delivered, at any length -- so the channel underneath is lossless and
    /// [`super::line_discipline`] supplies the semantics the guest expects, including Linux's 4096-byte
    /// canonical line and its truncate-rather-than-discard overflow.
    Linux,
}

impl NativeTerminalBridge {
    pub(crate) fn attach(terminal: Arc<Terminal>, discipline: InputDiscipline) -> Result<Self, CompositionError> {
        let (master, slave) = open_pair(terminal.initial())?;
        set_nonblocking(&master)?;
        let input_master = duplicate(&master)?;
        let output_master = duplicate(&master)?;
        let monitor = duplicate(&master)?;
        // Built before the master is moved into the control: it needs a descriptor of its own for
        // `tcgetpgrp`, and none of the pumps' duplicates outlive a failure here.
        let guest = match discipline {
            InputDiscipline::Host => None,
            InputDiscipline::Linux => Some(GuestDiscipline::adopt(&slave, &master)?),
        };
        let control = Arc::new(NativeTerminalControl { master });
        terminal.attach(control)?;
        let stop = Arc::new(AtomicBool::new(false));
        let in_flight = Arc::new(Mutex::new(0));
        let port = terminal.port();
        let input = spawn_input(Arc::clone(&port), Arc::clone(&stop), input_master, guest.clone())?;
        let output = match spawn_output(
            Arc::clone(&port),
            Arc::clone(&stop),
            Arc::clone(&in_flight),
            output_master,
            guest.clone(),
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
            slave: Some(slave),
            monitor,
            stop,
            in_flight,
            terminal,
            port,
            guest,
            workers: vec![input, output],
        })
    }

    /// Which discipline this bridge actually ended up running.
    ///
    /// Not the one that was asked for: adopting the Linux discipline needs the engine's termios
    /// store, and a bridge that could not reach it falls back to the host's rather than running with
    /// a guest view it cannot keep truthful.
    #[cfg(test)]
    pub(super) fn discipline(&self) -> InputDiscipline {
        if self.guest.is_some() {
            InputDiscipline::Linux
        } else {
            InputDiscipline::Host
        }
    }

    pub(crate) fn standard_fds(&self) -> [i32; 3] {
        [self.slave.as_ref().expect("engine-bound terminal slave").as_raw_fd(); 3]
    }

    /// Takes the slave end, transferring it to the caller.
    ///
    /// Used for a restored member's terminal, whose slave is handed to the member's own process rather
    /// than bound as this engine's standard descriptors. A bridge whose slave has been taken must not be
    /// asked for [`Self::standard_fds`].
    pub(super) fn take_slave(&mut self) -> Option<OwnedFd> {
        self.slave.take()
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
        // After the pumps have stopped, never before: a running pump would re-assert raw mode on the
        // next input batch and undo this.
        if let Some(guest) = self.guest.as_ref() {
            guest.release();
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

pub(super) fn open_pair(initial: (u16, u16)) -> Result<(OwnedFd, OwnedFd), CompositionError> {
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
    guest: Option<Arc<GuestDiscipline>>,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name("hl-terminal-input".to_owned())
        .spawn(move || {
            let mut bytes = [0_u8; 8192];
            // One effect for the life of the pump. Its buffers are cleared, not reallocated, so a
            // steady stream of keystrokes runs the discipline without touching the allocator.
            let mut effect = line_discipline::Effect::default();
            while !stop.load(Ordering::Acquire) {
                let count = match port.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                let Some(guest) = guest.as_ref() else {
                    if write_master(&mut master, &bytes[..count], &stop).is_err() {
                        break;
                    }
                    continue;
                };
                effect.clear();
                guest.receive(&bytes[..count], &mut effect);
                if !guest.deliver(&effect, &mut master, port.as_ref(), &stop) {
                    break;
                }
            }
        })
        .map_err(|_| CompositionError::RuntimeConstruction)
}

pub(crate) fn write_master(master: &mut File, bytes: &[u8], stop: &AtomicBool) -> std::io::Result<()> {
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

/// Drains bytes which are already readable into the current display batch.
///
/// A terminal is a byte stream, not a record transport: waiting in the hope that a later guest write
/// joins this batch only adds latency. Reading until `EAGAIN` keeps bytes which are ready together
/// without a timer or a busy-spin. The fixed buffer and follow-up-attempt cap bound the work before
/// the display gets it, including under a stream of tiny reads or repeated signal interruption.
pub(super) fn drain_ready_batch(
    bytes: &mut [u8; 16 * 1024],
    count: usize,
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
) -> usize {
    const FOLLOW_UP_ATTEMPTS: usize = 8;
    let mut count = count;
    for _ in 0..FOLLOW_UP_ATTEMPTS {
        if count == bytes.len() {
            break;
        }
        match read(&mut bytes[count..]) {
            Ok(read) if read > 0 => count += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            _ => break,
        }
    }
    count
}

fn spawn_output(
    port: Arc<dyn TerminalPort>,
    stop: Arc<AtomicBool>,
    in_flight: Arc<Mutex<usize>>,
    mut master: File,
    guest: Option<Arc<GuestDiscipline>>,
) -> Result<JoinHandle<()>, CompositionError> {
    std::thread::Builder::new()
        .name("hl-terminal-output".to_owned())
        .spawn(move || {
            let mut bytes = [0_u8; 16 * 1024];
            // Reused across batches for the same reason the input pump reuses its effect.
            let mut processed = Vec::new();
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
                let mut count = match master.read(&mut bytes) {
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
                count = drain_ready_batch(&mut bytes, count, |tail| master.read(tail));
                // A raw host slave post-processes nothing, so `OPOST` is applied here or every
                // guest newline reaches the display without its carriage return.
                let written = match guest.as_ref() {
                    None => write_output(port.as_ref(), &bytes[..count]),
                    Some(guest) => {
                        guest.await_output_resumed(&stop);
                        guest.post_process(&bytes[..count], &mut processed);
                        write_output(port.as_ref(), &processed)
                    }
                };
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
