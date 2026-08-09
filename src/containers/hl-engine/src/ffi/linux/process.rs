use super::{ErrnoMapper, LinuxHost, abi};
use crate::native_host::{
    ChildChannel, ChildExit, FileAction, ForkWireSyscalls, HostError, PeerCredentials, ProcessGroup, ProcessId,
    ProcessSignal, ProcessSyscalls, SpawnRequest,
};
use std::ptr;
use std::sync::Arc;

impl LinuxHost {
    pub fn channel_pair(self: &Arc<Self>) -> Result<(ChildChannel<Self>, ChildChannel<Self>), HostError> {
        let mut descriptors = [-1; 2];
        // SAFETY: descriptors is aligned and uniquely writable for two integers;
        // socketpair retains no pointer and cannot unwind. CLOEXEC/NONBLOCK are
        // applied atomically, so neither endpoint is transiently inheritable.
        if unsafe {
            abi::socketpair(
                abi::AF_UNIX,
                abi::SOCK_STREAM | abi::SOCK_CLOEXEC | abi::SOCK_NONBLOCK,
                0,
                descriptors.as_mut_ptr(),
            )
        } != 0
        {
            return Err(ErrnoMapper::current());
        }
        if let Err(error) = super::transfer::ControlTransaction::enable_credentials(descriptors[0])
            .and_then(|()| super::transfer::ControlTransaction::enable_credentials(descriptors[1]))
        {
            // SAFETY: both endpoints are owned locally until channels are built.
            unsafe {
                let _ = abi::close(descriptors[0]);
                let _ = abi::close(descriptors[1]);
            }
            return Err(error);
        }
        Ok((
            ChildChannel::from_host_handle(Arc::clone(self), descriptors[0] as u64),
            ChildChannel::from_host_handle(Arc::clone(self), descriptors[1] as u64),
        ))
    }
}

impl ForkWireSyscalls for LinuxHost {
    fn close_channel(&self, channel: u64) {
        if let Ok(descriptor) = i32::try_from(channel) {
            // SAFETY: ChildChannel surrenders this endpoint exactly once; libc
            // retains no storage and cannot unwind.
            let _ = unsafe { abi::close(descriptor) };
        }
    }

    fn send(&self, channel: u64, bytes: &[u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(channel).map_err(|_| HostError::Invalid)?;
        // SAFETY: bytes is valid and immutable for its exact length; the channel
        // remains owned, libc retains nothing, and cannot unwind.
        let result = unsafe { abi::send(descriptor, bytes.as_ptr().cast(), bytes.len(), 0) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn receive(&self, channel: u64, bytes: &mut [u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(channel).map_err(|_| HostError::Invalid)?;
        // SAFETY: bytes is uniquely writable for its exact length; the channel
        // remains owned, libc retains nothing, and cannot unwind.
        let result = unsafe { abi::recv(descriptor, bytes.as_mut_ptr().cast(), bytes.len(), 0) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn send_attachments(&self, channel: u64, bytes: &[u8], descriptors: &[i32]) -> Result<usize, HostError> {
        super::transfer::send(
            i32::try_from(channel).map_err(|_| HostError::Invalid)?,
            bytes,
            descriptors,
        )
    }

    fn receive_attachments(
        &self,
        channel: u64,
        bytes: &mut [u8],
        capacity: usize,
    ) -> Result<(usize, Vec<i32>, Option<PeerCredentials>), HostError> {
        super::transfer::receive(i32::try_from(channel).map_err(|_| HostError::Invalid)?, bytes, capacity)
    }
}

impl ProcessSyscalls for LinuxHost {
    fn spawn(&self, request: &SpawnRequest) -> Result<(ProcessId, u64), HostError> {
        request.validate()?;
        let actions = SpawnActions::new(request)?;
        let mut arguments: Vec<*mut core::ffi::c_char> = Vec::with_capacity(request.arguments.len() + 2);
        arguments.push(request.program.as_ptr().cast_mut());
        arguments.extend(request.arguments.iter().map(|argument| argument.as_ptr().cast_mut()));
        arguments.push(ptr::null_mut());
        let mut environment: Vec<*mut core::ffi::c_char> = request
            .environment
            .iter()
            .map(|entry| entry.as_ptr().cast_mut())
            .collect();
        environment.push(ptr::null_mut());
        let mut pid = 0;
        // SAFETY: every pointer references a live CString until posix_spawnp
        // returns; both vectors are NUL-terminated, no actions/attributes cross
        // the boundary, libc retains nothing, and cannot unwind. posix_spawnp
        // performs its child-side work without executing Rust after fork.
        let result = unsafe {
            // SAFETY: the complete pointer and lifetime proof is immediately above.
            abi::posix_spawnp(
                &raw mut pid,
                request.program.as_ptr(),
                actions.file_actions(),
                actions.attributes(),
                arguments.as_ptr(),
                environment.as_ptr(),
            )
        };
        if result != 0 {
            return Err(SpawnFailure::translate(result));
        }
        let process = ProcessId::new(u32::try_from(pid).map_err(|_| HostError::Failed)?)?;
        // SAFETY: pid is the positive child returned above; pidfd_open takes no
        // pointers, retains no Rust storage, and cannot unwind.
        let pidfd = unsafe { abi::syscall(abi::SYS_pidfd_open, pid, 0_u32) };
        if pidfd < 0 {
            // SAFETY: rollback targets only the just-created child. waitpid
            // receives a live status pointer, retains nothing, and cannot unwind.
            unsafe {
                let _ = abi::kill(pid, abi::SIGKILL);
                let mut status = 0;
                let _ = abi::waitpid(pid, &raw mut status, 0);
            }
            return Err(ErrnoMapper::current());
        }
        Ok((process, pidfd as u64))
    }

    fn close_process(&self, token: u64) {
        if let Ok(descriptor) = i32::try_from(token) {
            // SAFETY: ProcessHandle surrenders its pidfd once; libc retains
            // nothing and cannot unwind.
            let _ = unsafe { abi::close(descriptor) };
        }
    }

    fn wait(&self, process: ProcessId) -> Result<Option<ChildExit>, HostError> {
        let pid = i32::try_from(process.get()).map_err(|_| HostError::Invalid)?;
        let mut status = 0;
        // SAFETY: status is uniquely writable; pid is validated, libc retains
        // nothing, and cannot unwind.
        let result = unsafe { abi::waitpid(pid, &raw mut status, abi::WNOHANG) };
        if result < 0 {
            return Err(ErrnoMapper::current());
        }
        if result == 0 {
            return Ok(None);
        }
        let signal = status & 0x7f;
        if signal == 0 {
            return Ok(Some(ChildExit::Code(((status >> 8) & 0xff) as u8)));
        }
        if signal == 0x7f {
            return Ok(None);
        }
        Ok(Some(ChildExit::Signal(signal as u8)))
    }

    fn wait_blocking(&self, process: ProcessId) -> Result<ChildExit, HostError> {
        let pid = i32::try_from(process.get()).map_err(|_| HostError::Invalid)?;
        let mut status = 0;
        loop {
            // SAFETY: status is uniquely writable; pid is validated, libc
            // retains nothing, and cannot unwind.
            let result = unsafe { abi::waitpid(pid, &raw mut status, 0) };
            if result < 0 && ErrnoMapper::current() == HostError::Interrupted {
                continue;
            }
            if result < 0 {
                return Err(ErrnoMapper::current());
            }
            let signal = status & 0x7f;
            return if signal == 0 {
                Ok(ChildExit::Code(((status >> 8) & 0xff) as u8))
            } else {
                Ok(ChildExit::Signal(signal as u8))
            };
        }
    }

    fn signal(&self, process: ProcessId, signal: ProcessSignal) -> Result<(), HostError> {
        send_signal(process, signal, false)
    }

    fn signal_group(&self, group: ProcessId, signal: ProcessSignal) -> Result<(), HostError> {
        send_signal(group, signal, true)
    }
}

struct SpawnActions {
    file: [u64; 64],
    attributes: [u64; 64],
    file_initialized: bool,
    attributes_initialized: bool,
}

impl SpawnActions {
    fn new(request: &SpawnRequest) -> Result<Self, HostError> {
        let mut state = Self {
            file: [0; 64],
            attributes: [0; 64],
            file_initialized: false,
            attributes_initialized: false,
        };
        // SAFETY: both buffers are aligned, uniquely owned, and larger than the
        // glibc opaque objects. They remain alive through spawn and are destroyed
        // by Drop; libc retains no pointers and cannot unwind.
        let result = unsafe { abi::posix_spawn_file_actions_init(state.file.as_mut_ptr().cast()) };
        if result != 0 {
            return Err(SpawnFailure::translate(result));
        }
        state.file_initialized = true;
        for action in &request.file_actions {
            let result = unsafe {
                // SAFETY: the initialized opaque object remains uniquely owned;
                // descriptors were validated and libc retains nothing.
                match action {
                    FileAction::Duplicate { source, target } => abi::posix_spawn_file_actions_adddup2(
                        state.file.as_mut_ptr().cast(),
                        source.raw(),
                        target.raw(),
                    ),
                    FileAction::Close(descriptor) => {
                        abi::posix_spawn_file_actions_addclose(state.file.as_mut_ptr().cast(), descriptor.raw())
                    }
                    FileAction::Inherit(descriptor) => abi::posix_spawn_file_actions_adddup2(
                        state.file.as_mut_ptr().cast(),
                        descriptor.raw(),
                        descriptor.raw(),
                    ),
                }
            };
            if result != 0 {
                return Err(SpawnFailure::translate(result));
            }
        }
        if request.process_group != ProcessGroup::Inherit {
            let group = Self::process_group(request.process_group)?;
            // SAFETY: attributes buffer has sufficient alignment/capacity and is
            // uniquely owned until Drop; libc retains nothing and cannot unwind.
            let result = unsafe { abi::posix_spawnattr_init(state.attributes.as_mut_ptr().cast()) };
            if result != 0 {
                return Err(SpawnFailure::translate(result));
            }
            state.attributes_initialized = true;
            // SAFETY: the initialized attribute object is live and uniquely owned.
            let result = unsafe {
                abi::posix_spawnattr_setflags(state.attributes.as_mut_ptr().cast(), abi::POSIX_SPAWN_SETPGROUP)
            };
            if result != 0 {
                return Err(SpawnFailure::translate(result));
            }
            // SAFETY: the initialized attribute object is live; group is positive
            // and representable, libc retains nothing and cannot unwind.
            let result = unsafe { abi::posix_spawnattr_setpgroup(state.attributes.as_mut_ptr().cast(), group) };
            if result != 0 {
                return Err(SpawnFailure::translate(result));
            }
        }
        Ok(state)
    }

    fn process_group(group: ProcessGroup) -> Result<i32, HostError> {
        match group {
            ProcessGroup::New => Ok(0),
            ProcessGroup::Join(group) => i32::try_from(group.get()).map_err(|_| HostError::Invalid),
            ProcessGroup::Inherit => Err(HostError::Invalid),
        }
    }

    fn file_actions(&self) -> *const abi::c_void {
        self.file.as_ptr().cast()
    }

    fn attributes(&self) -> *const abi::c_void {
        if self.attributes_initialized {
            self.attributes.as_ptr().cast()
        } else {
            ptr::null()
        }
    }
}

impl Drop for SpawnActions {
    fn drop(&mut self) {
        if self.attributes_initialized {
            // SAFETY: this object was initialized once, is uniquely owned, and
            // will not be used again; libc cannot unwind.
            let _ = unsafe { abi::posix_spawnattr_destroy(self.attributes.as_mut_ptr().cast()) };
        }
        if self.file_initialized {
            // SAFETY: this object was initialized once, is uniquely owned, and
            // will not be used again; libc cannot unwind.
            let _ = unsafe { abi::posix_spawn_file_actions_destroy(self.file.as_mut_ptr().cast()) };
        }
    }
}

struct SpawnFailure;

impl SpawnFailure {
    fn translate(result: i32) -> HostError {
        match result {
            abi::EINTR => HostError::Interrupted,
            abi::EAGAIN => HostError::WouldBlock,
            abi::EINVAL | abi::EBADF => HostError::Invalid,
            abi::EACCES | abi::EPERM => HostError::Denied,
            abi::ENOENT => HostError::NotFound,
            abi::EEXIST => HostError::Exists,
            abi::EMFILE | abi::ENFILE | abi::ENOMEM => HostError::Exhausted,
            abi::ENOTSUP => HostError::Unsupported,
            _ => HostError::Failed,
        }
    }
}

fn send_signal(process: ProcessId, signal: ProcessSignal, group: bool) -> Result<(), HostError> {
    let mut pid = i32::try_from(process.get()).map_err(|_| HostError::Invalid)?;
    if group {
        pid = pid.checked_neg().ok_or(HostError::Invalid)?;
    }
    // A Linux host numbers signals exactly as the guest ABI does, so no translation applies.
    let native = signal.linux();
    // SAFETY: kill receives scalar values only, retains nothing, and cannot unwind.
    if unsafe { abi::kill(pid, native) } == 0 {
        Ok(())
    } else {
        Err(ErrnoMapper::current())
    }
}
