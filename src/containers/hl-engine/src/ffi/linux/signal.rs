use super::{ErrnoMapper, LinuxHost, abi};
use crate::native_host::{
    HostError, NativeSignal, NativeSignalInfo, NativeSignalMask, PreviousSignalMask, SignalSyscalls,
};

const NONBLOCK: i32 = 0x800;
const CLOEXEC: i32 = 0x80000;
const SIG_BLOCK: i32 = 0;
const SIG_SETMASK: i32 = 2;

#[repr(C)]
struct SignalFdInfo {
    signal: u32,
    error: i32,
    code: i32,
    process_id: u32,
    user_id: u32,
    descriptor: i32,
    thread_id: u32,
    band: u32,
    overrun: u32,
    trap_number: u32,
    status: i32,
    integer: i32,
    pointer: u64,
    user_time: u64,
    system_time: u64,
    address: u64,
    address_lsb: u16,
    padding: u16,
    syscall: i32,
    call_address: u64,
    architecture: u32,
    reserved: [u8; 28],
}

#[repr(C)]
struct LibcSignalSet {
    words: [u64; 16],
}

impl SignalSyscalls for LinuxHost {
    fn signal_create(&self, mask: NativeSignalMask) -> Result<i32, HostError> {
        let bits = mask.bits();
        // SAFETY: bits is an initialized Linux 64-bit signal set, consumed only
        // for this call. The returned descriptor transfers to NativeDescriptor.
        let descriptor = unsafe { signalfd(-1, &raw const bits, NONBLOCK | CLOEXEC) };
        (descriptor >= 0).then_some(descriptor).ok_or_else(ErrnoMapper::current)
    }

    fn signal_read(&self, descriptor: i32) -> Result<NativeSignalInfo, HostError> {
        let mut info = SignalFdInfo::zeroed();
        // SAFETY: info is aligned, initialized, and uniquely writable for its
        // exact 128-byte ABI size. No pointer is retained and libc cannot unwind.
        let count = unsafe {
            abi::read(
                descriptor,
                (&raw mut info).cast(),
                core::mem::size_of::<SignalFdInfo>(),
            )
        };
        if count < 0 {
            return Err(ErrnoMapper::current());
        }
        if count as usize != core::mem::size_of::<SignalFdInfo>() {
            return Err(HostError::Failed);
        }
        Ok(NativeSignalInfo {
            signal: NativeSignal::new(u8::try_from(info.signal).map_err(|_| HostError::Failed)?)?,
            code: info.code,
            process_id: info.process_id,
            user_id: info.user_id,
            value: info.pointer,
        })
    }

    fn signal_block(&self, mask: NativeSignalMask) -> Result<PreviousSignalMask, HostError> {
        let mut selected = LibcSignalSet { words: [0; 16] };
        selected.words[0] = mask.bits();
        let mut previous = LibcSignalSet { words: [0; 16] };
        // SAFETY: both signal sets have the libc sigset_t size and alignment,
        // remain live for the call, and are not retained. pthread_sigmask
        // affects only the calling thread and cannot unwind.
        let result = unsafe { pthread_sigmask(SIG_BLOCK, &raw const selected, &raw mut previous) };
        if result != 0 {
            return Err(LibcSignalSet::error(result));
        }
        Ok(PreviousSignalMask { words: previous.words })
    }

    fn signal_restore(&self, previous: PreviousSignalMask) -> Result<(), HostError> {
        let previous = LibcSignalSet { words: previous.words };
        // SAFETY: previous is a complete initialized libc signal set, borrowed
        // only for this call. pthread_sigmask cannot unwind.
        let result = unsafe { pthread_sigmask(SIG_SETMASK, &raw const previous, core::ptr::null_mut()) };
        (result == 0).then_some(()).ok_or_else(|| LibcSignalSet::error(result))
    }

    fn raise_current_thread(&self, signal: NativeSignal) -> Result<(), HostError> {
        // SAFETY: pthread_self returns the calling thread identity and
        // pthread_kill consumes it synchronously without retaining storage.
        let result = unsafe { pthread_kill(pthread_self(), i32::from(signal.number())) };
        (result == 0).then_some(()).ok_or_else(|| LibcSignalSet::error(result))
    }
}

impl SignalFdInfo {
    fn zeroed() -> Self {
        Self {
            signal: 0,
            error: 0,
            code: 0,
            process_id: 0,
            user_id: 0,
            descriptor: 0,
            thread_id: 0,
            band: 0,
            overrun: 0,
            trap_number: 0,
            status: 0,
            integer: 0,
            pointer: 0,
            user_time: 0,
            system_time: 0,
            address: 0,
            address_lsb: 0,
            padding: 0,
            syscall: 0,
            call_address: 0,
            architecture: 0,
            reserved: [0; 28],
        }
    }
}

impl LibcSignalSet {
    fn error(number: i32) -> HostError {
        match number {
            4 => HostError::Interrupted,
            11 => HostError::WouldBlock,
            22 => HostError::Invalid,
            _ => HostError::Failed,
        }
    }
}

unsafe extern "C" {
    fn signalfd(descriptor: i32, mask: *const u64, flags: i32) -> i32;
    fn pthread_sigmask(operation: i32, selected: *const LibcSignalSet, previous: *mut LibcSignalSet) -> i32;
    fn pthread_self() -> usize;
    fn pthread_kill(thread: usize, signal: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::SignalFdInfo;

    #[test]
    fn signalfd_record_matches() {
        assert_eq!(core::mem::size_of::<SignalFdInfo>(), 128);
        assert_eq!(core::mem::align_of::<SignalFdInfo>(), 8);
    }
}
