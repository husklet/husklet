use super::super::macos_plan::{DarwinPlan, KqueueInterest};
use super::{DarwinHost, last_error};
use crate::native_host::{EventSyscalls, HostError, TimerSetting};

impl EventSyscalls for DarwinHost {
    fn event_create(&self, _: u64, _: bool) -> Result<i32, HostError> {
        Err(HostError::Unsupported)
    }

    fn event_read(&self, _: i32) -> Result<u64, HostError> {
        Err(HostError::Unsupported)
    }

    fn event_write(&self, _: i32, _: u64) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn timer_create(&self) -> Result<i32, HostError> {
        Err(HostError::Unsupported)
    }

    fn timer_set(&self, _: i32, _: TimerSetting) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn timer_read(&self, _: i32) -> Result<u64, HostError> {
        Err(HostError::Unsupported)
    }

    fn poll_create(&self) -> Result<i32, HostError> {
        // SAFETY: kqueue takes no pointers and success returns an owned descriptor.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(last_error());
        }
        // SAFETY: fcntl receives scalar values for the newly owned descriptor.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            let error = last_error();
            // SAFETY: descriptor has not escaped and is rolled back exactly once.
            let _ = unsafe { libc::close(descriptor) };
            return Err(error);
        }
        Ok(descriptor)
    }

    fn poll_control(&self, poll: i32, source: i32, operation: u8, interests: u32, token: u64) -> Result<(), HostError> {
        if operation == 3 {
            return KqueueCall::remove(poll, source);
        }
        if operation != 1 && operation != 2 {
            return Err(HostError::Invalid);
        }
        let decoded = KqueueInterest::decode(interests)?;
        let mut changes = Vec::with_capacity(2);
        let common = libc::EV_ADD
            | libc::EV_ENABLE
            | if decoded.edge { libc::EV_CLEAR } else { 0 }
            | if decoded.oneshot { libc::EV_ONESHOT } else { 0 };
        if decoded.read {
            changes.push(KqueueCall::change(source, libc::EVFILT_READ, common, token));
        }
        if decoded.write {
            changes.push(KqueueCall::change(source, libc::EVFILT_WRITE, common, token));
        }
        KqueueCall::submit(poll, &changes)
    }

    fn poll_wait(&self, poll: i32, timeout_ms: i32, events: &mut [(u32, u64)]) -> Result<usize, HostError> {
        let timeout = KqueueCall::timeout(timeout_ms)?;
        let timeout_pointer = timeout
            .as_ref()
            .map_or(std::ptr::null(), |value| value as *const libc::timespec);
        // SAFETY: events are written first into an initialized vector of exact
        // capacity. The optional timeout lives through the synchronous call.
        let mut native = vec![KqueueCall::empty_event(); events.len()];
        let count = unsafe {
            libc::kevent(
                poll,
                std::ptr::null(),
                0,
                native.as_mut_ptr(),
                i32::try_from(native.len()).map_err(|_| HostError::Invalid)?,
                timeout_pointer,
            )
        };
        if count < 0 {
            return Err(last_error());
        }
        let count = usize::try_from(count).map_err(|_| HostError::Failed)?;
        for (output, event) in events.iter_mut().zip(native).take(count) {
            let mut readiness = if event.filter == libc::EVFILT_READ { 1 } else { 2 };
            if event.flags & libc::EV_EOF != 0 {
                readiness |= 4;
            }
            if event.flags & libc::EV_ERROR != 0 {
                readiness |= 8;
            }
            *output = (readiness, event.udata as usize as u64);
        }
        Ok(count)
    }
}

struct KqueueCall;

impl KqueueCall {
    fn change(source: i32, filter: i16, flags: u16, token: u64) -> libc::kevent {
        libc::kevent {
            ident: source as libc::uintptr_t,
            filter,
            flags,
            fflags: 0,
            data: 0,
            udata: token as usize as *mut libc::c_void,
        }
    }

    fn empty_event() -> libc::kevent {
        Self::change(0, 0, 0, 0)
    }

    fn submit(poll: i32, changes: &[libc::kevent]) -> Result<(), HostError> {
        // SAFETY: changes is initialized and immutably live for the synchronous call.
        let result = unsafe {
            libc::kevent(
                poll,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        (result == 0).then_some(()).ok_or_else(last_error)
    }

    fn remove(poll: i32, source: i32) -> Result<(), HostError> {
        let changes = [
            Self::change(source, libc::EVFILT_READ, libc::EV_DELETE, 0),
            Self::change(source, libc::EVFILT_WRITE, libc::EV_DELETE, 0),
        ];
        let mut receipts = [Self::empty_event(), Self::empty_event()];
        let receipt_changes = changes.map(|mut value| {
            value.flags |= libc::EV_RECEIPT;
            value
        });
        // SAFETY: both arrays are valid for two events and retained nowhere.
        let count = unsafe {
            libc::kevent(
                poll,
                receipt_changes.as_ptr(),
                2,
                receipts.as_mut_ptr(),
                2,
                std::ptr::null(),
            )
        };
        if count != 2 {
            return Err(last_error());
        }
        receipts
            .iter()
            .any(|receipt| receipt.data == 0)
            .then_some(())
            .ok_or(HostError::NotFound)
    }

    fn timeout(milliseconds: i32) -> Result<Option<libc::timespec>, HostError> {
        Ok(
            DarwinPlan::timeout_parts(milliseconds)?.map(|(seconds, nanos)| libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: nanos as libc::c_long,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::KqueueCall;

    #[test]
    fn timeout_conversion_preserves() {
        assert!(KqueueCall::timeout(-1).unwrap().is_none());
        let value = KqueueCall::timeout(1_250).unwrap().unwrap();
        assert_eq!(value.tv_sec, 1);
        assert_eq!(value.tv_nsec, 250_000_000);
    }
}
