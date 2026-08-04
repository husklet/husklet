use super::{Descriptor, DescriptorSyscalls, HostError};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Signal(u8);

impl Signal {
    pub fn new(number: u8) -> Result<Self, HostError> {
        (1..=64)
            .contains(&number)
            .then_some(Self(number))
            .ok_or(HostError::Invalid)
    }

    pub(crate) const fn number(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SignalMask(u64);

impl SignalMask {
    #[must_use]
    pub fn with(mut self, signal: Signal) -> Self {
        self.0 |= 1_u64 << (signal.number() - 1);
        self
    }

    #[must_use]
    pub fn contains(self, signal: Signal) -> bool {
        self.0 & (1_u64 << (signal.number() - 1)) != 0
    }

    pub(crate) const fn bits(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalInfo {
    pub signal: Signal,
    pub code: i32,
    pub process_id: u32,
    pub user_id: u32,
    pub value: u64,
}

#[derive(Clone, Copy)]
pub struct PreviousSignalMask {
    pub(crate) words: [u64; 16],
}

impl PreviousSignalMask {
    #[must_use]
    pub fn contains(self, signal: Signal) -> bool {
        self.words[0] & (1_u64 << (signal.number() - 1)) != 0
    }
}

pub trait SignalSyscalls: DescriptorSyscalls {
    fn signal_create(&self, mask: SignalMask) -> Result<i32, HostError>;
    fn signal_read(&self, descriptor: i32) -> Result<SignalInfo, HostError>;
    fn signal_block(&self, mask: SignalMask) -> Result<PreviousSignalMask, HostError>;
    fn signal_restore(&self, previous: PreviousSignalMask) -> Result<(), HostError>;
    fn raise_current_thread(&self, signal: Signal) -> Result<(), HostError>;
}

pub struct SignalSource<S: SignalSyscalls> {
    descriptor: Descriptor<S>,
}

impl<S: SignalSyscalls> SignalSource<S> {
    pub fn create(syscalls: Arc<S>, mask: SignalMask) -> Result<Self, HostError> {
        if mask.bits() == 0 {
            return Err(HostError::Invalid);
        }
        let raw = syscalls.signal_create(mask)?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn read(&self) -> Result<SignalInfo, HostError> {
        self.descriptor.syscalls().signal_read(self.descriptor.raw())
    }
}

pub struct ThreadSignalMask<S: SignalSyscalls> {
    syscalls: Arc<S>,
    previous: Option<PreviousSignalMask>,
}

impl<S: SignalSyscalls> ThreadSignalMask<S> {
    pub fn block(syscalls: Arc<S>, mask: SignalMask) -> Result<Self, HostError> {
        if mask.bits() == 0 {
            return Err(HostError::Invalid);
        }
        let previous = syscalls.signal_block(mask)?;
        Ok(Self {
            syscalls,
            previous: Some(previous),
        })
    }

    pub fn restore(mut self) -> Result<(), HostError> {
        let previous = self.previous.take().ok_or(HostError::Invalid)?;
        self.syscalls.signal_restore(previous)
    }

    pub fn raise(&self, signal: Signal) -> Result<(), HostError> {
        self.syscalls.raise_current_thread(signal)
    }

    #[must_use]
    pub fn was_blocked(&self, signal: Signal) -> bool {
        self.previous.is_some_and(|previous| previous.contains(signal))
    }
}

impl<S: SignalSyscalls> Drop for ThreadSignalMask<S> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            let _ = self.syscalls.signal_restore(previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSignals {
        closed: Mutex<Vec<i32>>,
        masks: Mutex<Vec<u64>>,
    }

    impl DescriptorSyscalls for FakeSignals {
        fn duplicate_cloexec(&self, _: i32, _: i32) -> Result<i32, HostError> {
            Err(HostError::Unsupported)
        }

        fn close_descriptor(&self, descriptor: i32) {
            self.closed.lock().unwrap().push(descriptor);
        }
    }

    impl SignalSyscalls for FakeSignals {
        fn signal_create(&self, mask: SignalMask) -> Result<i32, HostError> {
            self.masks.lock().unwrap().push(mask.bits());
            Ok(41)
        }

        fn signal_read(&self, _: i32) -> Result<SignalInfo, HostError> {
            Err(HostError::WouldBlock)
        }

        fn signal_block(&self, mask: SignalMask) -> Result<PreviousSignalMask, HostError> {
            self.masks.lock().unwrap().push(mask.bits());
            Ok(PreviousSignalMask { words: [7; 16] })
        }

        fn signal_restore(&self, previous: PreviousSignalMask) -> Result<(), HostError> {
            self.masks.lock().unwrap().push(previous.words[0]);
            Ok(())
        }

        fn raise_current_thread(&self, _: Signal) -> Result<(), HostError> {
            Ok(())
        }
    }

    #[test]
    fn source_and_mask() {
        let syscalls = Arc::new(FakeSignals::default());
        let signal = Signal::new(10).unwrap();
        let mask = SignalMask::default().with(signal);
        {
            let _blocked = ThreadSignalMask::block(Arc::clone(&syscalls), mask).unwrap();
            let source = SignalSource::create(Arc::clone(&syscalls), mask).unwrap();
            assert_eq!(source.read(), Err(HostError::WouldBlock));
        }
        assert_eq!(*syscalls.masks.lock().unwrap(), vec![512, 512, 7]);
        assert_eq!(*syscalls.closed.lock().unwrap(), vec![41]);
    }
}
