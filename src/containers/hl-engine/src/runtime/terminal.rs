#![allow(unsafe_code)]

use super::line_discipline::{self, LineDiscipline, Signal, Termios};
use crate::composition::{
    CompositionError, StandardStream, StandardStreamPort, Terminal, TerminalAttachment, TerminalPort,
};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[path = "terminal_output.rs"]
mod output;
pub(super) use output::NativeOutputBridge;
#[path = "terminal_bridge.rs"]
mod bridge;
#[cfg(test)]
use bridge::open_pair;
pub(crate) use bridge::write_master;
pub(super) use bridge::{InputDiscipline, NativeTerminalBridge};

/// The guest termios one pump is running, and the host projection it imposed in order to run it.
///
/// The pair is what makes "the guest changed something" decidable across a process boundary: the
/// projection is this pump's own last write to the slave, so a host termios that is not it was
/// written by the guest.
struct AdoptedTerminal {
    image: [u8; 36],
    projection: [u8; 36],
}

/// The Linux `N_TTY` discipline running over one raw host pty, and everything it needs from the host.
///
/// The host slave is raw for the whole life of the terminal, so the host kernel neither canonicalises,
/// echoes, post-processes, nor raises signals. Each of those is supplied here instead, from the
/// guest's own termios rather than the host's -- which is the point, because the two are made to
/// disagree deliberately and only the guest's is authoritative.
struct GuestDiscipline {
    /// A duplicate of the slave. Identifies the terminal to the engine's termios store, carries the
    /// raw mode, and is the descriptor whose queue a signal flush discards.
    slave: OwnedFd,
    /// A duplicate of the master, for `tcgetpgrp`. The guest's own `TIOCSPGRP` reaches a real
    /// `tcsetpgrp` on the host slave, so the host already knows the foreground group and no
    /// guest-to-host pid translation is needed to signal it.
    master: OwnedFd,
    /// The host termios the slave carried before this discipline made it raw. The bridge puts it
    /// back on the way out, so the divergence the pump creates on purpose never outlives the pump --
    /// the descriptor may be duplicated into a checkpoint capture or a restored member's engine, and
    /// a raw device with no discipline above it is not something to leave lying around.
    original: libc::termios,
    state: Mutex<LineDiscipline>,
    /// The termios generation `state` was last synchronised at. One relaxed load per input batch
    /// decides whether the engine's store has to be read again.
    generation: AtomicU64,
    /// Last input-flush event consumed from the fork-shared native ledger.
    flush_generation: AtomicU64,
    /// What this pump last adopted, and the host projection it imposed in order to run it.
    ///
    /// The engine's termios store is a plain static, and the guest runs in a **fork child** of this
    /// process -- `hl_linux_abi_spawn` in `engine/lifecycle.c`. So the guest's `TCSETS` bumps the
    /// generation and records the image in the child's private copy of that static, and this process
    /// never sees either; `the_guest_termios_store_does_not_cross_the_restore_fork` measures exactly
    /// that property for the restore fork, and the launch fork has it for the same reason.
    ///
    /// What does cross is the pty. Every TCSETS route -- `syscall/fs/control.c` and
    /// `syscall/binding/route_bound.c` alike -- reissues the guest's request as a real `tcsetattr`
    /// on this slave before recording anything, so a host termios that is no longer what this pump
    /// installed **is** the guest having installed one. That is the only signal that crosses the
    /// fork, which is why the pump keeps what it imposed rather than assuming the slave stayed raw.
    adopted: Mutex<AdoptedTerminal>,
    output_stopped: AtomicBool,
}

impl GuestDiscipline {
    /// Puts the slave in raw mode and takes over the discipline, or fails and leaves the terminal
    /// exactly as it was.
    ///
    /// Every step here needs the engine: the cooked image, the re-pairing, and the store the guest's
    /// own `TCGETS` reads back all live in it. So an engine that did not load is reported as
    /// [`CompositionError::EngineUnavailable`], naming the loader's reason, instead of leaving the
    /// caller with the host discipline and no way to tell.
    fn adopt(slave: &OwnedFd, master: &OwnedFd) -> Result<Arc<Self>, CompositionError> {
        if let Some(error) = hl_native::artifact_load_error() {
            eprintln!("[terminal] refuse: the guest line discipline needs the private engine: {error}");
            return Err(CompositionError::EngineUnavailable);
        }
        let missing = || CompositionError::RuntimeConstruction;
        let slave = duplicate_owned(slave).ok_or_else(missing)?;
        let master = duplicate_owned(master).ok_or_else(missing)?;
        // What the guest would have seen had nothing changed: a freshly opened pty's cooked termios.
        // It is captured before the slave goes raw, because afterwards the host no longer holds it.
        let mut image = [0_u8; 36];
        hl_native::terminal_termios_capture(slave.as_raw_fd(), &mut image).ok_or_else(missing)?;
        let original = attributes(&slave).ok_or_else(missing)?;
        make_raw(&slave).ok_or_else(missing)?;
        // Re-pair that cooked image with the raw projection the host now holds, so the guest's own
        // TCGETS keeps answering with a cooked terminal instead of the raw mode imposed here.
        hl_native::terminal_termios_adopt(slave.as_raw_fd(), &image).ok_or_else(missing)?;
        hl_native::terminal_termios_flush_register(slave.as_raw_fd()).ok_or_else(missing)?;
        let adopted = Mutex::new(AdoptedTerminal {
            image,
            projection: host_projection(&slave),
        });
        let flush_generation = hl_native::terminal_termios_flush_generation(slave.as_raw_fd());
        Ok(Arc::new(Self {
            slave,
            master,
            original,
            state: Mutex::new(LineDiscipline::new(Termios::from_image(&image))),
            generation: AtomicU64::new(hl_native::terminal_termios_generation()),
            flush_generation: AtomicU64::new(flush_generation),
            adopted,
            output_stopped: AtomicBool::new(false),
        }))
    }

    /// Feeds one batch of host input through the discipline, after adopting any termios the guest has
    /// installed since the last batch.
    ///
    /// Both stages append to the caller's effect, in that order, so the batch allocates nothing and
    /// copies nothing: what a keystroke costs is the discipline itself plus one uncontended lock.
    fn receive(&self, bytes: &[u8], effect: &mut line_discipline::Effect) {
        self.synchronise(effect);
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receive(bytes, effect);
    }

    /// Adopts the guest's termios when it has moved, from whichever of the two signals moved.
    ///
    /// A line editor -- `ash`'s, `readline`, every interactive shell there is -- performs a
    /// raw-mode `tcsetattr` between writing its prompt and issuing its own `read`, and restores the
    /// cooked image once the line is in. Measured on this host against the `alpine:3.20` busybox:
    /// `TCGETS`, then `TCSETS` clearing `ISIG|ICANON|ECHO`, then the read, then `TCSETS` putting
    /// them back -- once per prompt. `bash` does the same with `TCSETSW` and also clears `ICRNL`.
    /// Missing that one set is what makes the discipline draw a line the guest is about to draw
    /// again, so it is checked on every batch and not only when the store says so.
    ///
    /// **Two signals, because only one of them crosses a process boundary.**
    ///
    /// - `terminal_termios_generation` is the exact one: it carries the whole 36-byte image the
    ///   guest authored, including the bits a BSD host cannot hold. It is a plain static, so it
    ///   reports only guests sharing this address space.
    /// - The host slave's own termios is the one that crosses. Every TCSETS route reissues the
    ///   guest's request as a real `tcsetattr` on this descriptor, and the descriptor is shared with
    ///   the fork child the guest runs in, so a projection that is no longer what this pump imposed
    ///   is the guest having installed something. On Linux the host structure already **is** the
    ///   guest ABI and the reading is exact; on a BSD host it is the host's translation, so a
    ///   cross-process change is seen minus the flags `termios_m2l` cannot carry -- which is what
    ///   the host discipline would have given that terminal anyway, and strictly more than ignoring
    ///   the change.
    ///
    /// The exact signal wins when both moved, so a same-address-space guest keeps full fidelity on
    /// every host.
    fn synchronise(&self, effect: &mut line_discipline::Effect) {
        let generation = hl_native::terminal_termios_generation();
        let flush_generation = hl_native::terminal_termios_flush_generation(self.slave.as_raw_fd());
        let previous_flush = self.flush_generation.load(Ordering::Relaxed);
        let flush_input = flush_generation > previous_flush;
        if flush_generation > previous_flush {
            self.flush_generation.store(flush_generation, Ordering::Relaxed);
        }
        let mut host = [0_u8; 36];
        let read_host = hl_native::terminal_termios_capture(self.slave.as_raw_fd(), &mut host).is_some();
        let mut store = [0_u8; 36];
        let image = {
            let adopted = self.adopted.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // The generation counts installs across every terminal in the process, so a sibling
            // pump adopting its own pty moves it too. The entry for THIS terminal having changed is
            // what makes it ours; anything else falls through to the host.
            let stored = generation != self.generation.load(Ordering::Relaxed)
                && hl_native::terminal_termios(self.slave.as_raw_fd(), &mut store).is_some()
                && store != adopted.image;
            if stored {
                Some(store)
            } else if read_host && host != adopted.projection {
                Some(host)
            } else {
                None
            }
        };
        let Some(image) = image else {
            self.generation.store(generation, Ordering::Relaxed);
            if flush_input {
                let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let termios = state.termios();
                state.set_termios(termios, true, effect);
            }
            return;
        };
        // The guest's own TCSETS reached the host slave and undid the raw mode. Re-assert it before
        // any byte is written, and re-pair the guest's image with the projection that produces, so
        // the guest still reads back what it installed.
        if make_raw(&self.slave).is_some() {
            let _ = hl_native::terminal_termios_adopt(self.slave.as_raw_fd(), &image);
        }
        // After the re-assertion, never before: every write this module makes to the slave is
        // recorded here, so that the next batch reads a divergence only when somebody ELSE wrote.
        // `terminal_termios_adopt` bumps the generation itself, which is why it is re-read.
        *self.adopted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = AdoptedTerminal {
            image,
            projection: host_projection(&self.slave),
        };
        self.generation
            .store(hl_native::terminal_termios_generation(), Ordering::Relaxed);
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_termios(Termios::from_image(&image), flush_input, effect);
    }

    /// Puts the host termios back the way the pty was opened.
    fn release(&self) {
        let _ = install(&self.slave, &self.original);
    }

    /// Applies `OPOST` to one batch of guest output, replacing whatever `out` held.
    ///
    /// Only the 36-byte termios is read under the lock; the pass over the batch runs outside it. The
    /// pump reads up to 16 KiB at a time, and holding the discipline's lock across that pass put the
    /// output thread directly in the input thread's way -- every echoed keystroke returns through
    /// this path, so the two pumps met on the lock once per keystroke.
    fn post_process(&self, bytes: &[u8], out: &mut Vec<u8>) {
        let termios = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .termios();
        out.clear();
        termios.write_output(bytes, out);
    }

    /// Blocks the output pump while `IXON` has stopped output, as the host discipline would.
    fn await_output_resumed(&self, stop: &AtomicBool) {
        while self.output_stopped.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Carries out what one batch produced. Returns false when the pump should stop.
    fn deliver(
        &self,
        effect: &line_discipline::Effect,
        master: &mut File,
        port: &dyn TerminalPort,
        stop: &AtomicBool,
    ) -> bool {
        if let Some(stopped) = effect.output_stopped {
            self.output_stopped.store(stopped, Ordering::Release);
        }
        if effect.flush_input {
            // SAFETY: the discipline owns a live duplicate of the pty slave for the whole call.
            unsafe { libc::tcflush(self.slave.as_raw_fd(), libc::TCIFLUSH) };
        }
        if !effect.echo.is_empty() && !write_output(port, &effect.echo) {
            return false;
        }
        if !effect.to_guest.is_empty() && write_master(master, &effect.to_guest, stop).is_err() {
            return false;
        }
        for signal in &effect.signals {
            self.raise(*signal);
        }
        if effect.end_of_file {
            return self.raise_end_of_file(master, stop);
        }
        true
    }

    /// Delivers a terminal-generated signal to the pty's foreground process group.
    fn raise(&self, signal: Signal) {
        // SAFETY: the discipline owns a live duplicate of the pty master for the whole call.
        let group = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if group <= 0 {
            return;
        }
        let number = match signal {
            Signal::Interrupt => libc::SIGINT,
            Signal::Quit => libc::SIGQUIT,
            Signal::Suspend => libc::SIGTSTP,
        };
        // SAFETY: `killpg` takes two integers and the group was just read from the live master.
        unsafe { libc::killpg(group, number) };
    }

    /// Raises end-of-file on the guest's `read`.
    ///
    /// A raw slave has no `VEOF`, so the slave is flipped to canonical for exactly one byte, the EOF
    /// character is written, and the raw mode is restored. Measured on both hosts: the guest's read
    /// returns zero and the channel keeps working afterwards, and an idle raw slave reads `EAGAIN`
    /// first, so the zero is genuinely end-of-file rather than an empty read.
    fn raise_end_of_file(&self, master: &mut File, stop: &AtomicBool) -> bool {
        let (Some(mut attributes), Some(eof)) = (attributes(&self.slave), self.end_of_file_character()) else {
            // Nothing to raise: the guest disabled VEOF with `_POSIX_VDISABLE`, or the slave is gone.
            return true;
        };
        attributes.c_lflag |= libc::ICANON;
        attributes.c_lflag &= !(libc::ECHO | libc::ISIG);
        attributes.c_cc[libc::VEOF] = eof;
        if install(&self.slave, &attributes).is_none() {
            return true;
        }
        // The raw mode is restored whatever the write did: leaving the slave canonical would put the
        // discipline this module replaces back in the path.
        let written = write_master(master, &[eof], stop).is_ok();
        let _ = make_raw(&self.slave);
        let mut image = [0_u8; 36];
        if hl_native::terminal_termios(self.slave.as_raw_fd(), &mut image).is_some() {
            let _ = hl_native::terminal_termios_adopt(self.slave.as_raw_fd(), &image);
        }
        // Both writes above were this pump's, so the next batch must not read them as the guest's.
        self.adopted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection = host_projection(&self.slave);
        // A master that refused the end-of-file byte is gone, and so is the reason to keep pumping.
        written
    }

    fn end_of_file_character(&self) -> Option<u8> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .end_of_file_character()
    }
}

impl Drop for GuestDiscipline {
    fn drop(&mut self) {
        // Registration is acquired before the bridge attaches or starts either pump. Keeping its
        // release in Drop makes every construction failure unwind it too; the Arc held by each pump
        // also ensures it cannot be released while a pump can still consume its generation.
        hl_native::terminal_termios_flush_unregister(self.slave.as_raw_fd());
    }
}

fn duplicate_owned(descriptor: &OwnedFd) -> Option<OwnedFd> {
    // SAFETY: `dup` borrows a live descriptor and returns a fresh one on success.
    let copy = unsafe { libc::dup(descriptor.as_raw_fd()) };
    // SAFETY: a successful dup transferred a uniquely owned descriptor.
    (copy >= 0).then(|| unsafe { OwnedFd::from_raw_fd(copy) })
}

fn attributes(descriptor: &OwnedFd) -> Option<libc::termios> {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: the descriptor is live and `attributes` is writable for the whole call.
    let read = unsafe { libc::tcgetattr(descriptor.as_raw_fd(), attributes.as_mut_ptr()) };
    // SAFETY: a successful tcgetattr initialized the structure.
    (read == 0).then(|| unsafe { attributes.assume_init() })
}

fn install(descriptor: &OwnedFd, attributes: &libc::termios) -> Option<()> {
    // SAFETY: both the descriptor and the attributes stay live for this synchronous call.
    (unsafe { libc::tcsetattr(descriptor.as_raw_fd(), libc::TCSANOW, &raw const *attributes) } == 0).then_some(())
}

/// Puts a descriptor in raw mode, which is what makes the channel underneath lossless: a raw BSD
/// slave applies backpressure at its buffer instead of flushing the whole canonical queue.
/// The Linux image of the host termios `descriptor` carries right now, or all zeroes when it cannot
/// be read.
///
/// A descriptor whose termios cannot be read reads as a projection no guest can install, so the next
/// batch takes the divergence branch and re-derives everything rather than trusting a stale image.
fn host_projection(descriptor: &OwnedFd) -> [u8; 36] {
    let mut image = [0_u8; 36];
    if hl_native::terminal_termios_capture(descriptor.as_raw_fd(), &mut image).is_none() {
        image = [0_u8; 36];
    }
    image
}

fn make_raw(descriptor: &OwnedFd) -> Option<()> {
    let mut attributes = attributes(descriptor)?;
    // SAFETY: `attributes` is a valid initialized termios for the duration of the call.
    unsafe { libc::cfmakeraw(&raw mut attributes) };
    install(descriptor, &attributes)
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

/// The terminal one restored member reattaches to.
///
/// A whole-image restore rebinds every member's captured guest fds 0..2 to a live terminal, and until a
/// per-member terminal existed that could only be the restoring engine's own bridge -- one bridge for a
/// tree of many. This is the producer for a single member: an ordinary pty whose master end this host
/// pumps to and from the port it was built with, and whose slave end is handed to the member's process
/// during its descriptor restore.
///
/// It must be created BEFORE the container starts, because the member asks for it from inside that
/// restore, long before any pane exists to ask on its behalf.
#[cfg(unix)]
pub struct MemberTerminal {
    bridge: NativeTerminalBridge,
    terminal: Arc<Terminal>,
}

#[cfg(unix)]
impl MemberTerminal {
    /// Opens the pty and starts its pumps, yielding the slave end to register with the engine.
    ///
    /// The slave is returned rather than retained: the member owns it once the engine hands it over, and a
    /// copy kept here would hold the master open past the member's exit, turning an end-of-file into a
    /// hang for whoever is reading the session.
    ///
    /// # Errors
    /// Returns [`CompositionError::RuntimeConstruction`] when the pty or its pumps cannot be created.
    pub fn open(terminal: Arc<Terminal>) -> Result<(Self, OwnedFd), CompositionError> {
        // Explicitly the host discipline, and the reason is not the one this comment used to give.
        //
        // The lifetime objection -- that keeping a copy of the slave here would hold the master open
        // past the member's exit and turn an end-of-file into a hang -- is real but solvable: the
        // session that owns this terminal already observes the member's exit on its own poll, so it
        // could drop the discipline's copy at that moment and let the master see its end-of-file.
        //
        // What is NOT solvable from this side is where the guest's termios lives. A restored member
        // is a re-forked process of its own, and the engine's per-terminal termios store is a plain
        // static inside the dlopened engine, so the fork gives the member a private copy: its
        // `TCSETS` bumps ITS generation, never this process's, and
        // `the_guest_termios_store_does_not_cross_the_restore_fork` pins exactly that. A discipline
        // running here would therefore be frozen at the image the pty was opened with and would keep
        // canonicalising a line for a shell that had asked for `-icanon` -- every keystroke withheld
        // until Enter, no per-key echo, arrow keys and editors dead. That is far worse than losing a
        // line over 1024 bytes, so a restored member keeps the host discipline until the store the
        // guest writes to is one both processes can read. On macOS it therefore still loses a
        // canonical line over 1024 bytes, and `a_restored_member_terminal_falls_back_to_the_host_discipline`
        // keeps that fallback deliberate rather than accidental.
        let mut bridge = NativeTerminalBridge::attach(Arc::clone(&terminal), InputDiscipline::Host)?;
        let slave = bridge.take_slave().ok_or(CompositionError::RuntimeConstruction)?;
        Ok((Self { bridge, terminal }, slave))
    }

    /// Which discipline this member's terminal actually runs. See [`Self::open`] for why it is the
    /// host's.
    #[cfg(test)]
    pub(super) fn discipline(&self) -> InputDiscipline {
        self.bridge.discipline()
    }

    /// Resizes this member's terminal, never its container's.
    ///
    /// # Errors
    /// Returns [`CompositionError`] when the size is empty or the pty refuses the change.
    pub fn resize(&self, rows: u16, columns: u16) -> Result<(), CompositionError> {
        self.terminal.resize(rows, columns)
    }

    /// Waits for output already produced to reach the port, as the engine's own bridge does at exit.
    pub fn flush(&self) {
        self.bridge.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{InputDiscipline, NativeOutputBridge, NativeTerminalBridge, bridge::drain_ready_batch};
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
        reads: AtomicUsize,
        writes: AtomicUsize,
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
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(count)
        }

        fn write(&self, input: &[u8]) -> std::io::Result<usize> {
            self.writes.fetch_add(1, Ordering::Relaxed);
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

    #[derive(Default)]
    struct BlockingTerminalPort {
        state: Mutex<(bool, bool, Vec<u8>)>,
        changed: Condvar,
    }

    impl TerminalPort for BlockingTerminalPort {
        fn read(&self, _output: &mut [u8]) -> std::io::Result<usize> {
            let mut state = self.state.lock().unwrap();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
            Ok(0)
        }

        fn write(&self, input: &[u8]) -> std::io::Result<usize> {
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
    fn terminal_flush_waits_for_bytes_accepted_by_the_output_port() {
        let port = Arc::new(BlockingTerminalPort::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Host).unwrap();
        let descriptor = bridge.standard_fds()[1];
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(descriptor) };
        assert!(copy >= 0);
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut writer = unsafe { std::fs::File::from_raw_fd(copy) };
        writer.write_all(b"terminal-tail").unwrap();

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
                .expect("terminal flush returned before the port accepted its bytes");
        });
        assert_eq!(port.state.lock().unwrap().2, b"terminal-tail");
    }

    #[test]
    fn owned_pty_binds_stdio_pumps_and_resize() {
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal.clone(), InputDiscipline::Linux).unwrap();
        assert_eq!(
            bridge.discipline(),
            InputDiscipline::Linux,
            "the Linux discipline was not adopted, so this exercises the host's"
        );
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

    #[test]
    fn ready_terminal_output_is_drained_in_order_without_waiting_for_another_write() {
        let mut batch = [0_u8; 16 * 1024];
        batch[..3].copy_from_slice(b"abc");
        let mut reads = 0;
        let count = drain_ready_batch(&mut batch, 3, |tail| {
            reads += 1;
            match reads {
                1 => {
                    tail[..2].copy_from_slice(b"de");
                    Ok(2)
                }
                2 => Err(std::io::ErrorKind::Interrupted.into()),
                3 => {
                    tail[..3].copy_from_slice(b"fgh");
                    Ok(3)
                }
                4 => Err(std::io::ErrorKind::WouldBlock.into()),
                _ => panic!("the ready-byte drain spun after EAGAIN"),
            }
        });
        assert_eq!(reads, 4);
        assert_eq!(&batch[..count], b"abcdefgh");
    }

    #[test]
    fn ready_terminal_output_yields_after_eight_tiny_reads() {
        let mut batch = [0_u8; 16 * 1024];
        let mut reads = 0_u8;
        let count = drain_ready_batch(&mut batch, 0, |tail| {
            reads = reads.checked_add(1).expect("bounded follow-up reads");
            tail[0] = b'a' + reads - 1;
            Ok(1)
        });
        assert_eq!(reads, 8, "the ready-byte drain attempted a ninth read before yielding");
        assert_eq!(&batch[..count], b"abcdefgh");
    }

    #[test]
    fn ready_terminal_output_yields_after_eight_interruptions() {
        let mut batch = [0_u8; 16 * 1024];
        batch[..3].copy_from_slice(b"abc");
        let mut reads = 0;
        let count = drain_ready_batch(&mut batch, 3, |_| {
            reads += 1;
            Err(std::io::ErrorKind::Interrupted.into())
        });
        assert_eq!(reads, 8, "signal interruption kept the output pump from yielding");
        assert_eq!(&batch[..count], b"abc");
    }

    #[test]
    fn terminal_output_has_no_per_burst_timer_floor() {
        const BURSTS: usize = 64;
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Linux).unwrap();
        let descriptor = bridge.standard_fds()[1];
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(descriptor) };
        assert!(copy >= 0);
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut writer = unsafe { std::fs::File::from_raw_fd(copy) };
        let expected = (0..BURSTS)
            .map(|index| b'a' + u8::try_from(index % 26).unwrap())
            .collect::<Vec<_>>();
        let started = Instant::now();
        for (index, byte) in expected.iter().copied().enumerate() {
            writer.write_all(&[byte]).unwrap();
            let mut state = port.state.lock().unwrap();
            while state.1.len() <= index {
                state = port.changed.wait(state).unwrap();
            }
        }
        let elapsed = started.elapsed();
        assert_eq!(port.state.lock().unwrap().1, expected);
        let hash = port
            .state
            .lock()
            .unwrap()
            .1
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
            });
        println!(
            "terminal-burst engine n={BURSTS} elapsed_ns={} fnv64={hash:016x}",
            elapsed.as_nanos()
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "{BURSTS} acknowledged bursts took {elapsed:?}; the output pump has a per-burst timer floor"
        );
        drop(bridge);
    }

    /// Reads exactly `expected` bytes from `slave`, or panics with what it did get.
    fn read_exactly(slave: &mut std::fs::File, expected: usize) -> Vec<u8> {
        let mut collected = Vec::new();
        let mut chunk = [0_u8; 8192];
        let deadline = Instant::now() + Duration::from_secs(5);
        while collected.len() < expected && Instant::now() < deadline {
            match slave.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => collected.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut ready = libc::pollfd {
                        fd: slave.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: poll borrows one initialized descriptor record for this call.
                    assert!(unsafe { libc::poll(&raw mut ready, 1, 100) } >= 0, "poll the pty slave");
                }
                Err(error) => panic!("read the pty slave: {error}"),
            }
        }
        assert_eq!(
            collected.len(),
            expected,
            "the guest saw {} of {expected} bytes",
            collected.len()
        );
        collected
    }

    /// The defect this whole module exists for, end to end over a real pty.
    ///
    /// On a macOS host the slave carries the BSD discipline, whose `MAX_CANON` is 1024 and which
    /// **flushes the entire input queue** on overflow: a pasted line of 1025 bytes or more is
    /// silently destroyed and the shell waits forever for a command that never arrives. Linux allows
    /// 4096 and truncates instead. With the Linux discipline adopted here the guest sees the Linux
    /// answer on both hosts.
    #[test]
    fn a_pasted_canonical_line_reaches_the_guest_at_every_length() {
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Linux).unwrap();
        assert_eq!(
            bridge.discipline(),
            InputDiscipline::Linux,
            "the Linux discipline was not adopted, so this measures the host's"
        );
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(bridge.standard_fds()[0]) };
        assert!(copy >= 0);
        // SAFETY: the descriptor is live and F_SETFL only updates its status flags.
        unsafe { libc::fcntl(copy, libc::F_SETFL, libc::O_NONBLOCK) };
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut slave = unsafe { std::fs::File::from_raw_fd(copy) };

        for length in [281_usize, 1024, 1025, 1651, 4001] {
            let mut typed = vec![b'a'; length];
            typed.push(b'\n');
            port.state.lock().unwrap().0.extend(typed.iter().copied());
            port.changed.notify_all();
            let seen = read_exactly(&mut slave, length + 1);
            assert_eq!(seen, typed, "a {length}-byte canonical line did not arrive whole");
        }

        // Past the Linux capacity the line is truncated and kept, never thrown away, and the
        // terminator overwrites the last byte rather than extending past the buffer.
        let mut typed = vec![b'b'; 5001];
        typed.push(b'\n');
        port.state.lock().unwrap().0.extend(typed.iter().copied());
        port.changed.notify_all();
        let seen = read_exactly(&mut slave, crate::runtime::line_discipline::CANONICAL_CAPACITY);
        assert_eq!(seen.len(), 4096);
        assert_eq!(seen[4095], b'\n');
        assert!(seen[..4095].iter().all(|byte| *byte == b'b'));
    }

    #[test]
    fn tcsetsf_discards_the_partial_line_owned_by_the_guest_discipline() {
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Linux).unwrap();
        let slave_fd = bridge.standard_fds()[0];
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let copy = unsafe { libc::dup(slave_fd) };
        assert!(copy >= 0);
        // SAFETY: the descriptor is live and F_SETFL only updates its status flags.
        unsafe { libc::fcntl(copy, libc::F_SETFL, libc::O_NONBLOCK) };
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut slave = unsafe { std::fs::File::from_raw_fd(copy) };

        {
            let mut state = port.state.lock().unwrap();
            state.0.extend(b"old");
            port.changed.notify_all();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !state.1.ends_with(b"old") && Instant::now() < deadline {
                state = port.changed.wait_timeout(state, Duration::from_millis(20)).unwrap().0;
            }
            assert!(
                state.1.ends_with(b"old"),
                "the partial line never entered the real input pump"
            );
        }

        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: the slave is live and the output points at writable storage.
        assert_eq!(unsafe { libc::tcgetattr(slave_fd, attributes.as_mut_ptr()) }, 0);
        // SAFETY: successful tcgetattr initialized the structure.
        let attributes = unsafe { attributes.assume_init() };
        // SAFETY: the live slave and initialized attributes are borrowed for this synchronous call.
        assert_eq!(
            unsafe { libc::tcsetattr(slave_fd, libc::TCSAFLUSH, &raw const attributes) },
            0
        );
        let _ = hl_native::terminal_termios_flush_mark_test(slave_fd, 0x5404);
        {
            let mut state = port.state.lock().unwrap();
            state.0.extend(b"new\n");
            port.changed.notify_all();
        }
        assert_eq!(read_exactly(&mut slave, 4), b"new\n");

        {
            let mut state = port.state.lock().unwrap();
            state.0.extend(b"kept");
            port.changed.notify_all();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !state.1.ends_with(b"kept") && Instant::now() < deadline {
                state = port.changed.wait_timeout(state, Duration::from_millis(20)).unwrap().0;
            }
            assert!(state.1.ends_with(b"kept"));
        }
        let _ = hl_native::terminal_termios_flush_mark_test(slave_fd, 0x5402);
        let _ = hl_native::terminal_termios_flush_mark_test(slave_fd, 0x5403);
        {
            let mut state = port.state.lock().unwrap();
            state.0.extend(b"line\n");
            port.changed.notify_all();
        }
        assert_eq!(read_exactly(&mut slave, 9), b"keptline\n");
    }

    #[test]
    fn failed_bridge_attachment_releases_its_flush_registration() {
        let occupied_port = Arc::new(Port::default());
        let occupied = Terminal::new(occupied_port, 24, 80).unwrap();
        let _owner = NativeTerminalBridge::attach(occupied.clone(), InputDiscipline::Host).unwrap();
        for _ in 0..65 {
            assert!(NativeTerminalBridge::attach(occupied.clone(), InputDiscipline::Linux).is_err());
        }

        let fresh_port = Arc::new(Port::default());
        let fresh = Terminal::new(fresh_port, 24, 80).unwrap();
        let _bridge = NativeTerminalBridge::attach(fresh, InputDiscipline::Linux)
            .expect("failed attachments leaked every flush registration");
    }

    /// Keystroke-to-echo latency through the whole input pump, reported as percentiles.
    ///
    /// A measurement rather than an assertion, so it is ignored by default. Run it with
    /// `--ignored --nocapture` on the host whose latency you mean to quote, and quote that host.
    /// The reader spins: a sleeping reader measures the sleep, which has already voided one run of
    /// this measurement.
    #[test]
    #[ignore = "a latency measurement, not an assertion"]
    fn keystroke_echo_latency() {
        const WARMUP: usize = 2000;
        const SAMPLES: usize = 5000;
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Linux).unwrap();
        // Without this the run silently measures the host discipline instead, and a whole A/B
        // table can read as evidence about code none of its arms executed. That has happened.
        assert_eq!(
            bridge.discipline(),
            InputDiscipline::Linux,
            "the Linux discipline was not adopted, so this measures the host's"
        );
        let mut samples: Vec<u64> = Vec::with_capacity(SAMPLES);
        // 'a' then ERASE, so the edited line oscillates between zero and one byte: every keystroke
        // produces echo, none of them ever reaches the master, and nothing grows without bound.
        for index in 0..(WARMUP + SAMPLES) {
            let typed = if index % 2 == 0 { b'a' } else { 0x7f };
            let (seen, start) = {
                let mut state = port.state.lock().unwrap();
                let seen = state.1.len();
                state.0.push_back(typed);
                port.changed.notify_all();
                (seen, Instant::now())
            };
            let deadline = start + Duration::from_secs(5);
            loop {
                if port.state.lock().unwrap().1.len() > seen {
                    break;
                }
                assert!(Instant::now() < deadline, "the echo never arrived");
                std::hint::spin_loop();
            }
            if index >= WARMUP {
                samples.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
            }
        }
        samples.sort_unstable();
        let at = |percent: usize| samples[(samples.len() - 1) * percent / 100];
        println!(
            "latency n={} p50={}ns p90={}ns p99={}ns",
            samples.len(),
            at(50),
            at(90),
            at(99),
        );
        drop(bridge);
    }

    fn fnv64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    }

    fn queue_paste(port: &Port, bytes: &[u8], message_bytes: usize) -> usize {
        let mut messages = 0;
        for chunk in bytes.chunks(message_bytes) {
            port.state.lock().unwrap().0.extend(chunk.iter().copied());
            messages += 1;
        }
        messages
    }

    /// Large-paste throughput through the real input pump and PTY.
    ///
    /// The host arm is the native N_TTY raw control. The Linux arm keeps canonical processing in
    /// Husklet and deliberately stops below its 4096-byte line limit. Two rounds on each live
    /// bridge expose first-use allocation separately from the warm high-water mark. This is a
    /// profile, not a timing assertion: run it alone with `--ignored --nocapture` and quote the
    /// commit, host, and both byte hashes with any result.
    #[test]
    #[ignore = "a large-paste profile, not an assertion"]
    fn terminal_paste_throughput() {
        const MESSAGE_BYTES: usize = 16 * 1024;

        let raw_port = Arc::new(Port::default());
        let raw_terminal = Terminal::new(raw_port.clone(), 24, 80).unwrap();
        let raw_bridge = NativeTerminalBridge::attach(raw_terminal, InputDiscipline::Host).unwrap();
        let raw_fd = raw_bridge.standard_fds()[0];
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `raw_fd` is the bridge's live slave and `attributes` is writable.
        assert_eq!(unsafe { libc::tcgetattr(raw_fd, attributes.as_mut_ptr()) }, 0);
        // SAFETY: tcgetattr succeeded, and both calls borrow initialized termios/live fd values.
        let mut attributes = unsafe { attributes.assume_init() };
        unsafe { libc::cfmakeraw(&raw mut attributes) };
        assert_eq!(unsafe { libc::tcsetattr(raw_fd, libc::TCSANOW, &raw const attributes) }, 0);
        // SAFETY: dup returns a fresh descriptor and the result is checked.
        let raw_copy = unsafe { libc::dup(raw_fd) };
        assert!(raw_copy >= 0);
        // SAFETY: the descriptor is live and F_SETFL only updates its status flags.
        unsafe { libc::fcntl(raw_copy, libc::F_SETFL, libc::O_NONBLOCK) };
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut raw_slave = unsafe { std::fs::File::from_raw_fd(raw_copy) };

        for round in 0..2 {
            for length in [1024_usize, 8 * 1024, 64 * 1024, 1024 * 1024] {
                let end_to_end_started = Instant::now();
                let bytes = (0..length).map(|index| (index % 251) as u8).collect::<Vec<_>>();
                let reads_before = raw_port.reads.load(Ordering::Relaxed);
                let messages = queue_paste(raw_port.as_ref(), &bytes, MESSAGE_BYTES);
                let receiver_started = Instant::now();
                raw_port.changed.notify_all();
                let seen = read_exactly(&mut raw_slave, length);
                let reads = raw_port.reads.load(Ordering::Relaxed) - reads_before;
                assert_eq!(seen, bytes, "raw paste changed bytes");
                println!(
                    "paste host-ntty-control round={round} bytes={length} receiver_ns={} end_to_end_ns={} messages={messages} read_calls={reads} master_batches={reads} fnv64={:016x}",
                    receiver_started.elapsed().as_nanos(),
                    end_to_end_started.elapsed().as_nanos(),
                    fnv64(&seen)
                );
            }
        }
        drop(raw_bridge);

        let canonical_port = Arc::new(Port::default());
        let canonical_terminal = Terminal::new(canonical_port.clone(), 24, 80).unwrap();
        let canonical_bridge = NativeTerminalBridge::attach(canonical_terminal, InputDiscipline::Linux).unwrap();
        assert_eq!(canonical_bridge.discipline(), InputDiscipline::Linux);
        let canonical_copy = unsafe { libc::dup(canonical_bridge.standard_fds()[0]) };
        assert!(canonical_copy >= 0);
        // SAFETY: the descriptor is live and F_SETFL only updates its status flags.
        unsafe { libc::fcntl(canonical_copy, libc::F_SETFL, libc::O_NONBLOCK) };
        // SAFETY: successful dup transferred a uniquely owned descriptor.
        let mut canonical_slave = unsafe { std::fs::File::from_raw_fd(canonical_copy) };
        for round in 0..2 {
            for length in [1024_usize, 2048, 4000] {
                let end_to_end_started = Instant::now();
                let mut bytes = vec![b'p'; length];
                bytes.push(b'\n');
                let reads_before = canonical_port.reads.load(Ordering::Relaxed);
                let writes_before = canonical_port.writes.load(Ordering::Relaxed);
                let messages = queue_paste(canonical_port.as_ref(), &bytes, MESSAGE_BYTES);
                let receiver_started = Instant::now();
                canonical_port.changed.notify_all();
                let seen = read_exactly(&mut canonical_slave, bytes.len());
                let reads = canonical_port.reads.load(Ordering::Relaxed) - reads_before;
                let writes = canonical_port.writes.load(Ordering::Relaxed) - writes_before;
                assert_eq!(seen, bytes, "canonical paste changed bytes");
                assert_eq!(reads, 1, "canonical paste unexpectedly crossed more than one master batch");
                println!(
                    "paste canonical round={round} bytes={} receiver_ns={} end_to_end_ns={} messages={messages} read_calls={reads} master_batches={reads} echo_writes={writes} fnv64={:016x}",
                    bytes.len(),
                    receiver_started.elapsed().as_nanos(),
                    end_to_end_started.elapsed().as_nanos(),
                    fnv64(&seen)
                );
            }
        }
        drop(canonical_bridge);
    }

    /// The two line-editor shapes measured on this host, as the `tcsetattr` each performs between
    /// writing its prompt and issuing its own `read`.
    ///
    /// Both were taken from `strace -e trace=ioctl` against a real pty on `naa0245`: the
    /// `alpine:3.20` busybox from `/var/tmp/compat-sse-rootfs/alpine` and `bash 5.2 --norc -i`.
    /// They are not guesses about what a shell might do -- they are what these two do at every
    /// prompt, and the reason a pump that misses one draws a line the guest is about to draw again.
    #[derive(Clone, Copy)]
    enum LineEditor {
        /// busybox `ash`: `TCSETS` (`TCSANOW`) clearing `ISIG|ICANON|ECHO`, leaving `ICRNL` and
        /// `IXON` alone, and a cursor-position report at every prompt.
        BusyboxAsh,
        /// `bash`/`readline`: `TCSETSW` (`TCSADRAIN`) clearing `ICANON|ECHO` **and `ICRNL`**, and
        /// keeping `ISIG`. `readline` has no `-echo` bail-out of the kind busybox carries, so it
        /// draws the line back whatever the guest's `ECHO` says.
        BashReadline,
    }

    impl LineEditor {
        fn action(self) -> libc::c_int {
            match self {
                Self::BusyboxAsh => libc::TCSANOW,
                Self::BashReadline => libc::TCSADRAIN,
            }
        }

        /// Whether the editor asks the display where the cursor is, which is what puts a reply on
        /// the input side that is a *reply* and not a keystroke.
        fn asks_for_the_cursor(self) -> bool {
            matches!(self, Self::BusyboxAsh)
        }

        fn raw(self, saved: &libc::termios) -> libc::termios {
            let mut raw = *saved;
            match self {
                Self::BusyboxAsh => raw.c_lflag &= !(libc::ISIG | libc::ICANON | libc::ECHO),
                Self::BashReadline => {
                    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
                    raw.c_iflag &= !libc::ICRNL;
                }
            }
            // VMIN 0 with a five-second VTIME rather than the VMIN 1 a real editor sets: a wedged
            // child must end the test rather than hang the suite, and a zero-byte read is the only
            // bounded way for it to notice.
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 50;
            raw
        }
    }

    /// What one prompt of a line editor put on the display, and whether the editor saw its line.
    struct Prompt {
        display: Vec<u8>,
        editor_exit: i32,
    }

    /// Drives one prompt of `editor` against a real pump over a real pty, with the editor in a
    /// **fork child** -- which is where the guest actually runs.
    ///
    /// `hl_linux_abi_spawn` (`engine/lifecycle.c`) enters the Linux personality in a fork child, so
    /// the engine's termios store records the guest's `TCSETS` in a private copy of a static this
    /// process cannot read. The child here therefore calls `tcsetattr` and nothing else: recording
    /// into its own copy of the store is exactly as invisible to this process as not recording at
    /// all, and leaving the call out keeps the child free of every lock a fork of a threaded process
    /// must not take.
    fn one_prompt(editor: LineEditor) -> Prompt {
        const TYPED: &[u8] = b"ls -la";
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port.clone(), 24, 80).unwrap();
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Linux).unwrap();
        assert_eq!(
            bridge.discipline(),
            InputDiscipline::Linux,
            "the Linux discipline was not adopted, so this exercises the host's"
        );
        let slave = bridge.standard_fds()[0];

        // SAFETY: the child performs only `tcgetattr`/`tcsetattr`/`read`/`write`/`_exit` on an
        // inherited descriptor. It allocates nothing, takes no Rust lock and no engine lock, and
        // never returns into the harness, so no lock this threaded process held at fork time is
        // ever acquired in it.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork the line editor");
        if child == 0 {
            // SAFETY: every call below borrows an inherited descriptor and initialized storage for
            // the duration of the call, and the process leaves through `_exit`.
            unsafe {
                let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
                if libc::tcgetattr(slave, saved.as_mut_ptr()) != 0 {
                    libc::_exit(11);
                }
                let saved = saved.assume_init();
                let editing = editor.raw(&saved);
                if libc::tcsetattr(slave, editor.action(), &raw const editing) != 0 {
                    libc::_exit(12);
                }
                let prompt = b"$ ";
                libc::write(slave, prompt.as_ptr().cast(), prompt.len());
                if editor.asks_for_the_cursor() {
                    let query = b"\x1b[6n";
                    libc::write(slave, query.as_ptr().cast(), query.len());
                }
                let mut seen = [0_u8; 64];
                let mut count = 0_usize;
                let mut byte = 0_u8;
                let mut escaped = false;
                let mut control_sequence = false;
                let status = loop {
                    let read = libc::read(slave, (&raw mut byte).cast(), 1);
                    if read != 1 {
                        break 13; // VTIME expired, or the master went away
                    }
                    if escaped {
                        // An editor consumes its own reply rather than drawing it. `ESC [` opens a
                        // control sequence that runs to a final byte in 0x40..=0x7e; the `[` is
                        // itself in that range, so it has to open the sequence rather than close it.
                        if control_sequence {
                            if (0x40..=0x7e).contains(&byte) {
                                escaped = false;
                                control_sequence = false;
                            }
                        } else if byte == b'[' {
                            control_sequence = true;
                        } else {
                            escaped = false;
                        }
                        continue;
                    }
                    if byte == 0x1b {
                        escaped = true;
                        continue;
                    }
                    if byte == b'\r' || byte == b'\n' {
                        break i32::from(count != TYPED.len() || seen[..count] != *TYPED);
                    }
                    if count == seen.len() {
                        break 14;
                    }
                    seen[count] = byte;
                    count += 1;
                    // The editor's own echo. This is the copy the developer is meant to see.
                    libc::write(slave, (&raw const byte).cast(), 1);
                };
                let done = b"\r\n";
                libc::write(slave, done.as_ptr().cast(), done.len());
                libc::tcsetattr(slave, libc::TCSANOW, &raw const saved);
                libc::_exit(status);
            }
        }

        // The prompt is written after the `tcsetattr`, so seeing it on the display is proof the
        // editor's raw mode is installed and the keystrokes below cannot race it.
        let deadline = Instant::now() + Duration::from_secs(10);
        {
            let mut state = port.state.lock().unwrap();
            while !state.1.windows(2).any(|bytes| bytes == b"$ ") && Instant::now() < deadline {
                state = port.changed.wait_timeout(state, Duration::from_millis(20)).unwrap().0;
            }
            assert!(
                state.1.windows(2).any(|bytes| bytes == b"$ "),
                "the line editor never reached its prompt"
            );
            // The display answering the cursor-position query, then the developer typing. Both
            // arrive on the input side; only the second is a keystroke.
            if editor.asks_for_the_cursor() {
                state.0.extend(b"\x1b[24;5R");
            }
            state.0.extend(TYPED.iter().copied());
            state.0.push_back(b'\r');
            port.changed.notify_all();
        }

        let mut status = 0;
        let mut reaped = false;
        while Instant::now() < deadline {
            // SAFETY: `status` is writable and `child` is this process's child.
            let waited = unsafe { libc::waitpid(child, &raw mut status, libc::WNOHANG) };
            if waited == child {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !reaped {
            // SAFETY: `child` is this process's child and has not been reaped.
            unsafe { libc::kill(child, libc::SIGKILL) };
            // SAFETY: as above; the kill guarantees this returns.
            unsafe { libc::waitpid(child, &raw mut status, 0) };
            panic!("the line editor never finished its line");
        }
        // Let the editor's closing bytes finish their trip through the output pump.
        std::thread::sleep(Duration::from_millis(100));
        let display = port.state.lock().unwrap().1.clone();
        drop(bridge);
        Prompt {
            display,
            editor_exit: libc::WEXITSTATUS(status),
        }
    }

    fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        let mut count = 0;
        let mut index = 0;
        while index + needle.len() <= haystack.len() {
            if &haystack[index..index + needle.len()] == needle {
                count += 1;
                index += needle.len();
            } else {
                index += 1;
            }
        }
        count
    }

    /// A command line typed at a guest line editor is drawn **once**, and the display's own reply
    /// to a cursor-position query is never drawn at all.
    ///
    /// Both are the same defect. The guest runs in a fork child, so the engine's termios store --
    /// the signal [`GuestDiscipline::synchronise`] once read alone -- records the editor's raw-mode
    /// `tcsetattr` somewhere this process cannot see it. The pump therefore stayed on the cooked
    /// image it captured when the pty was opened and kept `ICANON|ECHO|ECHOCTL` forever: it drew
    /// every keystroke, held the line back until the terminator, and handed the whole line to an
    /// editor which then drew it a second time. The cursor report is the same mechanism reached
    /// from the other side -- a reply arriving on the input side, echoed in `ECHOCTL`'s caret form
    /// as though the developer had typed `^[[24;5R`, instead of being passed through to the editor
    /// that asked for it.
    ///
    /// The editor's exit status is asserted first and on purpose: without it, a pump that echoed
    /// nothing and delivered nothing would satisfy every other assertion here.
    fn one_line_is_drawn_once(editor: LineEditor) {
        let prompt = one_prompt(editor);
        let display = String::from_utf8_lossy(&prompt.display).into_owned();
        assert_eq!(
            prompt.editor_exit, 0,
            "the line editor did not receive exactly the line that was typed; it saw {display:?}"
        );
        assert_eq!(
            occurrences(&prompt.display, b"ls -la"),
            1,
            "the command line was drawn more than once: {display:?}"
        );
        assert!(
            !prompt.display.windows(2).any(|bytes| bytes == b"^["),
            "an escape sequence reached the display in echoed caret form: {display:?}"
        );
        assert!(
            !prompt.display.windows(3).any(|bytes| bytes == b";5R"),
            "the cursor-position report was echoed back to the display: {display:?}"
        );
    }

    /// busybox `ash`, which is what the default image ships. Carries the cursor-position report.
    #[test]
    fn a_busybox_line_editor_draws_its_line_once_across_the_launch_fork() {
        one_line_is_drawn_once(LineEditor::BusyboxAsh);
    }

    /// `bash`/`readline`, which is the first thing a developer installs. `TCSADRAIN` rather than
    /// `TCSANOW`, and `ICRNL` cleared as well, so a pump that follows only one of the two spellings
    /// passes here and fails there.
    #[test]
    fn a_readline_line_editor_draws_its_line_once_across_the_launch_fork() {
        one_line_is_drawn_once(LineEditor::BashReadline);
    }

    /// A restored member's terminal runs the HOST discipline, deliberately.
    ///
    /// It is the terminal a reattached pane is seated on after a Continue-later restore, so on macOS
    /// it still destroys a canonical line of 1025 bytes or more -- the defect
    /// [`a_pasted_canonical_line_reaches_the_guest_at_every_length`] exists to prevent. This pins the
    /// fallback so that flipping it to [`InputDiscipline::Linux`] is a decision somebody has to make
    /// against the reason in [`MemberTerminal::open`], rather than a one-word edit that reads like an
    /// improvement and leaves a re-forked member's shell stuck in canonical mode.
    #[test]
    fn a_restored_member_terminal_falls_back_to_the_host_discipline() {
        let port = Arc::new(Port::default());
        let terminal = Terminal::new(port, 24, 80).unwrap();
        let (member, slave) = super::MemberTerminal::open(terminal).expect("member terminal");
        assert_eq!(
            member.discipline(),
            InputDiscipline::Host,
            "a restored member's terminal must keep the host discipline while the guest's termios \
             store does not cross the restore fork"
        );
        drop(slave);
        drop(member);
    }

    /// The engine's per-terminal termios store does not cross a fork, which is why the discipline
    /// above cannot be the Linux one.
    ///
    /// A whole-image restore re-forks every captured process, so a restored member runs in a process
    /// of its own with a private copy of the engine's statics. Its guest's `TCSETS` is recorded
    /// there. This is the measurement, not the assumption: a child records a different image for the
    /// SAME terminal device -- same `dev`/`ino`, which is all the store keys on -- and the parent
    /// must still read back its own.
    ///
    /// The entry is what is asserted, not the generation counter: that counter is one number for
    /// every terminal in the process, so a sibling test adopting its own pty moves it and an
    /// assertion on it fails under `cargo test` while passing when run alone. It did.
    #[test]
    fn the_guest_termios_store_does_not_cross_the_restore_fork() {
        if let Some(error) = hl_native::artifact_load_error() {
            eprintln!("termios store fork check skipped: the engine is unavailable: {error}");
            return;
        }
        let (master, slave) = super::open_pair((24, 80)).expect("pty pair");
        let mut parent_image = [0_u8; 36];
        assert!(
            hl_native::terminal_termios_capture(slave.as_raw_fd(), &mut parent_image).is_some(),
            "capture the host termios of a fresh pty"
        );
        // A bit no fresh pty carries, so the two images cannot be confused for one another.
        parent_image[16] = 0x5a;
        hl_native::terminal_termios_adopt(slave.as_raw_fd(), &parent_image).expect("adopt in the parent");

        let mut child_image = parent_image;
        child_image[16] = 0xa5;
        // SAFETY: the child performs one FFI call and `_exit`s. It allocates nothing, takes no Rust
        // lock, and never returns into the test harness, so no lock this process held at fork time
        // is ever acquired in it.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork a stand-in for a restored member");
        if child == 0 {
            let recorded = hl_native::terminal_termios_adopt(slave.as_raw_fd(), &child_image).is_some();
            // SAFETY: `_exit` performs no cleanup, which is the point.
            unsafe { libc::_exit(i32::from(!recorded)) };
        }
        let mut status = 0;
        // SAFETY: `status` is writable and `child` is this process's child.
        assert!(unsafe { libc::waitpid(child, &raw mut status, 0) } == child);
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "the child could not record a guest termios of its own"
        );

        let mut seen = [0_u8; 36];
        assert!(
            hl_native::terminal_termios(slave.as_raw_fd(), &mut seen).is_some(),
            "the parent lost its own entry"
        );
        assert_eq!(
            seen, parent_image,
            "a re-forked member's guest termios reached this process's store, so the pump could \
             follow it and MemberTerminal could run the Linux discipline"
        );
        drop(master);
        drop(slave);
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
        let bridge = NativeTerminalBridge::attach(terminal, InputDiscipline::Host).unwrap();
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
