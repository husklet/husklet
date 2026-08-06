use super::{ErrnoMapper, LinuxHost, abi};
use crate::native_host::{EventSyscalls, HostError, TimerSetting};

const EFD_SEMAPHORE: i32 = 1;
const NONBLOCK: i32 = 0x800;
const CLOEXEC: i32 = 0x80000;
const EPOLLIN: u32 = 1;
const EPOLLOUT: u32 = 4;
const EPOLLERR: u32 = 8;
const EPOLLHUP: u32 = 0x10;
const EPOLLET: u32 = 1 << 31;
const EPOLLONESHOT: u32 = 1 << 30;

#[repr(C)]
#[cfg_attr(target_arch = "x86_64", repr(packed))]
#[derive(Clone, Copy)]
struct EpollEvent {
    events: u32,
    token: u64,
}

#[repr(C)]
struct TimerSpec {
    interval: abi::timespec,
    initial: abi::timespec,
}

impl EventSyscalls for LinuxHost {
    fn event_create(&self, initial: u64, semaphore: bool) -> Result<i32, HostError> {
        let initial = u32::try_from(initial).map_err(|_| HostError::Invalid)?;
        let flags = NONBLOCK | CLOEXEC | if semaphore { EFD_SEMAPHORE } else { 0 };
        // SAFETY: eventfd receives scalar values and returns an owned descriptor.
        let descriptor = unsafe { eventfd(initial, flags) };
        (descriptor >= 0).then_some(descriptor).ok_or_else(ErrnoMapper::current)
    }

    fn event_read(&self, descriptor: i32) -> Result<u64, HostError> {
        let mut value = 0_u64;
        // SAFETY: value is uniquely writable for exactly eight bytes.
        let count = unsafe { abi::read(descriptor, (&raw mut value).cast(), core::mem::size_of::<u64>()) };
        if count == 8 {
            Ok(value)
        } else if count < 0 {
            Err(ErrnoMapper::current())
        } else {
            Err(HostError::Failed)
        }
    }

    fn event_write(&self, descriptor: i32, value: u64) -> Result<(), HostError> {
        // SAFETY: value is readable for exactly eight bytes.
        let count = unsafe { abi::write(descriptor, (&raw const value).cast(), core::mem::size_of::<u64>()) };
        if count == 8 {
            Ok(())
        } else if count < 0 {
            Err(ErrnoMapper::current())
        } else {
            Err(HostError::Failed)
        }
    }

    fn timer_create(&self) -> Result<i32, HostError> {
        // SAFETY: timerfd_create receives scalar clock and flags.
        let descriptor = unsafe { timerfd_create(1, NONBLOCK | CLOEXEC) };
        (descriptor >= 0).then_some(descriptor).ok_or_else(ErrnoMapper::current)
    }

    fn timer_set(&self, descriptor: i32, setting: TimerSetting) -> Result<(), HostError> {
        let setting = TimerSpec {
            interval: EpollConversion::time(setting.interval_ns)?,
            initial: EpollConversion::time(setting.initial_ns)?,
        };
        // SAFETY: setting is initialized and borrowed for the synchronous call.
        let result = unsafe { timerfd_settime(descriptor, 0, &raw const setting, core::ptr::null_mut()) };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }

    fn timer_read(&self, descriptor: i32) -> Result<u64, HostError> {
        self.event_read(descriptor)
    }

    fn poll_create(&self) -> Result<i32, HostError> {
        // SAFETY: epoll_create1 receives scalar flags and returns an owned descriptor.
        let descriptor = unsafe { epoll_create1(CLOEXEC) };
        (descriptor >= 0).then_some(descriptor).ok_or_else(ErrnoMapper::current)
    }

    fn poll_control(&self, poll: i32, source: i32, operation: u8, interests: u32, token: u64) -> Result<(), HostError> {
        let operation = match operation {
            1 => 1,
            2 => 3,
            3 => 2,
            _ => return Err(HostError::Invalid),
        };
        let mut event = EpollConversion::subscription(interests, token);
        let event_pointer = if operation == 2 {
            core::ptr::null_mut()
        } else {
            &mut event
        };
        // SAFETY: event is live for ADD/MOD and Linux ignores it for DEL.
        let result = unsafe { epoll_ctl(poll, operation, source, event_pointer) };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }

    fn poll_wait(&self, poll: i32, timeout_ms: i32, events: &mut [(u32, u64)]) -> Result<usize, HostError> {
        let mut native = vec![EpollEvent { events: 0, token: 0 }; events.len()];
        // SAFETY: native is uniquely writable for its exact declared capacity.
        let count = unsafe {
            epoll_wait(
                poll,
                native.as_mut_ptr(),
                i32::try_from(native.len()).map_err(|_| HostError::Invalid)?,
                timeout_ms,
            )
        };
        let count: usize = count.try_into().map_err(|_| ErrnoMapper::current())?;
        for (output, event) in events.iter_mut().zip(native).take(count) {
            *output = EpollConversion::ready(event);
        }
        Ok(count)
    }
}

struct EpollConversion;

impl EpollConversion {
    fn time(nanoseconds: u64) -> Result<abi::timespec, HostError> {
        Ok(abi::timespec {
            tv_sec: i64::try_from(nanoseconds / 1_000_000_000).map_err(|_| HostError::Invalid)?,
            tv_nsec: (nanoseconds % 1_000_000_000) as i64,
        })
    }

    fn subscription(interests: u32, token: u64) -> EpollEvent {
        let events = (if interests & 1 != 0 { EPOLLIN } else { 0 })
            | (if interests & 2 != 0 { EPOLLOUT } else { 0 })
            | (if interests & 4 != 0 { EPOLLET } else { 0 })
            | (if interests & 8 != 0 { EPOLLONESHOT } else { 0 });
        EpollEvent { events, token }
    }

    fn ready(event: EpollEvent) -> (u32, u64) {
        let events = u32::from(event.events & EPOLLIN != 0)
            | (if event.events & EPOLLOUT != 0 { 2 } else { 0 })
            | (if event.events & EPOLLHUP != 0 { 4 } else { 0 })
            | (if event.events & EPOLLERR != 0 { 8 } else { 0 });
        (events, event.token)
    }
}

unsafe extern "C" {
    fn eventfd(initial: u32, flags: i32) -> i32;
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(poll: i32, operation: i32, source: i32, event: *mut EpollEvent) -> i32;
    fn epoll_wait(poll: i32, events: *mut EpollEvent, capacity: i32, timeout: i32) -> i32;
    fn timerfd_create(clock: i32, flags: i32) -> i32;
    fn timerfd_settime(descriptor: i32, flags: i32, setting: *const TimerSpec, previous: *mut TimerSpec) -> i32;
}

#[cfg(test)]
mod tests {
    use super::EpollEvent;

    #[test]
    fn epoll_event_layout() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(core::mem::size_of::<EpollEvent>(), 12);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(core::mem::size_of::<EpollEvent>(), 16);
        assert_eq!(core::mem::align_of::<EpollEvent>(), {
            #[cfg(target_arch = "x86_64")]
            {
                1
            }
            #[cfg(target_arch = "aarch64")]
            {
                8
            }
        });
    }
}
