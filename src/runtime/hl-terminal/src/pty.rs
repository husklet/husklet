use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_descriptor::{
    CancellationNotification, OperationCancellation, Readiness, ReadinessObserver, ReadinessRegistry,
    ReadinessSubscription,
};

use crate::Endpoint;
use crate::{Input, Local, Output, Settings};

pub(crate) const MAXIMUM_PAIRS: u16 = 1024;
const MAXIMUM_QUEUE: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PairId {
    pub index: u16,
    pub generation: u64,
}

/// Generation-qualified foreground process-group reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForegroundGroup {
    pub number: u32,
    pub generation: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
    Interrupt,
    Quit,
    Suspend,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteOutcome {
    pub accepted: usize,
    pub signals: Vec<Signal>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Window {
    pub rows: u16,
    pub columns: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadError {
    WouldBlock,
    Interrupted,
    Retired,
}

#[derive(Debug)]
struct State {
    settings: Settings,
    editing: VecDeque<u8>,
    input: VecDeque<u8>,
    output: VecDeque<u8>,
    eof: usize,
    foreground: Option<ForegroundGroup>,
    packet_mode: bool,
    window: Window,
    slave_references: usize,
    slave_seen: bool,
    retired: bool,
}

#[derive(Debug)]
pub struct Pair {
    id: PairId,
    state: Mutex<State>,
    changed: Condvar,
    readiness: [ReadinessRegistry; 2],
}

struct CancellationWake(Weak<Pair>);

impl CancellationNotification for CancellationWake {
    fn notify(&self) {
        if let Some(pair) = self.0.upgrade() {
            let state = pair.lock();
            pair.changed.notify_all();
            drop(state);
        }
    }
}

impl Pair {
    pub(crate) fn new(id: PairId) -> Self {
        Self {
            id,
            state: Mutex::new(State {
                settings: Settings::linux_default(),
                editing: VecDeque::new(),
                input: VecDeque::new(),
                output: VecDeque::new(),
                eof: 0,
                foreground: None,
                packet_mode: false,
                window: Window::default(),
                slave_references: 0,
                slave_seen: false,
                retired: false,
            }),
            changed: Condvar::new(),
            readiness: [ReadinessRegistry::new(), ReadinessRegistry::new()],
        }
    }

    #[must_use]
    pub const fn id(&self) -> PairId {
        self.id
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    pub fn set(&self, settings: Settings) -> Result<(), ReadError> {
        self.configure(settings, false)
    }

    pub fn configure(&self, settings: Settings, flush: bool) -> Result<(), ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        state.settings = settings;
        if flush {
            state.input.clear();
            state.editing.clear();
            state.eof = 0;
        }
        Ok(())
    }

    pub fn write_master(&self, input: &[u8]) -> Result<WriteOutcome, ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        let available = MAXIMUM_QUEUE.saturating_sub(state.input.len() + state.editing.len());
        let accepted = available.min(input.len());
        let mut signals = Vec::new();
        for byte in input.iter().copied().take(accepted) {
            let byte = if byte == b'\r' && state.settings.input.contains(Input::CR_TO_NL) {
                b'\n'
            } else {
                byte
            };
            if let Some(signal) = Self::signal(&state.settings, byte) {
                signals.push(signal);
            } else if state.settings.canonical() {
                Self::canonical(&mut state, byte);
            } else {
                state.input.push_back(byte);
            }
            if state.settings.local.contains(Local::ECHO) && state.output.len() < MAXIMUM_QUEUE {
                state.output.push_back(byte);
            }
        }
        drop(state);
        self.changed.notify_all();
        self.registry(Endpoint::Slave).notify();
        self.registry(Endpoint::Master).notify();
        Ok(WriteOutcome { accepted, signals })
    }

    pub fn read_slave(&self, output: &mut [u8]) -> Result<usize, ReadError> {
        let mut state = self.lock();
        Self::read_locked(&mut state, Endpoint::Slave, output)
    }

    pub fn write_slave(&self, input: &[u8]) -> Result<usize, ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        let mut accepted = 0;
        for byte in input.iter().copied() {
            let translated = byte == b'\n'
                && state.settings.output.contains(Output::PROCESS)
                && state.settings.output.contains(Output::NL_TO_CRNL);
            let needed = if translated { 2 } else { 1 };
            if state.output.len() + needed > MAXIMUM_QUEUE {
                break;
            }
            if translated {
                state.output.push_back(b'\r');
            }
            state.output.push_back(byte);
            accepted += 1;
        }
        drop(state);
        self.changed.notify_all();
        self.registry(Endpoint::Master).notify();
        Ok(accepted)
    }

    pub fn read_master(&self, output: &mut [u8]) -> Result<usize, ReadError> {
        let mut state = self.lock();
        Self::read_locked(&mut state, Endpoint::Master, output)
    }

    pub fn set_packet_mode(&self, enabled: bool) -> Result<(), ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        state.packet_mode = enabled;
        Ok(())
    }

    pub fn probe_read(&self, endpoint: Endpoint) -> Result<usize, ReadError> {
        let state = self.lock();
        if state.retired {
            return if endpoint == Endpoint::Slave {
                Ok(0)
            } else {
                Err(ReadError::Retired)
            };
        }
        let pending = match endpoint {
            Endpoint::Master => state.output.len(),
            Endpoint::Slave => state.input.len(),
        };
        if pending != 0 {
            return Ok(1);
        }
        match endpoint {
            Endpoint::Master if state.slave_seen && state.slave_references == 0 => Ok(0),
            Endpoint::Slave if state.eof != 0 => Ok(0),
            Endpoint::Slave if !state.settings.canonical() && state.settings.characters[Settings::MINIMUM] == 0 => {
                Ok(0)
            }
            Endpoint::Master | Endpoint::Slave => Err(ReadError::WouldBlock),
        }
    }

    pub fn read_blocking(
        self: &Arc<Self>,
        endpoint: Endpoint,
        output: &mut [u8],
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ReadError> {
        let _cancellation =
            cancellation.map(|cancellation| cancellation.subscribe(Arc::new(CancellationWake(Arc::downgrade(self)))));
        let mut state = self.lock();
        loop {
            let result = Self::read_locked(&mut state, endpoint, output);
            match result {
                Err(ReadError::WouldBlock) if nonblocking => return result,
                Err(ReadError::WouldBlock) => {
                    if cancellation.is_some_and(OperationCancellation::interrupted) {
                        return Err(ReadError::Interrupted);
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                _ => return result,
            }
        }
    }

    pub fn open_endpoint(&self, endpoint: Endpoint) {
        if endpoint != Endpoint::Slave {
            return;
        }
        let mut state = self.lock();
        state.slave_references = state.slave_references.saturating_add(1);
        state.slave_seen = true;
    }

    pub fn close_endpoint(&self, endpoint: Endpoint) {
        if endpoint != Endpoint::Slave {
            return;
        }
        let mut state = self.lock();
        state.slave_references = state.slave_references.saturating_sub(1);
        drop(state);
        self.changed.notify_all();
        self.registry(Endpoint::Master).notify();
    }

    pub fn retire(&self) {
        self.lock().retired = true;
        self.changed.notify_all();
        for registry in &self.readiness {
            registry.notify();
            registry.close();
        }
    }

    #[must_use]
    pub fn foreground(&self) -> Option<ForegroundGroup> {
        self.lock().foreground
    }

    pub fn set_foreground(&self, group: ForegroundGroup) -> Result<(), ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        state.foreground = Some(group);
        Ok(())
    }

    pub(crate) fn clear_foreground(&self) {
        self.lock().foreground = None;
    }

    #[must_use]
    pub fn window(&self) -> Window {
        self.lock().window
    }

    /// Replaces the window and reports whether the guest-visible dimensions
    /// changed. Linux emits SIGWINCH only for an actual change.
    pub fn set_window(&self, window: Window) -> Result<bool, ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        if state.window == window {
            return Ok(false);
        }
        state.window = window;
        Ok(true)
    }

    pub fn flush(&self, input: bool, output: bool) -> Result<(), ReadError> {
        let mut state = self.lock();
        if state.retired {
            return Err(ReadError::Retired);
        }
        if input {
            state.input.clear();
            state.editing.clear();
            state.eof = 0;
        }
        if output {
            state.output.clear();
        }
        drop(state);
        self.changed.notify_all();
        self.registry(Endpoint::Master).notify();
        self.registry(Endpoint::Slave).notify();
        Ok(())
    }

    #[must_use]
    pub fn pending(&self, endpoint: Endpoint) -> usize {
        let state = self.lock();
        match endpoint {
            Endpoint::Master => state.output.len(),
            Endpoint::Slave => state.input.len(),
        }
    }

    #[must_use]
    pub fn readiness(&self, endpoint: Endpoint, interests: Readiness) -> Readiness {
        let state = self.lock();
        let readable = match endpoint {
            Endpoint::Master => !state.output.is_empty() || (state.slave_seen && state.slave_references == 0),
            Endpoint::Slave => {
                let minimum = usize::from(state.settings.characters[Settings::MINIMUM]);
                state.eof != 0
                    || (!state.input.is_empty()
                        && (state.settings.canonical() || minimum == 0 || state.input.len() >= minimum))
            }
        };
        let mut available = Readiness::WRITE;
        if readable {
            available |= Readiness::READ;
        }
        if state.retired {
            available |= Readiness::HANGUP;
        }
        Readiness::from_bits(available & (interests.bits() | Readiness::HANGUP))
    }

    pub fn subscribe_readiness(
        &self,
        endpoint: Endpoint,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, hl_descriptor::ObjectError> {
        self.registry(endpoint).subscribe(observer)
    }

    fn read_locked(state: &mut State, endpoint: Endpoint, output: &mut [u8]) -> Result<usize, ReadError> {
        if state.retired {
            return Err(ReadError::Retired);
        }
        let queue = match endpoint {
            Endpoint::Master => &mut state.output,
            Endpoint::Slave => &mut state.input,
        };
        if !queue.is_empty() {
            if endpoint == Endpoint::Master && state.packet_mode {
                let Some((control, data)) = output.split_first_mut() else {
                    return Ok(0);
                };
                *control = 0;
                return Ok(1 + Self::drain(queue, data));
            }
            return Ok(Self::drain(queue, output));
        }
        match endpoint {
            Endpoint::Master if state.slave_seen && state.slave_references == 0 => Ok(0),
            Endpoint::Slave if state.eof != 0 => {
                state.eof -= 1;
                Ok(0)
            }
            Endpoint::Slave if !state.settings.canonical() && state.settings.characters[Settings::MINIMUM] == 0 => {
                Ok(0)
            }
            Endpoint::Master | Endpoint::Slave => Err(ReadError::WouldBlock),
        }
    }

    fn registry(&self, endpoint: Endpoint) -> &ReadinessRegistry {
        &self.readiness[match endpoint {
            Endpoint::Master => 0,
            Endpoint::Slave => 1,
        }]
    }

    fn canonical(state: &mut State, byte: u8) {
        let erase = state.settings.characters[Settings::ERASE];
        let eof = state.settings.characters[Settings::EOF];
        if byte == erase {
            state.editing.pop_back();
        } else if byte == eof {
            if state.editing.is_empty() {
                state.eof = state.eof.saturating_add(1);
            } else {
                state.input.extend(state.editing.drain(..));
            }
        } else {
            state.editing.push_back(byte);
            if byte == b'\n' {
                state.input.extend(state.editing.drain(..));
            }
        }
    }

    fn signal(settings: &Settings, byte: u8) -> Option<Signal> {
        if !settings.signals() {
            return None;
        }
        if byte == settings.characters[Settings::INTERRUPT] {
            Some(Signal::Interrupt)
        } else if byte == settings.characters[Settings::QUIT] {
            Some(Signal::Quit)
        } else if byte == settings.characters[Settings::SUSPEND] {
            Some(Signal::Suspend)
        } else {
            None
        }
    }

    fn drain(queue: &mut VecDeque<u8>, output: &mut [u8]) -> usize {
        let count = output.len().min(queue.len());
        for byte in output.iter_mut().take(count) {
            *byte = queue.pop_front().expect("count is bounded by queue length");
        }
        count
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::pty_catalog::{Catalog, CatalogError};
    use hl_descriptor::{CancellationNotification, CancellationSubscription};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Cancellation {
        interrupted: AtomicBool,
        notification: Mutex<Option<Arc<dyn CancellationNotification>>>,
    }

    struct Subscription;

    impl CancellationSubscription for Subscription {}

    impl OperationCancellation for Cancellation {
        fn interrupted(&self) -> bool {
            self.interrupted.swap(false, Ordering::AcqRel)
        }

        fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
            *self.notification.lock().unwrap() = Some(notification);
            Box::new(Subscription)
        }
    }

    impl Cancellation {
        fn interrupt(&self) {
            self.interrupted.store(true, Ordering::Release);
            if let Some(notification) = self.notification.lock().unwrap().clone() {
                notification.notify();
            }
        }
    }

    #[test]
    fn canonical_edit_eof() {
        let pair = Catalog::default().allocate().unwrap();
        assert_eq!(pair.write_master(b"abc\x7fX\n").unwrap().accepted, 6);
        let mut line = [0_u8; 8];
        assert_eq!(pair.read_slave(&mut line), Ok(4));
        assert_eq!(&line[..4], b"abX\n");
        pair.write_master(&[4]).unwrap();
        assert_eq!(pair.read_slave(&mut line), Ok(0));
    }

    #[test]
    fn signal_stale_identity() {
        let catalog = Catalog::default();
        let pair = catalog.allocate().unwrap();
        let id = pair.id();
        assert_eq!(pair.write_master(&[3]).unwrap().signals, [Signal::Interrupt]);
        catalog.retire(id).unwrap();
        assert_eq!(catalog.get(id).unwrap_err(), CatalogError::NotFound);
        let replacement = catalog.allocate().unwrap();
        assert_ne!(replacement.id(), id);
    }

    #[test]
    fn raw_echo_transforms() {
        let pair = Catalog::default().allocate().unwrap();
        let mut settings = pair.settings();
        settings.local = Local::from_bits(0);
        settings.input = Input::from_bits(0);
        settings.output = Output::from_bits(0);
        settings.characters[Settings::MINIMUM] = 0;
        pair.set(settings.clone()).unwrap();
        pair.write_master(b"a\r\n").unwrap();
        let mut bytes = [0_u8; 8];
        assert_eq!(pair.read_slave(&mut bytes), Ok(3));
        assert_eq!(&bytes[..3], b"a\r\n");
        assert_eq!(pair.read_master(&mut bytes), Err(ReadError::WouldBlock));

        settings.local = Local::from_bits(Local::ECHO);
        settings.input = Input::from_bits(Input::CR_TO_NL);
        settings.output = Output::from_bits(Output::PROCESS | Output::NL_TO_CRNL);
        pair.set(settings).unwrap();
        pair.write_master(b"Z\r").unwrap();
        assert_eq!(pair.read_slave(&mut bytes), Ok(2));
        assert_eq!(&bytes[..2], b"Z\n");
        assert_eq!(pair.read_master(&mut bytes), Ok(2));
        assert_eq!(&bytes[..2], b"Z\n");
        pair.write_slave(b"x\n").unwrap();
        assert_eq!(pair.read_master(&mut bytes), Ok(3));
        assert_eq!(&bytes[..3], b"x\r\n");
    }

    #[test]
    fn packet_mode_frames_master_data() {
        let pair = Catalog::default().allocate().unwrap();
        pair.set_packet_mode(true).unwrap();
        pair.write_slave(b"data").unwrap();
        let mut bytes = [0xff_u8; 3];
        assert_eq!(pair.read_master(&mut bytes), Ok(3));
        assert_eq!(&bytes, b"\0da");
        assert_eq!(pair.read_master(&mut bytes), Ok(3));
        assert_eq!(&bytes, b"\0ta");

        pair.set_packet_mode(false).unwrap();
        pair.write_slave(b"plain").unwrap();
        assert_eq!(pair.read_master(&mut bytes), Ok(3));
        assert_eq!(&bytes, b"pla");
    }

    #[test]
    fn window_flush_pending() {
        let pair = Catalog::default().allocate().unwrap();
        let window = Window {
            rows: 40,
            columns: 120,
            pixel_width: 640,
            pixel_height: 480,
        };
        assert!(pair.set_window(window).unwrap());
        assert!(!pair.set_window(window).unwrap());
        assert_eq!(pair.window(), window);
        let mut settings = pair.settings();
        settings.local = Local::from_bits(0);
        settings.characters[Settings::MINIMUM] = 0;
        pair.set(settings).unwrap();
        pair.write_master(b"five!").unwrap();
        assert_eq!(pair.pending(Endpoint::Slave), 5);
        pair.flush(true, false).unwrap();
        assert_eq!(pair.pending(Endpoint::Slave), 0);
    }

    #[test]
    fn bounded_sorted_indices() {
        let catalog = Catalog::default();
        let first = catalog.allocate().unwrap();
        let second = catalog.allocate().unwrap();
        assert_eq!(catalog.indices(), [0, 1]);
        catalog.retire(first.id()).unwrap();
        assert_eq!(catalog.indices(), [1]);
        let replacement = catalog.allocate().unwrap();
        assert_eq!(replacement.id().index, 0);
        assert_eq!(catalog.indices(), [0, 1]);
        assert_ne!(replacement.id().generation, first.id().generation);
        drop(second);
    }

    #[test]
    fn controlling_terminal_has_exclusive_session_ownership() {
        let catalog = Catalog::default();
        let first = catalog.allocate().unwrap();
        let second = catalog.allocate().unwrap();
        catalog.acquire(7, first.id()).unwrap();
        assert!(Arc::ptr_eq(&catalog.controlling(7).unwrap(), &first));
        assert_eq!(catalog.acquire(7, second.id()), Err(CatalogError::WrongEndpoint));
        assert_eq!(catalog.acquire(8, first.id()), Err(CatalogError::WrongEndpoint));
        assert_eq!(catalog.controlling_session(first.id()), Some(7));
        first
            .set_foreground(ForegroundGroup {
                number: 9,
                generation: 1,
            })
            .unwrap();
        catalog.detach(7, first.id()).unwrap();
        assert_eq!(first.foreground(), None);
        assert!(matches!(catalog.controlling(7), Err(CatalogError::NotFound)));
        catalog.acquire(8, first.id()).unwrap();
        catalog.retire(first.id()).unwrap();
        assert!(matches!(catalog.controlling(8), Err(CatalogError::NotFound)));
    }

    #[test]
    fn blocking_master() {
        let pair = Catalog::default().allocate().unwrap();
        pair.open_endpoint(Endpoint::Slave);
        let reader = Arc::clone(&pair);
        let blocked = std::thread::spawn(move || {
            let mut output = [0_u8; 8];
            let count = reader
                .read_blocking(Endpoint::Master, &mut output, false, None)
                .unwrap();
            (count, output)
        });
        std::thread::yield_now();
        pair.write_slave(b"ready").unwrap();
        let (count, output) = blocked.join().unwrap();
        assert_eq!(count, 5);
        assert_eq!(&output[..count], b"ready");
    }

    #[test]
    fn interruptible_master() {
        let pair = Catalog::default().allocate().unwrap();
        pair.open_endpoint(Endpoint::Slave);
        let mut output = [0_u8; 1];
        assert_eq!(
            pair.read_blocking(Endpoint::Master, &mut output, true, None),
            Err(ReadError::WouldBlock),
        );

        let cancellation = Arc::new(Cancellation::default());
        let reader = Arc::clone(&pair);
        let operation = Arc::clone(&cancellation);
        let blocked = std::thread::spawn(move || {
            reader.read_blocking(Endpoint::Master, &mut output, false, Some(operation.as_ref()))
        });
        while cancellation.notification.lock().unwrap().is_none() {
            std::thread::yield_now();
        }
        cancellation.interrupt();
        assert_eq!(blocked.join().unwrap(), Err(ReadError::Interrupted));
    }

    #[test]
    fn slave_eof() {
        let pair = Catalog::default().allocate().unwrap();
        pair.open_endpoint(Endpoint::Slave);
        pair.close_endpoint(Endpoint::Slave);
        let mut output = [0_u8; 1];
        assert_eq!(pair.read_blocking(Endpoint::Master, &mut output, false, None), Ok(0),);
        assert!(
            pair.readiness(Endpoint::Master, Readiness::from_bits(Readiness::READ))
                .contains(Readiness::READ)
        );
    }
}
