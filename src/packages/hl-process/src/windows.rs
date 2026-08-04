use super::{Capture, Command as OwnedCommand, Outcome};
use std::fs::{self, File};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

const POLL_MS: u32 = 10;

pub(super) fn run(
    command: &OwnedCommand,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    if command.environment().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "ordered byte-valued process environments are not implemented on Windows",
        ));
    }
    let stdin = File::open("NUL")?;
    let stdin_handle = stdin.as_raw_handle() as HANDLE;
    inherit(stdin_handle)?;
    let (stdout, stdout_handle) = pipe()?;
    let (stderr, stderr_handle) = pipe()?;
    let stdout = Drain::spawn(stdout, capture.stdout_limit);
    let stderr = Drain::spawn(stderr, capture.stderr_limit);
    let mut line = command_line(command)?;
    let directory = ptr::null();

    let attributes = Attributes::new([stdin_handle, stdout_handle.raw(), stderr_handle.raw()])?;
    // SAFETY: both Win32 records are plain C data for which an all-zero value
    // is the documented empty state. They contain no Rust references or
    // alignment-sensitive interior pointers. These stack owners remain live
    // for the call below, are not shared concurrently, and zeroing cannot
    // unwind across FFI.
    let (mut startup, mut process): (STARTUPINFOEXW, PROCESS_INFORMATION) =
        unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_handle;
    startup.StartupInfo.hStdOutput = stdout_handle.raw();
    startup.StartupInfo.hStdError = stderr_handle.raw();
    startup.lpAttributeList = attributes.pointer;
    let mut owned = OwnedProcess::new()?;
    // SAFETY: `line` and optional directory are live mutable NUL-terminated
    // UTF-16 buffers. Startup contains inheritable file handles owned above;
    // process receives initialized owned handles. The stack and handle owners
    // remain live, no concurrent Rust alias mutates them, and the call cannot
    // unwind over FFI.
    let created = unsafe {
        CreateProcessW(
            ptr::null(),
            line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            directory,
            &mut startup.StartupInfo,
            &mut process,
        )
    };
    let creation_error = (created == 0).then(std::io::Error::last_os_error);
    // SAFETY: `stdin_handle` is the live, uniquely owned `stdin` file handle.
    // Only its kernel inheritance bit is changed; `stdin` keeps it valid, no
    // concurrent Rust alias mutates it, and this call cannot unwind over FFI.
    let inheritance_cleared = unsafe { SetHandleInformation(stdin_handle, HANDLE_FLAG_INHERIT, 0) };
    if let Some(error) = creation_error {
        return Err(error);
    }
    // Ownership must transfer before the next fallible operation: the child is
    // suspended but already exists, so every later return must terminate it.
    owned.install(process);
    if inheritance_cleared == 0 {
        return Err(std::io::Error::last_os_error());
    }
    drop(attributes);
    drop(stdout_handle);
    drop(stderr_handle);
    // SAFETY: process is still suspended, its valid handles are exclusively
    // owned by `owned`, and the job is live until `owned` drops. No Rust memory
    // is shared with the kernel operation, which cannot unwind over FFI.
    // Assignment occurs before any guest instruction can spawn outside the job.
    if unsafe { AssignProcessToJobObject(owned.job, owned.process) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `thread` is the valid primary suspended-thread handle exclusively
    // owned by `owned`. Resuming changes kernel state, touches no aliased Rust
    // storage, retains no pointer, and cannot unwind over FFI.
    if unsafe { ResumeThread(owned.thread) } == u32::MAX {
        return Err(std::io::Error::last_os_error());
    }

    let started = Instant::now();
    loop {
        if stdout.exceeded() || stderr.exceeded() {
            owned.terminate();
            break finish(capture, stdout, stderr, Outcome::OutputLimit);
        }
        if cancelled.load(Ordering::Acquire) {
            owned.terminate();
            break finish(capture, stdout, stderr, Outcome::Cancelled);
        }
        if started.elapsed() >= timeout {
            owned.terminate();
            break finish(capture, stdout, stderr, Outcome::TimedOut);
        }
        // SAFETY: process remains a valid, exclusively owned handle until this
        // wait completes. Waiting does not access Rust storage concurrently,
        // retains no pointer, and cannot unwind over FFI.
        match unsafe { WaitForSingleObject(owned.process, POLL_MS) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: the signalled process handle is valid and owned;
                // `code` is aligned, writable, stack-local, and live for the
                // call. No concurrent alias exists and the call cannot unwind.
                if unsafe { GetExitCodeProcess(owned.process, &mut code) } == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The direct process is complete, but job descendants may
                // still own capture handles. Terminating the job establishes
                // EOF before the bounded drain threads are joined.
                owned.terminate();
                break finish(capture, stdout, stderr, Outcome::Exited(Some(code.cast_signed())));
            }
            WAIT_TIMEOUT => {}
            _ => return Err(std::io::Error::last_os_error()),
        }
    }
}

fn inherit(handle: HANDLE) -> std::io::Result<()> {
    // SAFETY: handle is valid and backed by a uniquely owned live File. The
    // operation changes only kernel metadata, retains no Rust pointer, has no
    // concurrent mutable alias, and cannot unwind over FFI.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pipe() -> std::io::Result<(File, OwnedHandle)> {
    let mut read = ptr::null_mut();
    let mut write = ptr::null_mut();
    let mut security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: the output pointers are aligned, writable, and stack-live, and
    // the initialized security descriptor is valid for the call. No aliases
    // access them concurrently; successful handles transfer immediately to
    // single Rust owners, and the call cannot unwind over FFI.
    if unsafe { CreatePipe(&mut read, &mut write, &mut security, 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `read` is a valid, uniquely owned pipe handle returned above.
    // The call changes kernel metadata, retains no pointer, races with no Rust
    // mutation, and cannot unwind over FFI.
    if unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: both valid handles are still uniquely owned by this failure
        // path. Closing consumes only kernel references, races with no Rust
        // access, retains no pointer, and cannot unwind over FFI.
        unsafe {
            CloseHandle(read);
            CloseHandle(write);
        }
        return Err(error);
    }
    // SAFETY: `read` is valid, correctly aligned as an opaque handle, and has
    // exactly one owner. `File` assumes that ownership for its whole lifetime;
    // no concurrent Rust alias exists and construction cannot unwind over FFI.
    let read = unsafe { File::from_raw_handle(read.cast()) };
    Ok((read, OwnedHandle(write)))
}

fn finish(capture: &Capture, stdout: Drain, stderr: Drain, outcome: Outcome) -> std::io::Result<Outcome> {
    let stdout = stdout.finish()?;
    let stderr = stderr.finish()?;
    let exceeded = stdout.exceeded || stderr.exceeded;
    fs::write(&capture.stdout, stdout.bytes)?;
    fs::write(&capture.stderr, stderr.bytes)?;
    Ok(if exceeded { Outcome::OutputLimit } else { outcome })
}

struct Drained {
    bytes: Vec<u8>,
    exceeded: bool,
}

struct Drain {
    count: Arc<AtomicU64>,
    limit: u64,
    thread: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drain {
    fn spawn(mut source: impl Read + Send + 'static, limit: u64) -> Self {
        let count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&count);
        let thread = thread::spawn(move || {
            let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
            let mut retained = Vec::with_capacity(capacity);
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let size = source.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                observed.fetch_add(size as u64, Ordering::Release);
                let available = usize::try_from(limit.saturating_sub(retained.len() as u64)).unwrap_or(usize::MAX);
                retained.extend_from_slice(&buffer[..size.min(available)]);
            }
            Ok(retained)
        });
        Self { count, limit, thread }
    }

    fn exceeded(&self) -> bool {
        self.count.load(Ordering::Acquire) > self.limit
    }

    fn finish(self) -> std::io::Result<Drained> {
        let exceeded = self.exceeded();
        let bytes = self
            .thread
            .join()
            .map_err(|_| std::io::Error::other("subprocess capture thread panicked"))??;
        Ok(Drained { bytes, exceeded })
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper is the sole owner of the valid handle. No
            // alias uses it concurrently after drop begins; closing retains no
            // Rust pointer and cannot unwind over FFI.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct Attributes {
    storage: Vec<usize>,
    handles: Box<[HANDLE; 3]>,
    pointer: windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl Attributes {
    fn new(handles: [HANDLE; 3]) -> std::io::Result<Self> {
        let handles = Box::new(handles);
        let mut size = 0;
        // SAFETY: a null list requests only its required size; `size` is aligned,
        // writable, stack-live, unaliased, and the call cannot unwind over FFI.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        if size == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let words = size.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        let pointer = storage.as_mut_ptr().cast();
        // SAFETY: `storage` owns at least the requested bytes at pointer-aligned
        // storage for this API, and both it and `size` remain live and unaliased
        // for the call. No concurrent access occurs and FFI cannot unwind.
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `pointer` names the initialized list owned by `storage`, and
        // `handles` is an aligned, live, immutable three-handle array. Both
        // owners outlive the attribute list, no concurrent mutation occurs,
        // and the FFI call cannot unwind.
        if unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of_val(handles.as_ref()),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            // SAFETY: `pointer` is the initialized, uniquely owned list. It is
            // no longer used after deletion; no concurrent access exists and
            // the call cannot unwind over FFI.
            unsafe { DeleteProcThreadAttributeList(pointer) };
            return Err(error);
        }
        Ok(Self {
            storage,
            handles,
            pointer,
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: `pointer` is the initialized list uniquely backed by live
        // `storage`. Drop has exclusive access, deletion retains no pointer,
        // and the call cannot unwind over FFI.
        unsafe { DeleteProcThreadAttributeList(self.pointer) };
        let _ = (self.storage.len(), self.handles.len());
    }
}

fn command_line(command: &OwnedCommand) -> std::io::Result<Vec<u16>> {
    let mut encoded = quote(command.program())?;
    for argument in command.arguments() {
        encoded.push(b' ' as u16);
        encoded.extend(quote(argument)?);
    }
    if encoded.len() >= 32_767 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows command line exceeds 32766 UTF-16 code units",
        ));
    }
    encoded.push(0);
    Ok(encoded)
}

fn quote(value: &std::ffi::OsStr) -> std::io::Result<Vec<u16>> {
    let value = wide(value);
    if value.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows command arguments cannot contain NUL",
        ));
    }
    let slash = b'\\' as u16;
    let quote = b'"' as u16;
    let mut result = vec![quote];
    let mut slashes = 0;
    for character in value {
        if character == slash {
            slashes += 1;
        } else {
            if character == quote {
                result.extend(std::iter::repeat_n(slash, slashes * 2 + 1));
            } else {
                result.extend(std::iter::repeat_n(slash, slashes));
            }
            slashes = 0;
            result.push(character);
        }
    }
    result.extend(std::iter::repeat_n(slash, slashes * 2));
    result.push(quote);
    Ok(result)
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

struct OwnedProcess {
    job: HANDLE,
    process: HANDLE,
    thread: HANDLE,
}

impl OwnedProcess {
    fn new() -> std::io::Result<Self> {
        // SAFETY: null security/name pointers request an unnamed job with
        // default security and require no referenced storage. The returned
        // handle gets one owner, no concurrent alias exists, and FFI cannot unwind.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: this Win32 information record is plain C data whose zero value
        // is its documented empty state. The aligned stack owner is live and
        // unaliased, no concurrent access occurs, and zeroing cannot unwind.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: job is valid and uniquely owned; `limits` is aligned, live,
        // and passed with its exact size. No concurrent mutation occurs, the
        // kernel retains no Rust pointer, and FFI cannot unwind.
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            // SAFETY: this failure path solely owns the valid job handle. No
            // concurrent alias exists; closing retains no pointer and cannot unwind.
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        Ok(Self {
            job,
            process: ptr::null_mut(),
            thread: ptr::null_mut(),
        })
    }

    fn install(&mut self, process: PROCESS_INFORMATION) {
        self.process = process.hProcess;
        self.thread = process.hThread;
    }

    fn terminate(&mut self) {
        if !self.job.is_null() {
            // SAFETY: the valid job handle is exclusively owned here. Neither
            // call accesses aliased Rust storage or retains a pointer, and FFI
            // cannot unwind. Closing activates KILL_ON_JOB_CLOSE before capture
            // drains can wait for EOF.
            unsafe { TerminateJobObject(self.job, 1) };
            unsafe { CloseHandle(self.job) };
            self.job = ptr::null_mut();
        }
        if !self.process.is_null() {
            // SAFETY: the process handle is exclusively owned here and remains
            // live through the following wait. Unconditionally terminating the
            // direct process is required when assignment failed while it was
            // suspended: terminating an empty job can itself succeed. The call
            // shares no Rust memory and cannot unwind over FFI.
            unsafe { TerminateProcess(self.process, 1) };
            // SAFETY: the same valid process handle remains exclusively owned
            // through the wait. No Rust storage is aliased or retained and the
            // wait cannot unwind over FFI.
            unsafe { WaitForSingleObject(self.process, 5_000) };
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.terminate();
        // SAFETY: each non-null handle was transferred exactly once from the
        // corresponding Win32 creator and is closed exactly once here. Drop has
        // exclusive access, the calls retain no pointers or aliases and cannot
        // unwind over FFI. Closing the job is the final kill-on-close backstop.
        unsafe {
            if !self.thread.is_null() {
                CloseHandle(self.thread);
            }
            if !self.process.is_null() {
                CloseHandle(self.process);
            }
            if !self.job.is_null() {
                CloseHandle(self.job);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::quote;
    use std::ffi::OsStr;

    #[test]
    fn quotes_spaces_quotes_and_trailing_slashes() {
        let text = |value| String::from_utf16(&quote(OsStr::new(value)).unwrap()).unwrap();
        assert_eq!(text("a b"), "\"a b\"");
        assert_eq!(text("a\\"), "\"a\\\\\"");
        assert_eq!(text("a\"b"), "\"a\\\"b\"");
    }
}
