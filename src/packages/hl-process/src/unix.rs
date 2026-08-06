// The process control primitives this module wraps are all `unsafe` libc entry points.
#![allow(unsafe_code)]

use super::{Capture, Command as OwnedCommand, Outcome};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Read;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);
const TERM_GRACE: Duration = Duration::from_millis(500);
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn run(
    command: &OwnedCommand,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    let spawned = spawn(command)?;
    let mut owned = OwnedChild::new(spawned.process);
    let stdout = Drain::spawn(spawned.stdout, capture.stdout_limit)?;
    let stderr = Drain::spawn(spawned.stderr, capture.stderr_limit)?;
    supervise(&mut owned, stdout, stderr, capture, timeout, cancelled)
}

fn spawn(command: &OwnedCommand) -> std::io::Result<Spawned> {
    if command.environment().is_some() {
        spawn_exact(command)
    } else {
        spawn_standard(command)
    }
}

fn spawn_standard(command: &OwnedCommand) -> std::io::Result<Spawned> {
    let _spawn = spawn_lock()?;
    let mut command = command.standard();
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr was not piped"))?;
    Ok(Spawned {
        process: Process::Standard(child),
        stdout: OwnedFd::from(stdout).into(),
        stderr: OwnedFd::from(stderr).into(),
    })
}

fn supervise(
    owned: &mut OwnedChild,
    stdout: Drain,
    stderr: Drain,
    capture: &Capture,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> std::io::Result<Outcome> {
    let started = Instant::now();
    let outcome = loop {
        if stdout.exceeded() || stderr.exceeded() {
            owned.terminate()?;
            break Outcome::OutputLimit;
        }
        if cancelled.load(Ordering::Acquire) {
            owned.terminate()?;
            break Outcome::Cancelled;
        }
        if started.elapsed() >= timeout {
            owned.terminate()?;
            break Outcome::TimedOut;
        }
        match owned.try_wait() {
            Ok(Some(status)) => {
                owned.quiesce()?;
                break status
                    .signal()
                    .map_or_else(|| Outcome::Exited(status.code()), Outcome::Signaled);
            }
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                let cleanup = owned.terminate();
                return cleanup.and(Err(error));
            }
        }
    };
    let stdout = stdout.finish()?;
    let stderr = stderr.finish()?;
    let exceeded = stdout.exceeded || stderr.exceeded;
    fs::write(&capture.stdout, stdout.bytes)?;
    fs::write(&capture.stderr, stderr.bytes)?;
    Ok(if exceeded { Outcome::OutputLimit } else { outcome })
}

struct Spawned {
    process: Process,
    stdout: File,
    stderr: File,
}

fn spawn_exact(command: &OwnedCommand) -> std::io::Result<Spawned> {
    let _spawn = spawn_lock()?;
    let stdout = pipe()?;
    let stderr = pipe()?;
    let null = CString::new("/dev/null").expect("static path has no NUL");
    let program = resolve(command.program())?;
    let program = cstring(program.as_os_str().as_bytes(), "program")?;
    let mut arguments = Vec::with_capacity(command.arguments().count() + 1);
    arguments.push(program.clone());
    for argument in command.arguments() {
        arguments.push(cstring(argument.as_bytes(), "argument")?);
    }
    let mut argument_pointers = arguments
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();
    let environment = command
        .environment()
        .expect("exact spawn requires an exact environment")
        .iter()
        .map(|entry| cstring(entry.record(), "environment record"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut environment_pointers = environment
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();

    let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    // SAFETY: `actions` is aligned uninitialized storage exclusively owned by
    // this function. Successful initialization establishes the C object's
    // lifetime; no Rust references alias it and the call cannot unwind.
    check_spawn(unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) })?;
    // SAFETY: initialization above succeeded, so the uniquely owned C object is
    // valid until the matching destroy below. Moving the value does not expose
    // internal storage to Rust and no concurrent access exists.
    let mut actions = unsafe { actions.assume_init() };
    let configured = (|| {
        // SAFETY: `actions` is initialized and uniquely owned, and the NUL-terminated path outlives the call.
        check_spawn(unsafe {
            libc::posix_spawn_file_actions_addopen(&raw mut actions, libc::STDIN_FILENO, null.as_ptr(), libc::O_RDONLY, 0)
        })?;
        for (source, target) in [
            (stdout.1.as_raw_fd(), libc::STDOUT_FILENO),
            (stderr.1.as_raw_fd(), libc::STDERR_FILENO),
        ] {
            // SAFETY: `actions` is initialized and uniquely owned; source is a
            // live pipe descriptor and target is a standard descriptor. The C
            // object copies integers only, retains no Rust pointer, and cannot unwind.
            check_spawn(unsafe { libc::posix_spawn_file_actions_adddup2(&raw mut actions, source, target) })?;
        }
        for descriptor in [
            stdout.0.as_raw_fd(),
            stdout.1.as_raw_fd(),
            stderr.0.as_raw_fd(),
            stderr.1.as_raw_fd(),
        ] {
            // SAFETY: the initialized action list is uniquely owned and stores
            // only the descriptor value. Pipe owners remain live through spawn;
            // no concurrent mutation or unwind crosses this call.
            check_spawn(unsafe { libc::posix_spawn_file_actions_addclose(&raw mut actions, descriptor) })?;
        }
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = configured {
        // SAFETY: the initialized list is still uniquely owned and no spawn is
        // active. Destroy retains no pointer and cannot unwind.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut actions) };
        return Err(error);
    }

    let mut attributes = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
    // SAFETY: aligned storage is exclusively owned and becomes initialized only
    // on success. The call retains no Rust pointer and cannot unwind.
    let attribute_status = unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) };
    if attribute_status != 0 {
        // SAFETY: actions remains initialized and uniquely owned.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut actions) };
        return Err(std::io::Error::from_raw_os_error(attribute_status));
    }
    // SAFETY: successful initialization established the C object's lifetime.
    let mut attributes = unsafe { attributes.assume_init() };
    let configured = (|| {
        // SAFETY: initialized attributes are uniquely owned; both setters copy
        // scalar values and retain no pointers. No concurrent access exists.
        check_spawn(unsafe { libc::posix_spawnattr_setflags(&raw mut attributes, libc::POSIX_SPAWN_SETPGROUP as i16) })?;
        // SAFETY: as above; group zero requests a new group led by the child.
        check_spawn(unsafe { libc::posix_spawnattr_setpgroup(&raw mut attributes, 0) })
    })();
    let mut pid = 0;
    let spawned = configured.and_then(|()| {
        // All C strings and pointer arrays are NUL-terminated and remain alive for the call.
        // Actions/attributes are initialized and unaliased; env order and duplicate pointers
        // are deliberately preserved.
        // SAFETY: the child receives independent kernel state and the FFI cannot unwind.
        check_spawn(unsafe {
            libc::posix_spawn(
                &raw mut pid,
                program.as_ptr(),
                &raw const actions,
                &raw const attributes,
                argument_pointers.as_mut_ptr(),
                environment_pointers.as_mut_ptr(),
            )
        })
    });
    // SAFETY: both initialized C objects are uniquely owned, no spawn call is
    // active, destruction retains no pointer, and neither call can unwind.
    unsafe {
        libc::posix_spawnattr_destroy(&raw mut attributes);
        libc::posix_spawn_file_actions_destroy(&raw mut actions);
    }
    spawned?;
    drop(stdout.1);
    drop(stderr.1);
    Ok(Spawned {
        process: Process::Exact(pid),
        stdout: stdout.0.into(),
        stderr: stderr.0.into(),
    })
}

fn spawn_lock() -> std::io::Result<MutexGuard<'static, ()>> {
    SPAWN_LOCK
        .lock()
        .map_err(|_| std::io::Error::other("host process spawn lock was poisoned"))
}

fn resolve(program: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
    if program.as_bytes().contains(&b'/') {
        return Ok(PathBuf::from(program));
    }
    let path = std::env::var_os("PATH")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "host PATH is unset"))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        let bytes = candidate.as_os_str().as_bytes();
        let Ok(candidate_c) = CString::new(bytes) else {
            continue;
        };
        // SAFETY: candidate_c is a live NUL-terminated path. access observes
        // filesystem metadata, retains no pointer or Rust alias, and cannot unwind.
        if unsafe { libc::access(candidate_c.as_ptr(), libc::X_OK) } == 0 {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("host executable {program:?} was not found in PATH"),
    ))
}

fn cstring(bytes: &[u8], kind: &str) -> std::io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("process {kind} contains NUL")))
}

fn check_spawn(status: i32) -> std::io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status))
    }
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: descriptors is aligned writable storage for exactly two integers.
    // The kernel writes no aliases and retains no pointer; the call cannot unwind.
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    // SAFETY: as above. `SPAWN_LOCK` is held across creation, CLOEXEC mutation,
    // and spawn, preventing another hl-process launch from observing the gap.
    let result = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for descriptor in descriptors {
        // SAFETY: descriptor is valid and live; F_SETFD copies the integer flag,
        // retains no pointer, the package spawn lock excludes sibling launches,
        // and the call cannot unwind.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            // SAFETY: both raw descriptors are uniquely owned on this error path.
            unsafe {
                libc::close(descriptors[0]);
                libc::close(descriptors[1]);
            }
            return Err(std::io::Error::last_os_error());
        }
    }
    let read = match relocate(descriptors[0]) {
        Ok(read) => read,
        Err(error) => {
            // SAFETY: the second descriptor remains uniquely owned because the
            // first relocation failed before wrapping it.
            unsafe { libc::close(descriptors[1]) };
            return Err(error);
        }
    };
    let write = match relocate(descriptors[1]) {
        Ok(write) => write,
        Err(error) => {
            drop(read);
            return Err(error);
        }
    };
    Ok((read, write))
}

fn relocate(descriptor: i32) -> std::io::Result<OwnedFd> {
    let descriptor = if descriptor <= libc::STDERR_FILENO {
        // SAFETY: descriptor is a valid uniquely owned pipe end. F_DUPFD_CLOEXEC
        // creates a second independently owned descriptor above stderr, retains
        // no pointer, and cannot unwind.
        let relocated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, libc::STDERR_FILENO + 1) };
        if relocated < 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: relocation failed, so the original descriptor remains
            // uniquely owned and must be closed on this error path.
            unsafe { libc::close(descriptor) };
            return Err(error);
        }
        // SAFETY: the original descriptor is uniquely owned and replaced by the
        // relocated descriptor; no alias or concurrent package spawn exists.
        unsafe { libc::close(descriptor) };
        relocated
    } else {
        descriptor
    };
    // SAFETY: this function has unique ownership of the live descriptor and
    // transfers it exactly once into OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

struct Drained {
    bytes: Vec<u8>,
    exceeded: bool,
}

struct Drain {
    count: Arc<AtomicU64>,
    limit: u64,
    stopping: Arc<AtomicBool>,
    thread: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drain {
    fn spawn(mut source: File, limit: u64) -> std::io::Result<Self> {
        nonblocking(&source)?;
        let count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&count);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = thread::spawn(move || {
            let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
            let mut retained = Vec::with_capacity(capacity);
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let size = match source.read(&mut buffer) {
                    Ok(size) => size,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(POLL);
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if size == 0 {
                    break;
                }
                observed.fetch_add(size as u64, Ordering::Release);
                let available = usize::try_from(limit.saturating_sub(retained.len() as u64)).unwrap_or(usize::MAX);
                retained.extend_from_slice(&buffer[..size.min(available)]);
            }
            Ok(retained)
        });
        Ok(Self {
            count,
            limit,
            stopping,
            thread,
        })
    }

    fn exceeded(&self) -> bool {
        self.count.load(Ordering::Acquire) > self.limit
    }

    fn finish(self) -> std::io::Result<Drained> {
        self.stopping.store(true, Ordering::Release);
        let bytes = self
            .thread
            .join()
            .map_err(|_| std::io::Error::other("subprocess capture thread panicked"))??;
        let exceeded = self.count.load(Ordering::Acquire) > self.limit;
        Ok(Drained { bytes, exceeded })
    }
}

fn nonblocking(source: &File) -> std::io::Result<()> {
    let descriptor = source.as_raw_fd();
    // SAFETY: `descriptor` is a live pipe descriptor owned by `source`.
    // F_GETFL reads integer descriptor flags, retains no pointer, and cannot
    // unwind or affect the descriptor lifetime.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the same uniquely owned descriptor remains live. F_SETFL copies
    // the integer flags, retains no pointer, and concurrent code only reads the
    // pipe through the drain thread created after this call.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct OwnedChild {
    process: Process,
    group: u32,
    reaped: bool,
}

impl OwnedChild {
    fn new(process: Process) -> Self {
        let group = process.id();
        Self {
            process,
            group,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.process.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + TERM_GRACE;
        while !self.reaped && Instant::now() < deadline {
            match self.try_wait() {
                Ok(None) => thread::sleep(POLL),
                Ok(Some(_)) | Err(_) => break,
            }
        }
        self.signal(libc::SIGKILL)?;
        let _ = self.process.kill();
        if !self.reaped {
            self.process.wait()?;
            self.reaped = true;
        }
        self.quiesce()
    }

    fn quiesce(&self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + TERM_GRACE;
        while self.group_exists() {
            if Instant::now() >= deadline {
                self.signal(libc::SIGKILL)?;
                let kill_deadline = Instant::now() + TERM_GRACE;
                while self.group_exists() {
                    if Instant::now() >= kill_deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "host subprocess group did not quiesce",
                        ));
                    }
                    thread::sleep(POLL);
                }
                return Ok(());
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    fn signal(&self, signal: i32) -> std::io::Result<bool> {
        let group =
            i32::try_from(self.group).map_err(|_| std::io::Error::other("subprocess group exceeded host pid range"))?;
        // SAFETY: a negative, validated process-group ID and integer signal do
        // not reference Rust memory. The kernel owns process identity, and the
        // call cannot unwind or retain an alias.
        let result = unsafe { libc::kill(-group, signal) };
        if result == 0 {
            Ok(true)
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    fn group_exists(&self) -> bool {
        self.signal(0).unwrap_or(true)
    }
}

enum Process {
    Standard(Child),
    Exact(libc::pid_t),
}

impl Process {
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::Exact(pid) => u32::try_from(*pid).unwrap_or(u32::MAX),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Standard(child) => child.try_wait(),
            Self::Exact(pid) => wait_pid(*pid, libc::WNOHANG),
        }
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Standard(child) => child.wait(),
            Self::Exact(pid) => loop {
                if let Some(status) = wait_pid(*pid, 0)? {
                    break Ok(status);
                }
            },
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            Self::Exact(pid) => {
                // SAFETY: pid is the positive identity returned by posix_spawn.
                // The call touches no Rust memory, retains nothing, and cannot unwind.
                if unsafe { libc::kill(*pid, libc::SIGKILL) } == 0 {
                    Ok(())
                } else {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
            }
        }
    }
}

fn wait_pid(pid: libc::pid_t, flags: i32) -> std::io::Result<Option<std::process::ExitStatus>> {
    loop {
        let mut status = 0;
        // SAFETY: status is aligned writable integer storage, pid is a child
        // returned by posix_spawn, and waitpid retains no pointer or alias.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, flags) };
        if waited == pid {
            return Ok(Some(std::process::ExitStatus::from_raw(status)));
        }
        if waited == 0 {
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped || self.group_exists() {
            let _ = self.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvironmentEntry;
    use std::cell::Cell;
    use std::sync::Arc;

    fn capture(directory: &tempfile::TempDir) -> Capture {
        Capture {
            stdout: directory.path().join("stdout"),
            stderr: directory.path().join("stderr"),
            stdout_limit: 1024,
            stderr_limit: 1024,
        }
    }

    #[test]
    fn completes_and_captures() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "printf owned"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
        assert_eq!(fs::read(directory.path().join("stdout")).unwrap(), b"owned");
    }

    #[test]
    fn exact_environment_preserves_order_duplicates_and_non_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let environment = [
            EnvironmentEntry::new(b"HL_PROCESS_ENV_CHILD", b"1").unwrap(),
            EnvironmentEntry::new(b"FIRST", b"one").unwrap(),
            EnvironmentEntry::new(b"DUP", b"first").unwrap(),
            EnvironmentEntry::new(b"RAW", b"\xff").unwrap(),
            EnvironmentEntry::new(b"DUP", b"second").unwrap(),
            EnvironmentEntry::new(b"EMPTY", b"").unwrap(),
        ];
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::exact_environment_child", "--ignored"]);
        command.exact_environment(environment).unwrap();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
    }

    #[test]
    fn exact_spawn_survives_closed_standard_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::closed_standard_descriptors_child", "--ignored"]);
        command
            .exact_environment([EnvironmentEntry::new(b"HL_PROCESS_CLOSED_CHILD", b"1").unwrap()])
            .unwrap();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_secs(3),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
    }

    #[test]
    #[ignore = "executed with standard descriptors closed"]
    fn closed_standard_descriptors_child() {
        let directory = tempfile::tempdir().unwrap();
        let environment = [
            EnvironmentEntry::new(b"HL_PROCESS_ENV_CHILD", b"1").unwrap(),
            EnvironmentEntry::new(b"FIRST", b"one").unwrap(),
            EnvironmentEntry::new(b"DUP", b"first").unwrap(),
            EnvironmentEntry::new(b"RAW", b"\xff").unwrap(),
            EnvironmentEntry::new(b"DUP", b"second").unwrap(),
            EnvironmentEntry::new(b"EMPTY", b"").unwrap(),
        ];
        // This isolated subprocess deliberately relinquishes its three standard descriptors
        // before exercising descriptor relocation.
        // SAFETY: it performs no Rust I/O afterward, terminates with `_exit`, owns no shared
        // process state, and no unwind crosses FFI.
        unsafe {
            libc::close(libc::STDIN_FILENO);
            libc::close(libc::STDOUT_FILENO);
            libc::close(libc::STDERR_FILENO);
        }
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::exact_environment_child", "--ignored"]);
        let success = command.exact_environment(environment).is_ok()
            && matches!(
                run(
                    &command,
                    &capture(&directory),
                    Duration::from_secs(2),
                    &AtomicBool::new(false),
                ),
                Ok(Outcome::Exited(Some(0)))
            );
        // SAFETY: this is the terminal action of the isolated child test. It
        // runs no destructors, retains no pointer, and cannot unwind.
        unsafe { libc::_exit(if success { 0 } else { 101 }) }
    }

    #[test]
    #[ignore = "executed as the controlled exact-environment subprocess"]
    fn exact_environment_child() {
        assert_eq!(
            raw_environment(),
            [
                &b"HL_PROCESS_ENV_CHILD=1"[..],
                &b"FIRST=one"[..],
                &b"DUP=first"[..],
                &b"RAW=\xff"[..],
                &b"DUP=second"[..],
                &b"EMPTY="[..],
            ]
        );
    }

    fn raw_environment() -> Vec<Vec<u8>> {
        let mut records = Vec::new();
        // SAFETY: the process owns a conventional NUL-terminated `environ`
        // pointer table for its entire lifetime. This test only observes each
        // immutable C string before any thread mutates the environment; no
        // pointer escapes and CStr validation cannot unwind across FFI.
        let mut cursor = unsafe { environment_pointer() };
        // SAFETY: the table is terminated by a null pointer and each non-null
        // entry addresses one NUL-terminated environment record owned by libc.
        while !unsafe { *cursor }.is_null() {
            // SAFETY: established above; the record is observed and copied
            // immediately, with no mutation or retained alias.
            records.push(unsafe { std::ffi::CStr::from_ptr(*cursor) }.to_bytes().to_vec());
            // SAFETY: the current entry is non-null, so advancing by one stays
            // within the table or reaches its required null terminator.
            cursor = unsafe { cursor.add(1) };
        }
        records
    }

    #[cfg(target_os = "macos")]
    unsafe fn environment_pointer() -> *mut *mut libc::c_char {
        unsafe extern "C" {
            fn _NSGetEnviron() -> *mut *mut *mut libc::c_char;
        }
        // SAFETY: Darwin returns the address of its process-lifetime environ
        // pointer. The caller observes the pointed-to table without mutation.
        unsafe { *_NSGetEnviron() }
    }

    #[cfg(not(target_os = "macos"))]
    unsafe fn environment_pointer() -> *mut *mut libc::c_char {
        unsafe extern "C" {
            static mut environ: *mut *mut libc::c_char;
        }
        // SAFETY: POSIX libc owns this process-lifetime pointer; the caller only
        // observes its table before any environment mutation.
        unsafe { environ }
    }

    #[test]
    fn timeout_kills_group_without_pipe_wait() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & echo $!; wait"]),
            &capture(&directory),
            Duration::from_millis(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert_reported_process_gone(&directory.path().join("stdout"));
    }

    #[test]
    fn timeout_does_not_wait_for_detached_capture_writer() {
        let directory = tempfile::tempdir().unwrap();
        let detached = DetachedChild::new(directory.path().join("detached.pid"));
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact", "unix::tests::detached_capture_writer_child", "--ignored"]);
        command
            .exact_environment([EnvironmentEntry::new(
                b"HL_PROCESS_DETACHED_PID_FILE",
                detached.path.as_os_str().as_bytes(),
            )
            .unwrap()])
            .unwrap();
        let started = Instant::now();
        let outcome = run(
            &command,
            &capture(&directory),
            Duration::from_millis(250),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert!(
            fs::read(directory.path().join("stdout"))
                .unwrap()
                .ends_with(b"detached\n")
        );
        detached.terminate().unwrap();
    }

    #[test]
    #[ignore = "executed as a subprocess retaining a detached capture writer"]
    fn detached_capture_writer_child() {
        let path = std::env::var_os("HL_PROCESS_DETACHED_PID_FILE").unwrap();
        let report = File::create(path).unwrap();
        // The parent remains in its original process group until supervision terminates it; the
        // child creates a new session and deliberately keeps inherited stdout/stderr open long
        // enough to expose an unbounded drain.
        // SAFETY: the fork child invokes only async-signal-safe libc operations, owns no Rust
        // references after the fork, and terminates with `_exit`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // SAFETY: this fork child is not a process-group leader, so setsid
            // creates a detached session without touching Rust storage.
            if unsafe { libc::setsid() } < 0 {
                // SAFETY: this is the terminal action of the isolated child.
                unsafe { libc::_exit(101) };
            }
            let marker = b"detached\n";
            let mut digits = [0_u8; 32];
            // SAFETY: `getpid` takes no argument, touches no Rust storage, and cannot fail.
            let mut value = unsafe { libc::getpid() } as u32;
            let mut cursor = digits.len();
            while value != 0 {
                cursor -= 1;
                digits[cursor] = b'0' + (value % 10) as u8;
                value /= 10;
            }
            // Stdout remains an inherited live descriptor, and the report descriptor was opened
            // before the fork.
            // SAFETY: marker and digits are immutable storage valid for their complete writes,
            // and write, close, sleep and `_exit` retain no Rust pointers and cannot unwind.
            unsafe {
                libc::write(libc::STDOUT_FILENO, marker.as_ptr().cast(), marker.len());
                libc::write(
                    report.as_raw_fd(),
                    digits[cursor..].as_ptr().cast(),
                    digits.len() - cursor,
                );
                libc::close(report.as_raw_fd());
                libc::sleep(2);
                libc::_exit(0);
            }
        }
        std::thread::sleep(Duration::from_secs(60));
    }

    struct DetachedChild {
        path: PathBuf,
        armed: Cell<bool>,
    }

    impl DetachedChild {
        fn new(path: PathBuf) -> Self {
            Self {
                path,
                armed: Cell::new(true),
            }
        }

        fn pid(&self) -> std::io::Result<i32> {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Ok(text) = fs::read_to_string(&self.path)
                    && let Ok(pid) = text.parse()
                {
                    return Ok(pid);
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "detached fixture did not publish its pid",
                    ));
                }
                thread::sleep(POLL);
            }
        }

        fn terminate(&self) -> std::io::Result<()> {
            if !self.armed.get() {
                return Ok(());
            }
            let pid = self.pid()?;
            // SAFETY: the positive PID came from the dedicated fixture file;
            // SIGKILL carries no Rust pointer and cannot unwind.
            if unsafe { libc::kill(pid, libc::SIGKILL) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                // SAFETY: signal zero only observes the exact fixture PID and
                // retains no pointer or Rust alias.
                if unsafe { libc::kill(pid, 0) } < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        self.armed.set(false);
                        return Ok(());
                    }
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "detached fixture was not reaped",
                    ));
                }
                thread::sleep(POLL);
            }
        }
    }

    impl Drop for DetachedChild {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }

    #[test]
    fn cancellation_kills_group() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let setting = Arc::clone(&cancelled);
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            setting.store(true, Ordering::Release);
        });
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & wait"]),
            &capture(&directory),
            Duration::from_secs(2),
            &cancelled,
        )
        .unwrap();
        setter.join().unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
    }

    #[test]
    fn output_is_bounded_without_blocking_on_a_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let mut capture = capture(&directory);
        capture.stdout_limit = 10;
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "while :; do printf 1234567890; done"]),
            &capture,
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::OutputLimit);
        assert_eq!(fs::metadata(directory.path().join("stdout")).unwrap().len(), 10);
    }

    #[test]
    fn signal_identity_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "kill -ABRT $$"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Signaled(libc::SIGABRT));
    }

    #[test]
    fn natural_leader_exit_quiesces_lingering_group() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run(
            OwnedCommand::new("sh").args(["-c", "sleep 60 & echo $!; exit 0"]),
            &capture(&directory),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Exited(Some(0)));
        assert_reported_process_gone(&directory.path().join("stdout"));
    }

    fn assert_reported_process_gone(path: &std::path::Path) {
        let process = fs::read_to_string(path).unwrap().trim().parse::<i32>().unwrap();
        // SAFETY: signal zero only queries the numeric PID and retains no Rust
        // storage. ESRCH proves the recorded descendant identity is gone.
        let result = unsafe { libc::kill(process, 0) };
        assert_eq!(result, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }
}
