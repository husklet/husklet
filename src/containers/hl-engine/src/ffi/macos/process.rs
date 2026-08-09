use super::{DarwinHost, last_error};
use crate::native_host::{
    ChildExit, FileAction, HostError, ProcessGroup, ProcessId, ProcessSignal, ProcessSyscalls, SpawnRequest,
};
use std::ptr;

impl ProcessSyscalls for DarwinHost {
    fn spawn(&self, request: &SpawnRequest) -> Result<(ProcessId, u64), HostError> {
        request.validate()?;
        let actions = SpawnActions::new(request)?;
        let mut arguments = ProcessCall::pointers(&request.arguments, Some(&request.program));
        let mut environment = ProcessCall::pointers(&request.environment, None);
        let mut pid = 0;
        // SAFETY: CString storage and both terminated pointer vectors live through
        // this synchronous call. The initialized actions/attributes are owned by
        // `actions`, libc retains no Rust pointer, and posix_spawn performs no
        // Rust work in its child-side critical section.
        let result = unsafe {
            // SAFETY: the complete pointer/lifetime proof is immediately above.
            libc::posix_spawnp(
                &mut pid,
                request.program.as_ptr(),
                actions.file_actions(),
                actions.attributes(),
                arguments.as_mut_ptr(),
                environment.as_mut_ptr(),
            )
        };
        if result != 0 {
            return Err(ProcessCall::errno(result));
        }
        let process = ProcessId::new(u32::try_from(pid).map_err(|_| HostError::Failed)?)?;
        match ProcessCall::process_token(pid) {
            Ok(token) => Ok((process, token as u64)),
            Err(error) => {
                // SAFETY: rollback targets only the child just created; waitpid's
                // status is uniquely writable and neither call retains pointers.
                unsafe {
                    let _ = libc::kill(pid, libc::SIGKILL);
                    let mut status = 0;
                    let _ = libc::waitpid(pid, &mut status, 0);
                }
                Err(error)
            }
        }
    }

    fn close_process(&self, token: u64) {
        if let Ok(descriptor) = i32::try_from(token) {
            // SAFETY: ProcessHandle surrenders its kqueue token exactly once.
            let _ = unsafe { libc::close(descriptor) };
        }
    }

    fn wait(&self, process: ProcessId) -> Result<Option<ChildExit>, HostError> {
        ProcessCall::wait(process, libc::WNOHANG)
    }

    fn wait_blocking(&self, process: ProcessId) -> Result<ChildExit, HostError> {
        loop {
            match ProcessCall::wait(process, 0) {
                Ok(Some(exit)) => return Ok(exit),
                Ok(None) => continue,
                Err(HostError::Interrupted) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn signal(&self, process: ProcessId, signal: ProcessSignal) -> Result<(), HostError> {
        send_signal(process, signal, false)
    }

    fn signal_group(&self, group: ProcessId, signal: ProcessSignal) -> Result<(), HostError> {
        send_signal(group, signal, true)
    }
}

struct ProcessCall;

impl ProcessCall {
    fn pointers(values: &[std::ffi::CString], leading: Option<&std::ffi::CString>) -> Vec<*mut libc::c_char> {
        let mut output = Vec::with_capacity(values.len() + usize::from(leading.is_some()) + 1);
        if let Some(value) = leading {
            output.push(value.as_ptr().cast_mut());
        }
        output.extend(values.iter().map(|value| value.as_ptr().cast_mut()));
        output.push(ptr::null_mut());
        output
    }

    fn process_token(pid: libc::pid_t) -> Result<i32, HostError> {
        // SAFETY: kqueue takes no pointers and returns an owned descriptor.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(last_error());
        }
        let change = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: ptr::null_mut(),
        };
        // SAFETY: change is initialized and live through this synchronous call.
        let result = unsafe { libc::kevent(descriptor, &change, 1, ptr::null_mut(), 0, ptr::null()) };
        // SAFETY: fcntl receives scalar values for the owned descriptor.
        if result != 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            let error = last_error();
            // SAFETY: descriptor has not escaped and is rolled back once.
            let _ = unsafe { libc::close(descriptor) };
            Err(error)
        } else {
            Ok(descriptor)
        }
    }

    fn wait(process: ProcessId, options: i32) -> Result<Option<ChildExit>, HostError> {
        let pid = i32::try_from(process.get()).map_err(|_| HostError::Invalid)?;
        let mut status = 0;
        // SAFETY: status is uniquely writable and no pointer is retained.
        let result = unsafe { libc::waitpid(pid, &mut status, options) };
        if result < 0 {
            return Err(last_error());
        }
        if result == 0 {
            return Ok(None);
        }
        if libc::WIFEXITED(status) {
            Ok(Some(ChildExit::Code(libc::WEXITSTATUS(status) as u8)))
        } else if libc::WIFSIGNALED(status) {
            Ok(Some(ChildExit::Signal(libc::WTERMSIG(status) as u8)))
        } else {
            Ok(None)
        }
    }

    fn errno(value: i32) -> HostError {
        match value {
            libc::EINTR => HostError::Interrupted,
            libc::EAGAIN => HostError::WouldBlock,
            libc::EINVAL => HostError::Invalid,
            libc::EACCES | libc::EPERM => HostError::Denied,
            libc::ENOENT => HostError::NotFound,
            libc::EEXIST => HostError::Exists,
            libc::ENOMEM => HostError::Exhausted,
            libc::ENOTSUP => HostError::Unsupported,
            _ => HostError::Failed,
        }
    }
}

/// Linux and BSD disagree on the numbering of signals 1..=31; every higher number,
/// including the real-time range, is unnumbered on macOS and passes through.
const fn linux_to_host(signal: i32) -> i32 {
    const TABLE: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 10, 8, 9, 30, 11, 31, 13, 14, 15, 16, 20, 19, 17, 18, 21, 22, 16, 24, 25, 26, 27, 28, 23,
        30, 12,
    ];
    if signal >= 1 && signal <= 31 {
        TABLE[signal as usize] as i32
    } else {
        signal
    }
}

fn send_signal(process: ProcessId, signal: ProcessSignal, group: bool) -> Result<(), HostError> {
    let mut pid = i32::try_from(process.get()).map_err(|_| HostError::Invalid)?;
    if group {
        pid = pid.checked_neg().ok_or(HostError::Invalid)?;
    }
    let signal = linux_to_host(signal.linux());
    // SAFETY: scalar arguments only.
    (unsafe { libc::kill(pid, signal) } == 0)
        .then_some(())
        .ok_or_else(last_error)
}

struct SpawnActions {
    file: libc::posix_spawn_file_actions_t,
    attributes: libc::posix_spawnattr_t,
    file_active: bool,
    attributes_active: bool,
}

impl SpawnActions {
    fn new(request: &SpawnRequest) -> Result<Self, HostError> {
        // SAFETY: zero provides storage subsequently initialized by libc before use.
        let mut state: Self = unsafe { std::mem::zeroed() };
        // SAFETY: file is uniquely writable and becomes initialized on success.
        let result = unsafe { libc::posix_spawn_file_actions_init(&mut state.file) };
        if result != 0 {
            return Err(ProcessCall::errno(result));
        }
        state.file_active = true;
        for action in &request.file_actions {
            // SAFETY: file remains initialized and uniquely borrowed; scalar
            // descriptor values were validated, and no pointer is retained.
            let result = unsafe {
                match action {
                    FileAction::Duplicate { source, target } => {
                        libc::posix_spawn_file_actions_adddup2(&mut state.file, source.raw(), target.raw())
                    }
                    FileAction::Close(descriptor) => {
                        libc::posix_spawn_file_actions_addclose(&mut state.file, descriptor.raw())
                    }
                    FileAction::Inherit(descriptor) => {
                        libc::posix_spawn_file_actions_adddup2(&mut state.file, descriptor.raw(), descriptor.raw())
                    }
                }
            };
            if result != 0 {
                return Err(ProcessCall::errno(result));
            }
        }
        if request.process_group != ProcessGroup::Inherit {
            state.configure_group(request.process_group)?;
        }
        Ok(state)
    }

    fn configure_group(&mut self, group: ProcessGroup) -> Result<(), HostError> {
        // SAFETY: attributes is uniquely writable and initialized on success.
        let result = unsafe { libc::posix_spawnattr_init(&mut self.attributes) };
        if result != 0 {
            return Err(ProcessCall::errno(result));
        }
        self.attributes_active = true;
        let group = match group {
            ProcessGroup::Inherit => return Err(HostError::Invalid),
            ProcessGroup::New => 0,
            ProcessGroup::Join(group) => i32::try_from(group.get()).map_err(|_| HostError::Invalid)?,
        };
        // SAFETY: attributes is initialized and uniquely borrowed.
        let result = unsafe { libc::posix_spawnattr_setflags(&mut self.attributes, libc::POSIX_SPAWN_SETPGROUP) };
        if result != 0 {
            return Err(ProcessCall::errno(result));
        }
        // SAFETY: attributes is initialized and uniquely borrowed.
        let result = unsafe { libc::posix_spawnattr_setpgroup(&mut self.attributes, group) };
        (result == 0).then_some(()).ok_or_else(|| ProcessCall::errno(result))
    }

    fn file_actions(&self) -> *const libc::posix_spawn_file_actions_t {
        &self.file
    }

    fn attributes(&self) -> *const libc::posix_spawnattr_t {
        if self.attributes_active {
            &self.attributes
        } else {
            ptr::null()
        }
    }
}

impl Drop for SpawnActions {
    fn drop(&mut self) {
        if self.file_active {
            // SAFETY: file_active records successful initialization.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(&mut self.file) };
        }
        if self.attributes_active {
            // SAFETY: attributes_active records successful initialization.
            let _ = unsafe { libc::posix_spawnattr_destroy(&mut self.attributes) };
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn darwin_kevent_process() {
        assert_eq!(std::mem::size_of::<libc::uintptr_t>(), std::mem::size_of::<usize>());
        assert_eq!(std::mem::size_of::<libc::kevent>(), 32);
        assert_eq!(std::mem::align_of::<libc::kevent>(), 4);
    }
}
